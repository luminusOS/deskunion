//! Windows system-output loopback capture via raw WASAPI. cpal has no
//! WASAPI loopback support on Windows (rustaudio/cpal#476: added in
//! PR #339, then lost in a later refactor, still absent in 0.18) — this
//! fills that one gap. cpal's own Windows backend still handles every
//! other case (plain mic input, playback); see `capture/cpal.rs`.
//!
//! Loopback in WASAPI isn't a distinct device type — it's an ordinary
//! *render* (output) endpoint, opened with the capture direction
//! requested against it. `AudioClient::initialize_client` in the
//! `wasapi` crate turns that specific combination (client's own
//! direction is `Render`, but `Direction::Capture` is requested, shared
//! mode) into `AUDCLNT_STREAMFLAGS_LOOPBACK` automatically — see
//! `wasapi-0.23.0/src/api.rs` around `initialize_client`. This only
//! shows up by reading that source; the crate's own `examples/loopback.rs`
//! is misleadingly named — it actually does plain mic capture, not
//! loopback.
//!
//! The backend is cross-compiled as part of the Windows package. Runtime
//! failures are returned to the service and written to DeskUnion's log.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use super::process_loopback::supports_process_loopback;
use super::{AudioCapture, CaptureCallback};
use crate::codec::SAMPLE_RATE;
use crate::{AudioDevice, AudioError, AudioFormat};
use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError, WaveFormat,
};

const CHANNELS: u16 = 2;
const BYTES_PER_SAMPLE: usize = 4; // 32-bit float, matches WaveFormat below
/// how long to wait for a WASAPI buffer-ready event before checking for
/// a stop request; keeps `stop()` responsive without busy-polling
const EVENT_TIMEOUT_MS: u32 = 100;
/// how long to wait before reopening the endpoint after the capture
/// stream broke, doubling up to this ceiling
const MAX_REOPEN_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);
/// the first reopen delay, and what the backoff returns to after a run
/// that held up long enough not to count as part of a failure burst
const INITIAL_REOPEN_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);
/// a capture run at least this long is treated as recovered, so the next
/// unrelated interruption starts over at [`INITIAL_REOPEN_BACKOFF`]
const STABLE_RUN_RESETS_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

/// why [`run_capture`] returned
enum CaptureRun {
    /// `stop()` was requested
    Stopped,
    /// the endpoint went away (device change, format change, session
    /// reset); the caller reopens it
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureSource {
    ProcessLoopback,
    EndpointLoopback,
}

impl CaptureSource {
    fn description(self) -> &'static str {
        match self {
            Self::ProcessLoopback => {
                "system-wide process loopback (excluding DeskUnion; master-volume independent; per-app volume preserved)"
            }
            Self::EndpointLoopback => "endpoint loopback (follows Windows master volume/mute)",
        }
    }
}

#[cfg(windows)]
fn windows_version() -> Result<(u32, u32), AudioError> {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major_version: u32,
        minor_version: u32,
        build_number: u32,
        platform_id: u32,
        service_pack: [u16; 128],
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(version: *mut OsVersionInfoW) -> i32;
    }

    let mut version = OsVersionInfoW {
        size: std::mem::size_of::<OsVersionInfoW>() as u32,
        major_version: 0,
        minor_version: 0,
        build_number: 0,
        platform_id: 0,
        service_pack: [0; 128],
    };
    // SAFETY: RtlGetVersion writes into the caller-provided, correctly sized struct.
    let status = unsafe { RtlGetVersion(&mut version) };
    if status < 0 {
        return Err(AudioError::WindowsVersion(format!(
            "RtlGetVersion returned NTSTATUS {status:#x}"
        )));
    }
    Ok((version.major_version, version.build_number))
}

fn ensure_com_initialized() -> Result<(), AudioError> {
    // safe to call more than once per thread: repeat CoInitializeEx
    // calls return S_FALSE (a non-error HRESULT), which `.ok()` treats
    // as success.
    wasapi::initialize_mta()
        .ok()
        .map_err(WasapiError::Windows)?;
    Ok(())
}

pub struct WasapiCapture {
    stop_tx: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WasapiCapture {
    pub fn new() -> Self {
        Self {
            stop_tx: None,
            join: None,
        }
    }
}

impl Default for WasapiCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCapture for WasapiCapture {
    fn devices(&self) -> Result<Vec<AudioDevice>, AudioError> {
        ensure_com_initialized()?;
        if windows_version().is_ok_and(|(major, build)| supports_process_loopback(major, build)) {
            return Ok(vec![AudioDevice {
                id: "process-loopback".to_owned(),
                name: "All application audio (master-volume independent)".to_owned(),
                is_monitor: true,
                is_default: true,
            }]);
        }
        let enumerator = DeviceEnumerator::new().map_err(AudioError::from)?;
        let default_id = enumerator
            .get_default_device(&Direction::Render)
            .ok()
            .and_then(|d| d.get_id().ok());
        let collection = enumerator.get_device_collection(&Direction::Render)?;
        let count = collection.get_nbr_devices()?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let device = collection.get_device_at_index(i)?;
            let id = device.get_id()?;
            let name = device.get_friendlyname()?;
            out.push(AudioDevice {
                is_default: default_id.as_deref() == Some(id.as_str()),
                id,
                name,
                // every entry here is a render endpoint opened in
                // loopback mode — by construction, all of them are
                // "monitor" (system-output) sources.
                is_monitor: true,
            });
        }
        Ok(out)
    }

    fn start(
        &mut self,
        device: Option<&str>,
        mut on_data: CaptureCallback,
    ) -> Result<AudioFormat, AudioError> {
        self.stop();

        let device_id = device.map(str::to_owned);
        let (format_tx, format_rx) = mpsc::channel::<Result<AudioFormat, AudioError>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let join = thread::spawn(move || {
            if let Err(e) = ensure_com_initialized() {
                let _ = format_tx.send(Err(e));
                return;
            }

            let source = match windows_version() {
                Ok((major, build)) if supports_process_loopback(major, build) => {
                    CaptureSource::ProcessLoopback
                }
                Ok((major, build)) => {
                    log::warn!(
                        "process loopback unavailable on Windows {major}.{build}; using endpoint loopback"
                    );
                    CaptureSource::EndpointLoopback
                }
                Err(error) => {
                    log::warn!(
                        "unable to determine Windows version ({error}); using endpoint loopback"
                    );
                    CaptureSource::EndpointLoopback
                }
            };
            log::info!("Windows audio capture mode: {}", source.description());

            // a broken capture used to end the thread with a single
            // warning: the encode thread then slept forever on an empty
            // ring and audio stopped while the connection stayed up.
            // Reopen instead.
            let mut announced = false;
            let mut backoff = INITIAL_REOPEN_BACKOFF;
            loop {
                let ran_since = std::time::Instant::now();
                match run_capture(
                    source,
                    device_id.as_deref(),
                    &mut on_data,
                    &stop_rx,
                    &format_tx,
                    &mut announced,
                ) {
                    Ok(CaptureRun::Stopped) => break,
                    Ok(CaptureRun::Interrupted) => {
                        log::error!(
                            "wasapi loopback capture was interrupted; reopening the endpoint"
                        );
                    }
                    Err(e) => {
                        if !announced {
                            let _ = format_tx.send(Err(e));
                            return;
                        }
                        log::error!(
                            "Windows audio capture failed in {} mode ({e}); reopening",
                            source.description()
                        );
                    }
                }
                // a run that streamed for a while was not part of the
                // burst this backoff is pacing. Without this the delay
                // only ever grows, so after a handful of unrelated
                // endpoint changes over a session (default-device switch,
                // format change) every later interruption costs the full
                // MAX_REOPEN_BACKOFF of silence instead of 200 ms.
                if ran_since.elapsed() >= STABLE_RUN_RESETS_BACKOFF {
                    backoff = INITIAL_REOPEN_BACKOFF;
                }
                if stop_rx.recv_timeout(backoff).is_ok() {
                    break;
                }
                backoff = (backoff * 2).min(MAX_REOPEN_BACKOFF);
            }
        });

        self.stop_tx = Some(stop_tx);
        self.join = Some(join);
        format_rx.recv().map_err(|_| AudioError::BackendClosed)?
    }

    fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// open the loopback endpoint and pump samples into `on_data` until
/// `stop_rx` fires or the endpoint breaks. Announces the negotiated
/// format through `format_tx` on the first successful open only —
/// `start()` blocks on that, and later reopens must not resend it.
fn run_capture(
    source: CaptureSource,
    device_id: Option<&str>,
    on_data: &mut CaptureCallback,
    stop_rx: &mpsc::Receiver<()>,
    format_tx: &mpsc::Sender<Result<AudioFormat, AudioError>>,
    announced: &mut bool,
) -> Result<CaptureRun, AudioError> {
    let mut audio_client = match source {
        CaptureSource::ProcessLoopback => {
            AudioClient::new_application_loopback_client(std::process::id(), false)?
        }
        CaptureSource::EndpointLoopback => {
            let enumerator = DeviceEnumerator::new()?;
            let device = match device_id {
                Some(id) => match enumerator.get_device(id) {
                    Ok(device) => device,
                    Err(error) => {
                        // Older DeskUnion builds enumerated CPAL input
                        // devices on Windows. Their persisted IDs are not
                        // render endpoint IDs, so keep upgrades working by
                        // falling back to the default output.
                        log::warn!(
                            "configured Windows audio endpoint is unavailable ({error}); using the default output"
                        );
                        enumerator.get_default_device(&Direction::Render)?
                    }
                },
                None => enumerator.get_default_device(&Direction::Render)?,
            };
            device.get_iaudioclient()?
        }
    };
    let desired_format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        SAMPLE_RATE as usize,
        CHANNELS as usize,
        None,
    );
    // Process-loopback duration is not device-relative, so use a fixed
    // shared-mode duration there. Endpoint loopback uses 3× engine period
    // to leave headroom for scheduling jitter.
    let buffer_duration_hns = match source {
        CaptureSource::ProcessLoopback => 200_000,
        CaptureSource::EndpointLoopback => audio_client.get_device_period()?.0 * 3,
    };
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns,
    };
    // Process loopback clients also require shared capture mode.
    audio_client.initialize_client(&desired_format, &Direction::Capture, &mode)?;

    let event_handle = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;
    audio_client.start_stream()?;

    log::info!("Windows audio capture active: {}", source.description());
    if !*announced {
        let _ = format_tx.send(Ok(AudioFormat {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
        }));
        *announced = true;
    }

    let mut byte_queue: VecDeque<u8> = VecDeque::with_capacity(BYTES_PER_SAMPLE * 4096);
    let mut samples = Vec::with_capacity(4096);
    let outcome = loop {
        if stop_rx.try_recv().is_ok() {
            break CaptureRun::Stopped;
        }
        match event_handle.wait_for_event(EVENT_TIMEOUT_MS) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => continue,
            Err(e) => {
                log::warn!("wasapi loopback event wait failed: {e}");
                break CaptureRun::Interrupted;
            }
        }
        if let Err(e) = capture_client.read_from_device_to_deque(&mut byte_queue) {
            log::warn!("wasapi loopback read failed: {e}");
            break CaptureRun::Interrupted;
        }
        let usable_bytes = byte_queue.len() - (byte_queue.len() % BYTES_PER_SAMPLE);
        if usable_bytes == 0 {
            continue;
        }
        samples.clear();
        samples.reserve(usable_bytes / BYTES_PER_SAMPLE);
        for _ in 0..usable_bytes / BYTES_PER_SAMPLE {
            let mut bytes = [0u8; BYTES_PER_SAMPLE];
            for byte in &mut bytes {
                *byte = byte_queue.pop_front().expect("checked usable_bytes above");
            }
            samples.push(f32::from_le_bytes(bytes));
        }
        on_data(&samples);
    };

    let _ = audio_client.stop_stream();
    Ok(outcome)
}
