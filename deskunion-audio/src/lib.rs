//! Cross-platform audio capture/playback for deskunion's audio streaming
//! feature. See `deskunion/DESKUNION_AUDIO_PLAN.md` for the design.

pub mod capture;
pub mod codec;
mod error;
pub mod jitter;
pub mod playback;
pub mod receiver;
pub mod sender;

pub use receiver::AudioReceiver;
pub use sender::AudioSender;

pub use error::AudioError;

/// PCM format of a raw audio callback: interleaved samples at this
/// sample rate and channel count. Devices are used at their native
/// format; resampling to the wire format (48kHz) happens upstream in
/// `sender`/`receiver`, not in the capture/playback backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// backend-specific identifier; pass back into `start()` to select
    /// this device. Currently the device name (cpal has no stable id).
    pub id: String,
    pub name: String,
    /// true if this looks like a system-output loopback/monitor source
    /// rather than a real input (e.g. PipeWire/PulseAudio `*.monitor`)
    pub is_monitor: bool,
    pub is_default: bool,
}
