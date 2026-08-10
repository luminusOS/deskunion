//! Thin bridge between `deskunion_audio` and the rest of the service.
//! Kept in one place, behind the `audio` feature, so `listen.rs` /
//! `connect.rs` only need a narrow `#[cfg(feature = "audio")]` surface
//! (see `deskunion/DESKUNION_AUDIO_PLAN.md` §5.3) rather than being
//! sprinkled with `deskunion_audio` types throughout.

use deskunion_audio::{AudioReceiver, AudioSender, capture, playback};
use tokio::sync::mpsc::{Receiver, channel};

use crate::config::AudioSettings;

/// wire channel count — stereo, per plano §3.2
pub(crate) const WIRE_CHANNELS: u16 = 2;

/// `(seq, ts_ms, opus payload)`
pub(crate) type EncodedFrame = (u32, u32, Vec<u8>);

// Keep latency bounded if DTLS briefly stalls. Audio is real-time data:
// dropping an old frame is preferable to building an unbounded queue that
// delays input/keepalive traffic and eventually plays seconds behind.
const AUDIO_QUEUE_FRAMES: usize = 8;

fn capture_backend() -> capture::Backend {
    // CPAL's Windows backend only exposes regular input endpoints. For
    // system-output forwarding we need the render-endpoint loopback
    // implementation in `deskunion_audio::capture::wasapi`.
    #[cfg(windows)]
    {
        capture::Backend::Wasapi
    }
    #[cfg(not(windows))]
    {
        capture::Backend::Cpal
    }
}

/// start capturing+encoding this machine's audio, if `audio.send` is
/// enabled. Returns the live sender (keep it alive — dropping it stops
/// the stream) and a channel of encoded frames to forward to the peer.
pub(crate) fn start_sender(
    settings: &AudioSettings,
) -> Option<(AudioSender, Receiver<EncodedFrame>)> {
    if !settings.send {
        return None;
    }
    let (tx, rx) = channel(AUDIO_QUEUE_FRAMES);
    let backend = capture_backend();
    log::info!(
        "starting audio sender with {backend:?} capture{}",
        settings
            .capture_device
            .as_deref()
            .map(|device| format!(" device {device}"))
            .unwrap_or_else(|| " on the default output".to_owned())
    );
    // frames dropped here never reach the peer — the receiver conceals
    // them as packet loss, so make the drops visible at debug level
    // instead of failing silently
    let mut queue_drops = 0u64;
    match AudioSender::start(
        backend,
        settings.capture_device.as_deref(),
        WIRE_CHANNELS,
        settings.bitrate as i32,
        Box::new(move |seq, ts_ms, payload: &[u8]| {
            if tx.try_send((seq, ts_ms, payload.to_vec())).is_err() {
                queue_drops += 1;
                if queue_drops == 1 || queue_drops.is_multiple_of(100) {
                    log::debug!(
                        "audio send queue full ({AUDIO_QUEUE_FRAMES} frames); dropped an encoded frame ({queue_drops} dropped so far)"
                    );
                }
            }
        }),
    ) {
        Ok(sender) => Some((sender, rx)),
        Err(e) => {
            log::warn!("failed to start audio sender: {e}");
            None
        }
    }
}

/// start playback for a peer stream that just announced itself via
/// `AudioControl::Start`, if `audio.receive` is enabled. The announced
/// `sample_rate`/`channels` are honored rather than assuming the wire
/// defaults.
pub(crate) fn start_receiver(
    settings: &AudioSettings,
    sample_rate: u32,
    channels: u8,
) -> Option<AudioReceiver> {
    if !settings.receive {
        return None;
    }
    match AudioReceiver::start(
        playback::Backend::Cpal,
        settings.playback_device.as_deref(),
        sample_rate,
        channels as u16,
        settings.buffer_ms,
    ) {
        Ok(receiver) => Some(receiver),
        Err(e) => {
            log::warn!("failed to start audio receiver: {e}");
            None
        }
    }
}

/// list available capture and playback devices, for
/// `FrontendRequest::EnumerateAudioDevices`.
pub(crate) fn enumerate_devices() -> (
    Vec<deskunion_ipc::AudioDeviceInfo>,
    Vec<deskunion_ipc::AudioDeviceInfo>,
) {
    let capture = capture::create(capture_backend())
        .devices()
        .unwrap_or_else(|e| {
            log::warn!("failed to list capture devices: {e}");
            Vec::new()
        });
    let playback = playback::create(playback::Backend::Cpal)
        .devices()
        .unwrap_or_else(|e| {
            log::warn!("failed to list playback devices: {e}");
            Vec::new()
        });
    (to_ipc_devices(capture), to_ipc_devices(playback))
}

fn to_ipc_devices(
    devices: Vec<deskunion_audio::AudioDevice>,
) -> Vec<deskunion_ipc::AudioDeviceInfo> {
    devices
        .into_iter()
        .map(|d| deskunion_ipc::AudioDeviceInfo {
            id: d.id,
            name: d.name,
            is_monitor: d.is_monitor,
            is_default: d.is_default,
        })
        .collect()
}
