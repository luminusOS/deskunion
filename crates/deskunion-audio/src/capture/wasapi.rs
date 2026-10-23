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

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::call_watch::{SessionMonitor, Signal};
use super::calls::{CallTracker, ScanPacer};
use super::mixer::Mixer;
use super::process_loopback::{supports_process_loopback, use_process_loopback};
use super::{AudioCapture, CaptureCallback};
use crate::codec::SAMPLE_RATE;
use crate::{AudioDevice, AudioError, AudioFormat};
use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError, WaveFormat,
};

const CHANNELS: u16 = 2;
const SAMPLES_PER_SECOND: usize = SAMPLE_RATE as usize * CHANNELS as usize;
/// how often the mixer hands a block downstream
const MIX_TICK: Duration = Duration::from_millis(10);
/// outputs captured at once during a call; one is the norm
const MAX_CALL_OUTPUTS: usize = 4;
/// audio a stream must have buffered before it joins the mix
const MIX_PRIME_SAMPLES: usize = SAMPLES_PER_SECOND / 50;
/// more than this buffered means the stream outpaces the mixer clock
const MIX_MAX_BACKLOG_SAMPLES: usize = SAMPLES_PER_SECOND / 8;
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
    unsafe extern "system" {
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

/// What the process loopback and the call capture share. While a call is
/// captured from its output, that capture already holds every sound of the
/// output, so the process loopback stops forwarding to avoid playing
/// everything twice.
struct CallCapture {
    downstream: Mutex<CaptureCallback>,
    mixer: Mutex<Mixer>,
    engaged: AtomicBool,
}

impl CallCapture {
    fn new(downstream: CaptureCallback) -> Self {
        Self {
            downstream: Mutex::new(downstream),
            mixer: Mutex::new(Mixer::new(
                MAX_CALL_OUTPUTS,
                MIX_PRIME_SAMPLES,
                MIX_MAX_BACKLOG_SAMPLES,
            )),
            engaged: AtomicBool::new(false),
        }
    }

    fn forward(&self, data: &[f32]) {
        let mut downstream = self
            .downstream
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (*downstream)(data);
    }

    fn engaged(&self) -> bool {
        self.engaged.load(Ordering::Acquire)
    }
}

struct OutputCapture {
    slot: usize,
    name: String,
    opened: mpsc::Receiver<Result<AudioFormat, AudioError>>,
    stop: mpsc::Sender<()>,
    join: JoinHandle<()>,
    is_open: bool,
}

impl OutputCapture {
    fn start(calls: &Arc<CallCapture>, device_id: String, name: String, slot: usize) -> Self {
        let (stop, stop_rx) = mpsc::channel();
        let (format_tx, opened) = mpsc::channel();
        let calls = calls.clone();
        let join = thread::spawn(move || {
            run_output_capture(&device_id, &calls, slot, &stop_rx, format_tx)
        });
        Self {
            slot,
            name,
            opened,
            stop,
            join,
            is_open: false,
        }
    }

    fn refresh_open(&mut self) {
        if !self.is_open && matches!(self.opened.try_recv(), Ok(Ok(_))) {
            self.is_open = true;
        }
    }

    fn finish(self) {
        let _ = self.stop.send(());
        let _ = self.join.join();
    }
}

/// Keeps capturing one output into the call mixer, reopening it when it breaks.
fn run_output_capture(
    device_id: &str,
    calls: &Arc<CallCapture>,
    slot: usize,
    stop_rx: &mpsc::Receiver<()>,
    format_tx: mpsc::Sender<Result<AudioFormat, AudioError>>,
) {
    if ensure_com_initialized().is_err() {
        return;
    }
    let mut announced = false;
    let mut backoff = INITIAL_REOPEN_BACKOFF;
    loop {
        let ran_since = Instant::now();
        let calls = calls.clone();
        let mut on_data: CaptureCallback = Box::new(move |data| {
            calls
                .mixer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(slot, data);
        });
        match run_capture(
            CaptureSource::EndpointLoopback,
            Some(device_id),
            &mut on_data,
            stop_rx,
            &format_tx,
            &mut announced,
        ) {
            Ok(CaptureRun::Stopped) => return,
            Ok(CaptureRun::Interrupted) => log::warn!("call output capture interrupted; reopening"),
            Err(error) => log::warn!("call output capture failed ({error}); retrying"),
        }
        if ran_since.elapsed() >= STABLE_RUN_RESETS_BACKOFF {
            backoff = INITIAL_REOPEN_BACKOFF;
        }
        if stop_rx.recv_timeout(backoff).is_ok() {
            return;
        }
        backoff = (backoff * 2).min(MAX_REOPEN_BACKOFF);
    }
}

/// Process loopback returns silence for the Teams desktop client, so while a
/// call plays the output it plays on is captured instead. Woken by audio
/// notifications; see [`SessionMonitor`].
fn run_call_watch(
    calls: Arc<CallCapture>,
    signal_tx: mpsc::Sender<Signal>,
    signals: mpsc::Receiver<Signal>,
) {
    if ensure_com_initialized().is_err() {
        return;
    }
    let mut monitor = match SessionMonitor::new(signal_tx) {
        Ok(monitor) => monitor,
        Err(error) => {
            log::warn!(
                "cannot watch the Windows audio sessions ({error}); Teams calls will not be captured"
            );
            return;
        }
    };
    let Ok(enumerator) = DeviceEnumerator::new() else {
        return;
    };
    let mut tracker = CallTracker::default();
    let mut pacer = ScanPacer::default();
    let mut captures: HashMap<String, OutputCapture> = HashMap::new();
    'watch: loop {
        let now = Instant::now();
        let wanted = tracker.update(now, &monitor.active_call_outputs());
        captures.retain(|id, capture| {
            let keep = wanted.contains(id);
            if !keep {
                log::info!("Teams stopped playing on {}; releasing it", capture.name);
            }
            keep
        });
        for id in wanted {
            if captures.contains_key(&id) {
                continue;
            }
            let Some(slot) =
                (0..MAX_CALL_OUTPUTS).find(|slot| captures.values().all(|c| c.slot != *slot))
            else {
                break;
            };
            let name = enumerator
                .get_device(&id)
                .and_then(|device| device.get_friendlyname())
                .unwrap_or_else(|_| id.clone());
            log::info!(
                "Teams is playing on {name}; capturing that output while the call lasts (follows its Windows volume and mute)"
            );
            captures.insert(id.clone(), OutputCapture::start(&calls, id, name, slot));
        }
        for capture in captures.values_mut() {
            capture.refresh_open();
        }
        let engaged = captures.values().any(|capture| capture.is_open);
        if engaged != calls.engaged() {
            calls.engaged.store(engaged, Ordering::Release);
            if !engaged {
                calls
                    .mixer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clear();
            }
        }

        let wait = pacer.next_wait(now, tracker.next_release_in(now));
        let mut signal = signals.recv_timeout(wait);
        loop {
            match signal {
                Ok(Signal::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break 'watch,
                Ok(Signal::Changed) => pacer.changed(Instant::now()),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
            // a burst of notifications costs one look at the sessions
            signal = signals.try_recv().map_err(|error| match error {
                mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
                mpsc::TryRecvError::Disconnected => mpsc::RecvTimeoutError::Disconnected,
            });
        }
    }
    calls.engaged.store(false, Ordering::Release);
    for (_, capture) in captures.drain() {
        capture.finish();
    }
}

/// Paces the call mixer on the wall clock and hands the sum downstream.
fn run_mixer(calls: Arc<CallCapture>, stop_rx: mpsc::Receiver<()>) {
    let mut last = Instant::now();
    let mut owed_frames = 0.0_f64;
    let mut block = Vec::with_capacity(SAMPLES_PER_SECOND / 20);
    while let Err(mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(MIX_TICK) {
        let now = Instant::now();
        if !calls.engaged() {
            last = now;
            owed_frames = 0.0;
            continue;
        }
        owed_frames += now.duration_since(last).as_secs_f64() * f64::from(SAMPLE_RATE);
        last = now;
        let frames = owed_frames as usize;
        owed_frames -= frames as f64;
        block.clear();
        let mixed = calls
            .mixer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mix(&mut block, frames * CHANNELS as usize);
        if mixed {
            calls.forward(&block);
        }
    }
}

/// stops and joins the helper threads of a mixed capture, also when the
/// capture thread leaves early
struct Helpers(Vec<(StopHelper, JoinHandle<()>)>);

/// asks one helper thread to finish
type StopHelper = Box<dyn Fn() + Send>;

impl Drop for Helpers {
    fn drop(&mut self) {
        for (stop, _) in &self.0 {
            stop();
        }
        for (_, join) in self.0.drain(..) {
            let _ = join.join();
        }
    }
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
        let process_supported =
            windows_version().is_ok_and(|(major, build)| supports_process_loopback(major, build));
        let mut out = Vec::new();
        if process_supported {
            out.push(AudioDevice {
                id: "process-loopback".to_owned(),
                name: "All application audio (master-volume independent)".to_owned(),
                is_monitor: true,
                is_default: true,
            });
        }
        let enumerator = DeviceEnumerator::new().map_err(AudioError::from)?;
        let default_id = enumerator
            .get_default_device(&Direction::Render)
            .ok()
            .and_then(|d| d.get_id().ok());
        let collection = enumerator.get_device_collection(&Direction::Render)?;
        let count = collection.get_nbr_devices()?;
        out.reserve(count as usize);
        for i in 0..count {
            let device = collection.get_device_at_index(i)?;
            let id = device.get_id()?;
            let name = device.get_friendlyname()?;
            out.push(AudioDevice {
                is_default: !process_supported && default_id.as_deref() == Some(id.as_str()),
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
                Ok((major, build)) if use_process_loopback(major, build, device_id.as_deref()) => {
                    CaptureSource::ProcessLoopback
                }
                Ok((major, build)) if supports_process_loopback(major, build) => {
                    log::info!("using explicitly selected Windows render endpoint");
                    CaptureSource::EndpointLoopback
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

            let mut helpers = Helpers(Vec::new());
            if source == CaptureSource::ProcessLoopback {
                let downstream = std::mem::replace(&mut on_data, Box::new(|_| {}));
                let calls = Arc::new(CallCapture::new(downstream));
                let (mixer_stop, mixer_rx) = mpsc::channel();
                let mixer_calls = calls.clone();
                helpers.0.push((
                    Box::new(move || {
                        let _ = mixer_stop.send(());
                    }),
                    thread::spawn(move || run_mixer(mixer_calls, mixer_rx)),
                ));
                let (signal_tx, signals) = mpsc::channel();
                let stop_tx = signal_tx.clone();
                let watch_calls = calls.clone();
                helpers.0.push((
                    Box::new(move || {
                        let _ = stop_tx.send(Signal::Stop);
                    }),
                    thread::spawn(move || run_call_watch(watch_calls, signal_tx, signals)),
                ));
                on_data = Box::new(move |data| {
                    if !calls.engaged() {
                        calls.forward(data);
                    }
                });
            }

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
                Some("process-loopback") | None => {
                    enumerator.get_default_device(&Direction::Render)?
                }
                Some(id) => match enumerator.get_device(id) {
                    Ok(device) => device,
                    Err(error) => {
                        log::warn!("configured Windows audio endpoint is unavailable ({error})");
                        return Err(AudioError::from(error));
                    }
                },
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
