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

/// type:u8, frame_count:u8; each frame is seq:u32, ts_ms:u32, len:u16, payload
const AUDIO_BATCH_HEADER_SIZE: usize = 2;
const AUDIO_BATCH_FRAME_HEADER_SIZE: usize = 4 + 4 + 2;

/// Maximum UTF-8 clipboard text size, in bytes.
pub const MAX_CLIPBOARD_TEXT_SIZE: usize = 64 * 1024;
/// Text fragments stay within the existing conservative DTLS datagram budget.
pub const MAX_CLIPBOARD_FRAGMENT_SIZE: usize = MAX_DATAGRAM_SIZE - CLIPBOARD_HEADER_SIZE;
const CLIPBOARD_HEADER_SIZE: usize = 1 + 4 + 2 + 2;

/// Maximum UTF-8 computer name size, in bytes (excluding the event id).
pub const MAX_COMPUTER_NAME_SIZE: usize = 255;

/// Validate display metadata without accepting control characters.
pub fn valid_computer_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_COMPUTER_NAME_SIZE && !name.chars().any(char::is_control)
}

#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub seq: u32,
    pub ts_ms: u32,
    pub payload_range: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct AudioFrameRef<'a> {
    pub seq: u32,
    pub ts_ms: u32,
    pub payload: &'a [u8],
}

#[derive(Clone, Debug)]
pub struct ClipboardTextFragment {
    pub transfer_id: u32,
    pub index: u16,
    pub count: u16,
    pub payload_range: Range<usize>,
}

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
    /// Validated UTF-8 computer display name, not a DNS endpoint.
    ComputerName(String),
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
    /// Multiple consecutive Opus frames. Batching amortizes the relatively
    /// expensive DTLS send without changing the codec's 20 ms frame size.
    AudioBatch(Vec<AudioFrame>),
    /// UTF-8 text clipboard fragment. The complete transfer must be
    /// reassembled and validated as UTF-8 by the receiver.
    ClipboardText(ClipboardTextFragment),
    /// receipt acknowledgement for a completed clipboard transfer
    ClipboardAck(u32),
    /// audio stream control
    AudioControl(AudioControlCmd),
}

/// encoding counterpart of [`Datagram`]: audio payloads are passed
/// by slice so encoding never allocates.
#[derive(Clone, Copy, Debug)]
pub enum DatagramRef<'a> {
    /// UTF-8 computer name; wire format is type:u8 followed by 1–255 bytes.
    ComputerName(&'a str),
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
    AudioBatch(&'a [AudioFrameRef<'a>]),
    ClipboardText {
        transfer_id: u32,
        index: u16,
        count: u16,
        payload: &'a [u8],
    },
    ClipboardAck(u32),
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
        EventType::ComputerName => {
            let payload = &buf[1..];
            // Bound before decoding or allocating, even for oversized packets.
            if payload.is_empty() || payload.len() > MAX_COMPUTER_NAME_SIZE {
                return Err(ProtocolError::InvalidComputerName);
            }
            let name =
                std::str::from_utf8(payload).map_err(|_| ProtocolError::InvalidComputerName)?;
            if !valid_computer_name(name) {
                return Err(ProtocolError::InvalidComputerName);
            }
            Ok(Datagram::ComputerName(name.to_owned()))
        }
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
        EventType::AudioBatch => {
            if buf.len() < AUDIO_BATCH_HEADER_SIZE {
                return Err(ProtocolError::Truncated(buf.len()));
            }
            let count = buf[1];
            if count == 0 {
                return Err(ProtocolError::InvalidAudioBatchCount(count));
            }
            let mut cursor = AUDIO_BATCH_HEADER_SIZE;
            let mut frames = Vec::with_capacity(count as usize);
            for _ in 0..count {
                if buf.len() < cursor + AUDIO_BATCH_FRAME_HEADER_SIZE {
                    return Err(ProtocolError::Truncated(buf.len()));
                }
                let seq =
                    u32::from_be_bytes(buf[cursor..cursor + 4].try_into().expect("slice len"));
                let ts_ms =
                    u32::from_be_bytes(buf[cursor + 4..cursor + 8].try_into().expect("slice len"));
                let len =
                    u16::from_be_bytes(buf[cursor + 8..cursor + 10].try_into().expect("slice len"));
                if len == 0 {
                    return Err(ProtocolError::InvalidAudioLength(len));
                }
                cursor += AUDIO_BATCH_FRAME_HEADER_SIZE;
                let payload_range = cursor..cursor + len as usize;
                if buf.len() < payload_range.end {
                    return Err(ProtocolError::Truncated(buf.len()));
                }
                cursor = payload_range.end;
                frames.push(AudioFrame {
                    seq,
                    ts_ms,
                    payload_range,
                });
            }
            Ok(Datagram::AudioBatch(frames))
        }
        EventType::ClipboardText => {
            if buf.len() < CLIPBOARD_HEADER_SIZE {
                return Err(ProtocolError::Truncated(buf.len()));
            }
            let transfer_id = u32::from_be_bytes(buf[1..5].try_into().expect("slice len"));
            let index = u16::from_be_bytes(buf[5..7].try_into().expect("slice len"));
            let count = u16::from_be_bytes(buf[7..9].try_into().expect("slice len"));
            let max_count = MAX_CLIPBOARD_TEXT_SIZE.div_ceil(MAX_CLIPBOARD_FRAGMENT_SIZE) as u16;
            if count == 0 || index >= count || count > max_count {
                return Err(ProtocolError::InvalidClipboardFragment { index, count });
            }
            let payload_range = CLIPBOARD_HEADER_SIZE..buf.len();
            if payload_range.len() > MAX_CLIPBOARD_FRAGMENT_SIZE {
                return Err(ProtocolError::ClipboardFragmentTooLarge(
                    payload_range.len(),
                ));
            }
            Ok(Datagram::ClipboardText(ClipboardTextFragment {
                transfer_id,
                index,
                count,
                payload_range,
            }))
        }
        EventType::ClipboardAck => {
            if buf.len() < 1 + 4 {
                return Err(ProtocolError::Truncated(buf.len()));
            }
            Ok(Datagram::ClipboardAck(u32::from_be_bytes(
                buf[1..5].try_into().expect("slice len"),
            )))
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
        DatagramRef::ComputerName(name) => {
            if !valid_computer_name(name) {
                return Err(ProtocolError::InvalidComputerName);
            }
            let needed = 1 + name.len();
            if out.len() < needed {
                return Err(ProtocolError::BufferTooSmall {
                    needed,
                    have: out.len(),
                });
            }
            out[0] = EventType::ComputerName as u8;
            out[1..needed].copy_from_slice(name.as_bytes());
            Ok(needed)
        }
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
        DatagramRef::AudioBatch(frames) => {
            if frames.is_empty() || frames.len() > u8::MAX as usize {
                return Err(ProtocolError::InvalidAudioBatchCount(frames.len() as u8));
            }
            let needed = AUDIO_BATCH_HEADER_SIZE
                + frames
                    .iter()
                    .map(|frame| AUDIO_BATCH_FRAME_HEADER_SIZE + frame.payload.len())
                    .sum::<usize>();
            if let Some(frame) = frames.iter().find(|frame| frame.payload.is_empty()) {
                return Err(ProtocolError::InvalidAudioLength(frame.payload.len() as u16));
            }
            if frames
                .iter()
                .any(|frame| frame.payload.len() > u16::MAX as usize)
            {
                return Err(ProtocolError::InvalidAudioLength(u16::MAX));
            }
            if out.len() < needed {
                return Err(ProtocolError::BufferTooSmall {
                    needed,
                    have: out.len(),
                });
            }
            out[0] = EventType::AudioBatch as u8;
            out[1] = frames.len() as u8;
            let mut cursor = AUDIO_BATCH_HEADER_SIZE;
            for frame in frames {
                out[cursor..cursor + 4].copy_from_slice(&frame.seq.to_be_bytes());
                out[cursor + 4..cursor + 8].copy_from_slice(&frame.ts_ms.to_be_bytes());
                out[cursor + 8..cursor + 10]
                    .copy_from_slice(&(frame.payload.len() as u16).to_be_bytes());
                cursor += AUDIO_BATCH_FRAME_HEADER_SIZE;
                out[cursor..cursor + frame.payload.len()].copy_from_slice(frame.payload);
                cursor += frame.payload.len();
            }
            Ok(cursor)
        }
        DatagramRef::ClipboardText {
            transfer_id,
            index,
            count,
            payload,
        } => {
            let max_count = MAX_CLIPBOARD_TEXT_SIZE.div_ceil(MAX_CLIPBOARD_FRAGMENT_SIZE) as u16;
            if count == 0 || index >= count || count > max_count {
                return Err(ProtocolError::InvalidClipboardFragment { index, count });
            }
            if payload.len() > MAX_CLIPBOARD_FRAGMENT_SIZE {
                return Err(ProtocolError::ClipboardFragmentTooLarge(payload.len()));
            }
            let needed = CLIPBOARD_HEADER_SIZE + payload.len();
            if out.len() < needed {
                return Err(ProtocolError::BufferTooSmall {
                    needed,
                    have: out.len(),
                });
            }
            out[0] = EventType::ClipboardText as u8;
            out[1..5].copy_from_slice(&transfer_id.to_be_bytes());
            out[5..7].copy_from_slice(&index.to_be_bytes());
            out[7..9].copy_from_slice(&count.to_be_bytes());
            out[CLIPBOARD_HEADER_SIZE..needed].copy_from_slice(payload);
            Ok(needed)
        }
        DatagramRef::ClipboardAck(transfer_id) => {
            let needed = 1 + 4;
            if out.len() < needed {
                return Err(ProtocolError::BufferTooSmall {
                    needed,
                    have: out.len(),
                });
            }
            out[0] = EventType::ClipboardAck as u8;
            out[1..needed].copy_from_slice(&transfer_id.to_be_bytes());
            Ok(needed)
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
    fn computer_name_round_trips() {
        for name in ["Biah".to_owned(), "机器🦀".to_owned(), "x".repeat(255)] {
            let encoded = encode(DatagramRef::ComputerName(&name));
            assert_eq!(encoded[0], 17);
            assert_eq!(&encoded[1..], name.as_bytes());
            let Datagram::ComputerName(decoded) = decode(&encoded).expect("decode") else {
                panic!("expected computer name");
            };
            assert_eq!(decoded, name);
        }
    }

    #[test]
    fn computer_name_rejects_malformed_payloads() {
        for payload in [
            vec![],
            vec![0xff],
            vec![b'a'; 256],
            b"Biah\0".to_vec(),
            b"Biah\n".to_vec(),
            "Biah\u{0085}".as_bytes().to_vec(),
        ] {
            let mut packet = vec![17];
            packet.extend(payload);
            assert!(matches!(
                decode(&packet),
                Err(ProtocolError::InvalidComputerName)
            ));
        }
        for name in [
            "".to_owned(),
            "a".repeat(256),
            "a\t".to_owned(),
            "é".repeat(128),
        ] {
            assert!(matches!(
                encode_into(
                    DatagramRef::ComputerName(&name),
                    &mut [0; MAX_DATAGRAM_SIZE]
                ),
                Err(ProtocolError::InvalidComputerName)
            ));
        }
        assert!(matches!(
            encode_into(DatagramRef::ComputerName("Biah"), &mut [0; 4]),
            Err(ProtocolError::BufferTooSmall { needed: 5, have: 4 })
        ));
        let mut fixed = [0; MAX_EVENT_SIZE];
        fixed[0] = 17;
        assert!(matches!(
            ProtoEvent::try_from(fixed),
            Err(ProtocolError::UnexpectedVariableEvent(
                EventType::ComputerName
            ))
        ));
    }

    #[test]
    fn clipboard_fragment_round_trips_with_bounded_payload() {
        let text = "clipboard 🦀 text".as_bytes();
        let encoded = encode(DatagramRef::ClipboardText {
            transfer_id: 0x1234_5678,
            index: 1,
            count: 3,
            payload: text,
        });
        let Datagram::ClipboardText(fragment) = decode(&encoded).expect("decode") else {
            panic!("expected clipboard fragment");
        };
        assert_eq!(fragment.transfer_id, 0x1234_5678);
        assert_eq!(fragment.index, 1);
        assert_eq!(fragment.count, 3);
        assert_eq!(&encoded[fragment.payload_range], text);
        assert!(encoded.len() <= MAX_DATAGRAM_SIZE);
    }

    #[test]
    fn clipboard_ack_round_trips() {
        let encoded = encode(DatagramRef::ClipboardAck(0xAABB_CCDD));
        assert_eq!(encoded.len(), 5);
        assert!(matches!(
            decode(&encoded),
            Ok(Datagram::ClipboardAck(0xAABB_CCDD))
        ));
    }

    #[test]
    fn clipboard_fragment_rejects_invalid_indices_and_oversize_payloads() {
        let mut invalid = [0u8; CLIPBOARD_HEADER_SIZE];
        invalid[0] = crate::EventType::ClipboardText as u8;
        invalid[5..7].copy_from_slice(&1u16.to_be_bytes());
        invalid[7..9].copy_from_slice(&1u16.to_be_bytes());
        assert!(matches!(
            decode(&invalid),
            Err(ProtocolError::InvalidClipboardFragment { index: 1, count: 1 })
        ));

        assert!(matches!(
            encode_into(
                DatagramRef::ClipboardText {
                    transfer_id: 1,
                    index: 0,
                    count: 1,
                    payload: &vec![0; MAX_CLIPBOARD_FRAGMENT_SIZE + 1],
                },
                &mut [0; MAX_DATAGRAM_SIZE],
            ),
            Err(ProtocolError::ClipboardFragmentTooLarge(_))
        ));
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
    fn audio_batch_roundtrip() {
        let payloads = [vec![1, 2, 3], vec![4, 5], vec![6; 240]];
        let frames = payloads
            .iter()
            .enumerate()
            .map(|(i, payload)| AudioFrameRef {
                seq: 40 + i as u32,
                ts_ms: 1000 + i as u32 * 20,
                payload,
            })
            .collect::<Vec<_>>();
        let buf = encode(DatagramRef::AudioBatch(&frames));
        let Datagram::AudioBatch(decoded) = decode(&buf).expect("decode") else {
            panic!("expected audio batch");
        };
        assert_eq!(decoded.len(), frames.len());
        for (i, frame) in decoded.iter().enumerate() {
            assert_eq!(frame.seq, frames[i].seq);
            assert_eq!(frame.ts_ms, frames[i].ts_ms);
            assert_eq!(&buf[frame.payload_range.clone()], payloads[i]);
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

    #[test]
    fn oversized_batch_frames_still_fit_individually() {
        // a 3-frame batch of large Opus frames can overflow the
        // datagram budget; the sender falls back to single-frame
        // datagrams instead of dropping the whole batch, so every
        // frame must encode on its own
        let payload = vec![0xAB; 420];
        let frames = (0..3)
            .map(|i| AudioFrameRef {
                seq: i,
                ts_ms: i * 20,
                payload: &payload,
            })
            .collect::<Vec<_>>();
        let mut buf = [0u8; MAX_DATAGRAM_SIZE];
        assert!(matches!(
            encode_into(DatagramRef::AudioBatch(&frames), &mut buf),
            Err(ProtocolError::BufferTooSmall { .. })
        ));
        for frame in &frames {
            let len = encode_into(
                DatagramRef::Audio {
                    seq: frame.seq,
                    ts_ms: frame.ts_ms,
                    payload: frame.payload,
                },
                &mut buf,
            )
            .expect("single frame fits");
            assert!(len <= MAX_DATAGRAM_SIZE);
        }
    }
}
