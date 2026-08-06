//! Thin bridge between `deskunion_audio` and the rest of the service.
//! Kept in one place, behind the `audio` feature, so `listen.rs` /
//! `connect.rs` only need a narrow `#[cfg(feature = "audio")]` surface
//! (see `deskunion/DESKUNION_AUDIO_PLAN.md` §5.3) rather than being
//! sprinkled with `deskunion_audio` types throughout.

use deskunion_audio::{AudioReceiver, AudioSender, capture, playback};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::config::AudioSettings;

/// wire channel count — stereo, per plano §3.2
pub(crate) const WIRE_CHANNELS: u16 = 2;

/// `(seq, ts_ms, opus payload)`
pub(crate) type EncodedFrame = (u32, u32, Vec<u8>);

/// start capturing+encoding this machine's audio, if `audio.send` is
/// enabled. Returns the live sender (keep it alive — dropping it stops
/// the stream) and a channel of encoded frames to forward to the peer.
pub(crate) fn start_sender(
    settings: &AudioSettings,
) -> Option<(AudioSender, UnboundedReceiver<EncodedFrame>)> {
    if !settings.send {
        return None;
    }
    let (tx, rx) = unbounded_channel();
    match AudioSender::start(
        capture::Backend::Cpal,
        settings.capture_device.as_deref(),
        WIRE_CHANNELS,
        settings.bitrate as i32,
        Box::new(move |seq, ts_ms, payload: &[u8]| {
            let _ = tx.send((seq, ts_ms, payload.to_vec()));
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
/// `AudioControl::Start`, if `audio.receive` is enabled.
pub(crate) fn start_receiver(settings: &AudioSettings) -> Option<AudioReceiver> {
    if !settings.receive {
        return None;
    }
    match AudioReceiver::start(
        playback::Backend::Cpal,
        settings.playback_device.as_deref(),
        WIRE_CHANNELS,
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
    let capture = capture::create(capture::Backend::Cpal)
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
