//! Variable-length datagrams that travel alongside the fixed-size
//! legacy [`ProtoEvent`]s on the same DTLS connection.
//!
//! The first byte of every datagram is an [`EventType`]. Legacy event
//! types keep their fixed-size wire format and are decoded into
//! [`Datagram::Event`]. `Audio` and `AudioControl` are variable-size
//! and are decoded without copying the payload — the caller receives
//! a [`Range`](std::ops::Range) into its own receive buffer and hands
//! the payload to the jitter buffer, which performs the single copy
//! into its own slot.

use std::ops::Range;

use crate::{EventType, MAX_EVENT_SIZE, ProtoEvent, ProtocolError};

/// conservative upper bound for a single datagram, MTU minus
/// DTLS/UDP/IP overhead — audio frames must stay below this
pub const MAX_DATAGRAM_SIZE: usize = 1200;

/// size of the audio header: type:u8, seq:u32, ts_ms:u32, len:u16
const AUDIO_HEADER_SIZE: usize = 1 + 4 + 4 + 2;

/// size of an audio control datagram: type:u8, cmd:u8, sample_rate:u32, channels:u8
const AUDIO_CONTROL_SIZE: usize = 1 + 1 + 4 + 1;

/// audio stream control, sent by the sending (emulation) side before
/// the first [`Datagram::Audio`] frame and when the stream stops
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioControlCmd {
    /// stop the stream; receiver should destroy its audio state
    Stop,
    /// start a stream with the given format
    Start { sample_rate: u32, channels: u8 },
}

/// a decoded datagram. Borrows nothing: for [`Datagram::Audio`] the
/// payload is referenced by range into the caller's buffer.
#[derive(Clone, Debug)]
pub enum Datagram {
    /// legacy fixed-size control/input event
    Event(ProtoEvent),
    /// Opus audio frame
    Audio {
        /// wrapping sequence number; detects loss and reordering
        seq: u32,
        /// sender timestamp in milliseconds, for latency estimation
        ts_ms: u32,
        /// range of the Opus payload within the decoded buffer
        payload_range: Range<usize>,
    },
    /// audio stream control
    AudioControl(AudioControlCmd),
}

/// encoding counterpart of [`Datagram`]: audio payloads are passed
/// by slice so encoding never allocates.
#[derive(Clone, Copy, Debug)]
pub enum DatagramRef<'a> {
    /// legacy fixed-size control/input event
    Event(ProtoEvent),
    /// Opus audio frame
    Audio {
        /// wrapping sequence number
        seq: u32,
        /// sender timestamp in milliseconds
        ts_ms: u32,
        /// Opus payload; must fit [`MAX_DATAGRAM_SIZE`]
        payload: &'a [u8],
    },
    /// audio stream control
    AudioControl(AudioControlCmd),
}

/// Decode a datagram, dispatching on the first byte. Unknown event
/// types yield [`ProtocolError::InvalidEventId`], which read loops
/// treat as "skip" — this keeps the protocol forward-compatible with
/// peers running newer versions.
pub fn decode(buf: &[u8]) -> Result<Datagram, ProtocolError> {
    let (&event_type, _) = buf.split_first().ok_or(ProtocolError::Truncated(0))?;
    match EventType::try_from(event_type)? {
        EventType::Audio => {
            if buf.len() < AUDIO_HEADER_SIZE {
                return Err(ProtocolError::Truncated(buf.len()));
            }
            let seq = u32::from_be_bytes(buf[1..5].try_into().expect("slice len"));
            let ts_ms = u32::from_be_bytes(buf[5..9].try_into().expect("slice len"));
            let len = u16::from_be_bytes(buf[9..11].try_into().expect("slice len"));
            if len == 0 {
                return Err(ProtocolError::InvalidAudioLength(len));
            }
            let payload_range = AUDIO_HEADER_SIZE..AUDIO_HEADER_SIZE + len as usize;
            if buf.len() < payload_range.end {
                return Err(ProtocolError::Truncated(buf.len()));
            }
            Ok(Datagram::Audio {
                seq,
                ts_ms,
                payload_range,
            })
        }
        EventType::AudioControl => {
            if buf.len() < AUDIO_CONTROL_SIZE {
                return Err(ProtocolError::Truncated(buf.len()));
            }
            let sample_rate = u32::from_be_bytes(buf[2..6].try_into().expect("slice len"));
            let channels = buf[6];
            let cmd = match buf[1] {
                0 => AudioControlCmd::Stop,
                1 => AudioControlCmd::Start {
                    sample_rate,
                    channels,
                },
                cmd => return Err(ProtocolError::InvalidAudioControlCmd(cmd)),
            };
            Ok(Datagram::AudioControl(cmd))
        }
        _ => {
            // legacy events are variable-length on the wire (e.g. `Ping`
            // is 1 byte) — zero-pad into the fixed decode buffer rather
            // than requiring MAX_EVENT_SIZE bytes. Safe: every variant's
            // decoder consumes exactly its own field width from a buffer
            // that is always MAX_EVENT_SIZE long, so missing trailing
            // bytes just read as zero instead of panicking or rejecting
            // a validly-short datagram.
            let mut fixed = [0u8; MAX_EVENT_SIZE];
            let n = buf.len().min(MAX_EVENT_SIZE);
            fixed[..n].copy_from_slice(&buf[..n]);
            Ok(Datagram::Event(fixed.try_into()?))
        }
    }
}

/// Serialize `dg` into a caller-provided buffer — no allocation on
/// the hot path (audio runs at ~50 datagrams/s per peer). Returns the
/// number of bytes written.
pub fn encode_into(dg: DatagramRef, out: &mut [u8]) -> Result<usize, ProtocolError> {
    match dg {
        DatagramRef::Event(event) => {
            let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
            if out.len() < len {
                return Err(ProtocolError::BufferTooSmall {
                    needed: len,
                    have: out.len(),
                });
            }
            out[..len].copy_from_slice(&buf[..len]);
            Ok(len)
        }
        DatagramRef::Audio {
            seq,
            ts_ms,
            payload,
        } => {
            let needed = AUDIO_HEADER_SIZE + payload.len();
            if payload.is_empty() {
                return Err(ProtocolError::InvalidAudioLength(0));
            }
            if payload.len() > u16::MAX as usize {
                return Err(ProtocolError::InvalidAudioLength(u16::MAX));
            }
            if out.len() < needed {
                return Err(ProtocolError::BufferTooSmall {
                    needed,
                    have: out.len(),
                });
            }
            out[0] = EventType::Audio as u8;
            out[1..5].copy_from_slice(&seq.to_be_bytes());
            out[5..9].copy_from_slice(&ts_ms.to_be_bytes());
            out[9..11].copy_from_slice(&(payload.len() as u16).to_be_bytes());
            out[AUDIO_HEADER_SIZE..needed].copy_from_slice(payload);
            Ok(needed)
        }
        DatagramRef::AudioControl(cmd) => {
            if out.len() < AUDIO_CONTROL_SIZE {
                return Err(ProtocolError::BufferTooSmall {
                    needed: AUDIO_CONTROL_SIZE,
                    have: out.len(),
                });
            }
            out[0] = EventType::AudioControl as u8;
            match cmd {
                AudioControlCmd::Stop => {
                    out[1] = 0;
                    out[2..6].copy_from_slice(&0u32.to_be_bytes());
                    out[6] = 0;
                }
                AudioControlCmd::Start {
                    sample_rate,
                    channels,
                } => {
                    out[1] = 1;
                    out[2..6].copy_from_slice(&sample_rate.to_be_bytes());
                    out[6] = channels;
                }
            }
            Ok(AUDIO_CONTROL_SIZE)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::AudioControlCmd;

    fn encode(dg: DatagramRef) -> Vec<u8> {
        let mut buf = [0u8; MAX_DATAGRAM_SIZE];
        let len = encode_into(dg, &mut buf).expect("encode");
        buf[..len].to_vec()
    }

    #[test]
    fn audio_roundtrip() {
        for size in [1usize, 240, MAX_DATAGRAM_SIZE - AUDIO_HEADER_SIZE] {
            let payload = vec![0xAB; size];
            let buf = encode(DatagramRef::Audio {
                seq: 42,
                ts_ms: 1337,
                payload: &payload,
            });
            let decoded = match decode(&buf).expect("decode") {
                Datagram::Audio {
                    seq,
                    ts_ms,
                    payload_range,
                } => {
                    assert_eq!(seq, 42);
                    assert_eq!(ts_ms, 1337);
                    buf[payload_range].to_vec()
                }
                other => panic!("expected audio, got {other:?}"),
            };
            assert_eq!(decoded, payload);
        }
    }

    #[test]
    fn audio_control_roundtrip() {
        for cmd in [
            AudioControlCmd::Stop,
            AudioControlCmd::Start {
                sample_rate: 48000,
                channels: 2,
            },
        ] {
            let buf = encode(DatagramRef::AudioControl(cmd));
            assert_eq!(buf.len(), AUDIO_CONTROL_SIZE);
            match decode(&buf).expect("decode") {
                Datagram::AudioControl(decoded) => assert_eq!(decoded, cmd),
                other => panic!("expected audio control, got {other:?}"),
            }
        }
    }

    #[test]
    fn truncated_audio_header() {
        let buf = encode(DatagramRef::Audio {
            seq: 1,
            ts_ms: 1,
            payload: &[1, 2, 3],
        });
        for cut in 0..AUDIO_HEADER_SIZE {
            assert!(matches!(
                decode(&buf[..cut]),
                Err(ProtocolError::Truncated(_))
            ));
        }
    }

    #[test]
    fn truncated_audio_payload() {
        let buf = encode(DatagramRef::Audio {
            seq: 1,
            ts_ms: 1,
            payload: &[1, 2, 3],
        });
        assert!(matches!(
            decode(&buf[..buf.len() - 1]),
            Err(ProtocolError::Truncated(_))
        ));
    }

    #[test]
    fn zero_length_audio_is_rejected() {
        let mut buf = [0u8; AUDIO_HEADER_SIZE];
        buf[0] = EventType::Audio as u8;
        buf[9..11].copy_from_slice(&0u16.to_be_bytes());
        assert!(matches!(
            decode(&buf),
            Err(ProtocolError::InvalidAudioLength(0))
        ));
    }

    #[test]
    fn unknown_event_type_is_an_error() {
        // read loops treat this as "skip datagram" — the mechanism
        // that keeps the protocol forward-compatible
        let buf = [200u8; MAX_DATAGRAM_SIZE];
        assert!(matches!(
            decode(&buf),
            Err(ProtocolError::InvalidEventId(_))
        ));
    }

    #[test]
    fn invalid_audio_control_command() {
        let mut buf = [0u8; AUDIO_CONTROL_SIZE];
        buf[0] = EventType::AudioControl as u8;
        buf[1] = 99;
        assert!(matches!(
            decode(&buf),
            Err(ProtocolError::InvalidAudioControlCmd(99))
        ));
    }

    #[test]
    fn legacy_events_decode_identically() {
        // regression guard: the wire format of legacy events must not
        // change when routed through decode()/encode_into()
        let events = [
            ProtoEvent::Ping,
            ProtoEvent::Pong(true),
            ProtoEvent::Leave(0xDEADBEEF),
            ProtoEvent::Ack(42),
            ProtoEvent::Enter(crate::Position::Top),
            ProtoEvent::Hello {
                commit: *b"abcdefgh",
            },
        ];
        for event in events {
            let (legacy_buf, legacy_len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
            let mut buf = [0u8; MAX_DATAGRAM_SIZE];
            let len = encode_into(DatagramRef::Event(event), &mut buf).expect("encode");
            assert_eq!(&buf[..len], &legacy_buf[..legacy_len]);
            match decode(&buf[..len]).expect("decode") {
                Datagram::Event(decoded) => {
                    assert_eq!(format!("{decoded}"), format!("{event}"));
                }
                other => panic!("expected event, got {other:?}"),
            }
        }
    }

    #[test]
    fn legacy_short_buffer_zero_pads_instead_of_erroring() {
        // a real `Ping` datagram is 1 byte on the wire (UDP preserves
        // message boundaries, so decode() sees exactly that). Missing
        // trailing fields must zero-pad, not reject the packet.
        let buf = [EventType::Ping as u8; 1];
        assert!(matches!(
            decode(&buf),
            Ok(Datagram::Event(ProtoEvent::Ping))
        ));
    }

    #[test]
    fn encode_into_small_buffer() {
        let mut buf = [0u8; 4];
        assert!(matches!(
            encode_into(
                DatagramRef::Audio {
                    seq: 1,
                    ts_ms: 1,
                    payload: &[0; 32]
                },
                &mut buf
            ),
            Err(ProtocolError::BufferTooSmall {
                needed: 43,
                have: 4
            })
        ));
    }
}
