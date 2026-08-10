use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::DeviceId;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::{AudioDevice, AudioError, AudioFormat};

/// input frames pulled per top-up in the resampling fallback path
const RESAMPLER_PULL_SAMPLES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Cpal,
    /// discards pushed samples; used in tests and as a last-resort
    /// fallback with no audio hardware
    Dummy,
}

pub fn create(backend: Backend) -> Box<dyn AudioPlayback> {
    match backend {
        Backend::Cpal => Box::new(CpalPlayback::new()),
        Backend::Dummy => Box::new(DummyPlayback::new()),
    }
}

pub trait AudioPlayback: Send {
    fn devices(&self) -> Result<Vec<AudioDevice>, AudioError>;

    /// open an output stream at `format` on `device` (`None` = system
    /// default) and let it pull its audio from `source`.
    fn start(
        &mut self,
        device: Option<&str>,
        format: AudioFormat,
        source: Box<dyn AudioSource>,
    ) -> Result<(), AudioError>;

    /// stop the stream, if running. Safe to call when not started.
    fn stop(&mut self);

    /// true once the backend reported a stream error it cannot recover
    /// from on its own. The owner is expected to tear the stream down
    /// and open a new one — a dead cpal stream otherwise stays silent
    /// forever with everything upstream looking healthy.
    fn failed(&self) -> bool {
        false
    }
}

/// audio the output device pulls, on the device's own clock.
///
/// The device is the only accurate clock in the pipeline: a producer
/// thread ticking on `thread::sleep` always runs slightly slow, so a
/// push model drains whatever buffer sits in between until playback is
/// permanently silent. Pulling removes that buffer entirely.
pub trait AudioSource: Send {
    /// fill `out` with the next interleaved samples in the stream's
    /// format. Runs on the device's realtime callback: it must never
    /// block, and it must always fill the whole buffer — silence when
    /// there is nothing to play.
    fn fill(&mut self, out: &mut [f32]);
}

/// an [`AudioSource`] that only ever plays silence
pub struct SilentSource;

impl AudioSource for SilentSource {
    fn fill(&mut self, out: &mut [f32]) {
        out.fill(0.0);
    }
}

/// output on any OS cpal supports. Like `CpalCapture`, owns the stream
/// on a dedicated thread since `cpal::Stream` isn't `Send`.
pub struct CpalPlayback {
    stop_tx: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
    /// set from the cpal error callback; see [`AudioPlayback::failed`]
    failed: Arc<AtomicBool>,
}

impl CpalPlayback {
    pub fn new() -> Self {
        Self {
            stop_tx: None,
            join: None,
            failed: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Default for CpalPlayback {
    fn default() -> Self {
        Self::new()
    }
}

/// does the device advertise an f32 output config matching `config`
/// (channels and sample rate) exactly?
fn supports_config(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
) -> Result<bool, cpal::Error> {
    Ok(device.supported_output_configs()?.any(|c| {
        c.sample_format() == cpal::SampleFormat::F32
            && c.channels() == config.channels
            && c.min_sample_rate() <= config.sample_rate
            && config.sample_rate <= c.max_sample_rate()
    }))
}

/// pick a workable f32 output config when the wire format isn't
/// supported: the device default if it's f32, otherwise the
/// highest-rate f32 range on offer.
fn fallback_config(device: &cpal::Device) -> Option<cpal::StreamConfig> {
    if let Ok(default) = device.default_output_config() {
        if default.sample_format() == cpal::SampleFormat::F32 {
            return Some(default.config());
        }
    }
    device
        .supported_output_configs()
        .ok()?
        .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
        .max_by_key(|c| c.max_sample_rate())
        .map(|c| c.with_max_sample_rate().config())
}

/// linear-interpolating resampler between the wire format and the
/// device's actual output config — only used when the device can't run
/// at the wire format directly (see [`fallback_config`]). Runs inside
/// the cpal callback, pulling its input from the same [`AudioSource`]
/// the primary path uses, so it never blocks.
/// Extra output channels replicate the last input channel; extra input
/// channels are dropped. Good enough for a fallback path — the primary
/// path does no resampling at all.
struct LinearResampler {
    in_channels: usize,
    out_channels: usize,
    /// input frames consumed per output frame
    step: f64,
    /// fractional read position within `buf`, in input frames
    pos: f64,
    /// unconsumed input samples, always a whole number of frames
    buf: std::collections::VecDeque<f32>,
}

impl LinearResampler {
    fn new(in_rate: u32, in_channels: u16, out_rate: u32, out_channels: u16) -> Self {
        Self {
            in_channels: in_channels as usize,
            out_channels: out_channels as usize,
            step: f64::from(in_rate) / f64::from(out_rate),
            pos: 0.0,
            buf: Default::default(),
        }
    }

    fn fill(&mut self, source: &mut dyn AudioSource, out: &mut [f32]) {
        let mut filled = 0;
        while filled + self.out_channels <= out.len() {
            // interpolation reads input frames floor(pos) and
            // floor(pos)+1 — top up until both are available
            while (self.buf.len() / self.in_channels) as f64 <= self.pos + 1.0 {
                let mut tmp = [0f32; RESAMPLER_PULL_SAMPLES];
                // pull whole input frames only
                let pull = (RESAMPLER_PULL_SAMPLES / self.in_channels) * self.in_channels;
                source.fill(&mut tmp[..pull]);
                self.buf.extend(&tmp[..pull]);
            }
            let base = self.pos as usize * self.in_channels;
            let frac = (self.pos - self.pos.floor()) as f32;
            for (ch, sample) in out[filled..filled + self.out_channels]
                .iter_mut()
                .enumerate()
            {
                let src = ch.min(self.in_channels - 1);
                let a = self.buf[base + src];
                let b = self.buf[base + self.in_channels + src];
                *sample = a + (b - a) * frac;
            }
            filled += self.out_channels;
            self.pos += self.step;
            // drop fully consumed input frames
            let consumed = self.pos as usize;
            if consumed > 0 {
                self.buf.drain(..consumed * self.in_channels);
                self.pos -= consumed as f64;
            }
        }
    }
}

fn find_device(host: &cpal::Host, id: &str) -> Option<cpal::Device> {
    DeviceId::from_str(id)
        .ok()
        .and_then(|id| host.device_by_id(&id))
}

impl AudioPlayback for CpalPlayback {
    fn devices(&self) -> Result<Vec<AudioDevice>, AudioError> {
        let host = cpal::default_host();
        let default_id = host.default_output_device().and_then(|d| d.id().ok());
        let mut out = Vec::new();
        for device in host.output_devices()? {
            let Ok(id) = device.id() else {
                continue;
            };
            let name = device.to_string();
            let is_default = default_id.as_ref() == Some(&id);
            out.push(AudioDevice {
                id: id.to_string(),
                // "monitor" (loopback capture) is a capture-device
                // concept; it doesn't apply to playback/output devices.
                is_monitor: false,
                name,
                is_default,
            });
        }
        Ok(out)
    }

    fn start(
        &mut self,
        device: Option<&str>,
        format: AudioFormat,
        mut source: Box<dyn AudioSource>,
    ) -> Result<(), AudioError> {
        self.stop();
        self.failed.store(false, Ordering::Relaxed);

        let device_id = device.map(str::to_owned);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AudioError>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let failed = self.failed.clone();

        let join = thread::spawn(move || {
            let host = cpal::default_host();
            let device = match &device_id {
                Some(id) => find_device(&host, id),
                None => host.default_output_device(),
            };
            let device = match device {
                Some(d) => d,
                None => {
                    let _ = ready_tx.send(Err(AudioError::DeviceNotFound));
                    return;
                }
            };

            let wanted = cpal::StreamConfig {
                channels: format.channels,
                sample_rate: format.sample_rate,
                buffer_size: cpal::BufferSize::Default,
            };

            // the fixed wire format (48 kHz stereo f32) isn't
            // universally supported — fall back to a config the device
            // actually offers and resample in the callback
            let config = match supports_config(&device, &wanted) {
                Ok(true) => wanted,
                Ok(false) => match fallback_config(&device) {
                    Some(config) => {
                        log::warn!(
                            "playback device does not support {} Hz/{} ch f32; using {} Hz/{} ch instead",
                            format.sample_rate,
                            format.channels,
                            config.sample_rate,
                            config.channels,
                        );
                        config
                    }
                    None => {
                        let _ = ready_tx.send(Err(AudioError::NoSupportedConfig));
                        return;
                    }
                },
                Err(e) => {
                    log::warn!(
                        "failed to query supported output configs ({e}); trying the wire format as-is"
                    );
                    wanted
                }
            };

            // a stream error is terminal for this stream: cpal will not
            // call the data callback again, so record it for `failed()`
            // instead of leaving the pipeline silently dead
            let err_fn = move |e| {
                log::error!("audio playback stream error: {e}");
                failed.store(true, Ordering::Relaxed);
            };

            let stream =
                if config.channels == format.channels && config.sample_rate == format.sample_rate {
                    device.build_output_stream(
                        config,
                        move |data: &mut [f32], _| source.fill(data),
                        err_fn,
                        None,
                    )
                } else {
                    let mut resampler = LinearResampler::new(
                        format.sample_rate,
                        format.channels,
                        config.sample_rate,
                        config.channels,
                    );
                    device.build_output_stream(
                        config,
                        move |data: &mut [f32], _| resampler.fill(source.as_mut(), data),
                        err_fn,
                        None,
                    )
                };

            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.into()));
                    return;
                }
            };

            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(e.into()));
                return;
            }

            let _ = ready_tx.send(Ok(()));
            let _ = stop_rx.recv();
        });

        ready_rx.recv().map_err(|_| AudioError::BackendClosed)??;
        self.stop_tx = Some(stop_tx);
        self.join = Some(join);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

/// discards the audio it plays; used in tests and as a last-resort
/// fallback with no audio hardware. Same role as
/// `capture::dummy::DummyCapture`.
///
/// A clock thread pulls from the source at the format's nominal rate,
/// like a real output device would, and records how much it pulled and
/// how many of those blocks came back fully silent — which is what
/// "played the first seconds, then went quiet" looks like from the
/// device's side.
pub struct DummyPlayback {
    metrics: DummyMetrics,
    clock_stop_tx: Option<mpsc::Sender<()>>,
    clock_thread: Option<JoinHandle<()>>,
}

/// live counters of a running [`DummyPlayback`] stream. Cheap to clone;
/// all clones point at the same counters.
#[derive(Clone, Default)]
pub struct DummyMetrics {
    /// samples the virtual device has pulled so far
    samples_received: Arc<Mutex<usize>>,
    /// clock ticks whose block came back fully silent
    silent_ticks: Arc<std::sync::atomic::AtomicU64>,
    /// total clock ticks so far
    clock_ticks: Arc<std::sync::atomic::AtomicU64>,
}

impl DummyMetrics {
    pub fn samples_received(&self) -> Arc<Mutex<usize>> {
        self.samples_received.clone()
    }

    pub fn silent_ticks(&self) -> u64 {
        self.silent_ticks.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn clock_ticks(&self) -> u64 {
        self.clock_ticks.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// how often the virtual playback clock consumes
const DUMMY_CLOCK_MS: u64 = 10;

impl DummyPlayback {
    pub fn new() -> Self {
        Self {
            metrics: DummyMetrics::default(),
            clock_stop_tx: None,
            clock_thread: None,
        }
    }

    pub fn samples_received(&self) -> Arc<Mutex<usize>> {
        self.metrics.samples_received()
    }

    pub fn metrics(&self) -> DummyMetrics {
        self.metrics.clone()
    }
}

impl Default for DummyPlayback {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPlayback for DummyPlayback {
    fn devices(&self) -> Result<Vec<AudioDevice>, AudioError> {
        Ok(vec![AudioDevice {
            id: "dummy".to_owned(),
            name: "Dummy (discard)".to_owned(),
            is_monitor: false,
            is_default: true,
        }])
    }

    fn start(
        &mut self,
        _device: Option<&str>,
        format: AudioFormat,
        mut source: Box<dyn AudioSource>,
    ) -> Result<(), AudioError> {
        self.stop();

        // fresh stream: reset the clock counters (samples_received
        // stays cumulative — tests hold that handle across restarts)
        use std::sync::atomic::Ordering::Relaxed;
        self.metrics.silent_ticks.store(0, Relaxed);
        self.metrics.clock_ticks.store(0, Relaxed);

        // virtual output device clock: pulls at the nominal rate and
        // records the blocks that came back silent
        let samples_per_tick =
            (format.sample_rate as u64 * DUMMY_CLOCK_MS / 1000) as usize * format.channels as usize;
        let metrics = self.metrics.clone();
        let (stop_tx, stop_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            let mut block = vec![0.0f32; samples_per_tick];
            loop {
                if stop_rx
                    .recv_timeout(std::time::Duration::from_millis(DUMMY_CLOCK_MS))
                    .is_ok()
                {
                    return;
                }
                source.fill(&mut block);
                *metrics.samples_received.lock().expect("lock") += block.len();
                if block.iter().all(|sample| *sample == 0.0) {
                    metrics.silent_ticks.fetch_add(1, Relaxed);
                }
                metrics.clock_ticks.fetch_add(1, Relaxed);
            }
        });
        self.clock_stop_tx = Some(stop_tx);
        self.clock_thread = Some(join);

        Ok(())
    }

    fn stop(&mut self) {
        if let Some(tx) = self.clock_stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.clock_thread.take() {
            let _ = join.join();
        }
    }
}

impl Drop for DummyPlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// plays back a fixed slice, then silence
    struct SliceSource {
        data: Vec<f32>,
        at: usize,
    }

    impl SliceSource {
        fn new(data: &[f32]) -> Self {
            Self {
                data: data.to_vec(),
                at: 0,
            }
        }
    }

    impl AudioSource for SliceSource {
        fn fill(&mut self, out: &mut [f32]) {
            for sample in out.iter_mut() {
                *sample = self.data.get(self.at).copied().unwrap_or(0.0);
                self.at += 1;
            }
        }
    }

    /// never runs out of audio
    struct ToneSource;

    impl AudioSource for ToneSource {
        fn fill(&mut self, out: &mut [f32]) {
            out.fill(0.25);
        }
    }

    #[test]
    fn dummy_playback_pulls_on_its_own_clock() {
        let mut playback = DummyPlayback::new();
        let metrics = playback.metrics();
        let samples = metrics.samples_received();
        playback
            .start(
                None,
                AudioFormat {
                    sample_rate: 48_000,
                    channels: 1,
                },
                Box::new(ToneSource),
            )
            .expect("start");
        std::thread::sleep(std::time::Duration::from_millis(100));
        playback.stop();

        let ticks = metrics.clock_ticks();
        assert!(ticks >= 3, "clock should have ticked, got {ticks}");
        assert_eq!(
            *samples.lock().expect("lock"),
            ticks as usize * 480,
            "the device pulls a full block on every tick"
        );
        assert_eq!(
            metrics.silent_ticks(),
            0,
            "a source with audio must not read as silent"
        );
    }

    #[test]
    fn dummy_playback_records_silent_blocks() {
        let mut playback = DummyPlayback::new();
        let metrics = playback.metrics();
        playback
            .start(
                None,
                AudioFormat {
                    sample_rate: 48_000,
                    channels: 1,
                },
                Box::new(SilentSource),
            )
            .expect("start");
        std::thread::sleep(std::time::Duration::from_millis(100));
        playback.stop();

        assert!(metrics.clock_ticks() >= 3);
        assert_eq!(
            metrics.silent_ticks(),
            metrics.clock_ticks(),
            "a silent source must read as silent on every tick"
        );
    }

    #[test]
    fn linear_resampler_interpolates_when_upsampling() {
        let mut source = SliceSource::new(&[0.0, 0.5, 1.0, 0.5]);
        // step = 0.5 input frames per output frame
        let mut resampler = LinearResampler::new(48_000, 1, 96_000, 1);
        let mut out = [0f32; 5];
        resampler.fill(&mut source, &mut out);
        assert_eq!(out, [0.0, 0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn linear_resampler_replicates_mono_into_stereo() {
        let mut source = SliceSource::new(&[1.0, 2.0, 3.0]);
        let mut resampler = LinearResampler::new(48_000, 1, 48_000, 2);
        let mut out = [0f32; 4];
        resampler.fill(&mut source, &mut out);
        assert_eq!(out, [1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn linear_resampler_passes_silence_through() {
        let mut source = SilentSource;
        let mut resampler = LinearResampler::new(48_000, 2, 48_000, 2);
        let mut out = [1f32; 8];
        resampler.fill(&mut source, &mut out);
        assert!(out.iter().all(|sample| *sample == 0.0));
    }
}
