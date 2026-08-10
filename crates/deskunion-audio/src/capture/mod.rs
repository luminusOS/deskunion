use crate::{AudioDevice, AudioError, AudioFormat};

/// interleaved-sample callback invoked on the backend's real-time audio
/// thread; see [`AudioCapture::start`] for the constraints it must obey.
pub type CaptureCallback = Box<dyn FnMut(&[f32]) + Send>;

// cpal covers every OS it supports, including Windows — it's only
// system-output *loopback* capture that cpal can't do on Windows
// (rustaudio/cpal#476: added, then lost in a refactor). `wasapi` fills
// that one gap; on Linux/macOS, cpal alone already covers loopback too
// (see `is_monitor_name`'s doc comment).
mod cpal;
mod dummy;
#[cfg(windows)]
mod wasapi;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Cpal,
    #[cfg(windows)]
    Wasapi,
    Dummy,
}

/// A running or not-yet-started audio input backend. `start`/`stop` may
/// be called repeatedly; a fresh `start` after `stop` is expected to work.
pub trait AudioCapture: Send {
    /// enumerate available input devices
    fn devices(&self) -> Result<Vec<AudioDevice>, AudioError>;

    /// begin capturing from `device` (`None` = system default). `on_data`
    /// is invoked with interleaved samples on the backend's real-time
    /// audio thread — it must not block, allocate, or do I/O (see
    /// `deskunion/DESKUNION_AUDIO_PLAN.md` §5.4). Returns the format the
    /// stream actually opened at.
    fn start(
        &mut self,
        device: Option<&str>,
        on_data: CaptureCallback,
    ) -> Result<AudioFormat, AudioError>;

    /// stop the stream, if running. Safe to call when not started.
    fn stop(&mut self);
}

pub fn create(backend: Backend) -> Box<dyn AudioCapture> {
    match backend {
        Backend::Cpal => Box::new(cpal::CpalCapture::new()),
        #[cfg(windows)]
        Backend::Wasapi => Box::new(wasapi::WasapiCapture::new()),
        Backend::Dummy => Box::new(dummy::DummyCapture::new()),
    }
}

/// heuristic: does this device name/id look like a loopback/monitor
/// source (system output) rather than a real input device (mic/line-in)?
///
/// `.monitor` / `monitor of` cover PulseAudio-style naming. `sink` /
/// `_output.` cover cpal's native PipeWire host: verified empirically
/// (§3.5 spike) that opening PipeWire sink nodes — e.g. `sink_default`
/// or `alsa_output.*` — as an *input* device captures that sink's
/// audio (RMS tracked a live tone, silence otherwise). PipeWire has no
/// separate monitor-device concept in cpal's enumeration; every sink
/// node doubles as its own monitor tap.
pub(crate) fn is_monitor_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".monitor")
        || lower.starts_with("monitor of")
        || lower.contains("sink")
        || lower.contains("_output.")
}
