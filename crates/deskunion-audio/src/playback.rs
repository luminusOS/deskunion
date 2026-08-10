use std::str::FromStr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::DeviceId;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::{AudioDevice, AudioError, AudioFormat};

/// generous vs. the ~80ms jitter buffer upstream (§3.6 of the audio
/// plan) — this ring only has to absorb scheduling jitter between the
/// decode thread and the cpal output callback, not network jitter.
const RING_CAPACITY_SECONDS: f64 = 0.5;

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
    /// default) and return a sink to push decoded samples into. The
    /// sink is lock-free and safe to push from any thread; on underrun
    /// the stream plays silence rather than blocking.
    fn start(
        &mut self,
        device: Option<&str>,
        format: AudioFormat,
    ) -> Result<Box<dyn AudioSink>, AudioError>;

    /// stop the stream, if running. Safe to call when not started.
    fn stop(&mut self);
}

pub trait AudioSink: Send {
    /// enqueue interleaved samples for playback. Never blocks; excess
    /// samples beyond the ring capacity are dropped.
    fn push(&mut self, samples: &[f32]);
}

struct RingSink {
    producer: ringbuf::HeapProd<f32>,
    /// number of pushes that couldn't fit everything; rate-limits the
    /// overflow log (overflows fire per playback tick when the decode
    /// thread outpaces the output)
    overflow_count: u64,
}

impl AudioSink for RingSink {
    fn push(&mut self, samples: &[f32]) {
        let pushed = self.producer.push_slice(samples);
        if pushed < samples.len() {
            self.overflow_count += 1;
            if self.overflow_count == 1 || self.overflow_count.is_multiple_of(100) {
                log::debug!(
                    "playback ring full, dropped {} samples ({} overflows so far)",
                    samples.len() - pushed,
                    self.overflow_count
                );
            }
        }
    }
}

/// output on any OS cpal supports. Like `CpalCapture`, owns the stream
/// on a dedicated thread since `cpal::Stream` isn't `Send`.
pub struct CpalPlayback {
    stop_tx: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl CpalPlayback {
    pub fn new() -> Self {
        Self {
            stop_tx: None,
            join: None,
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
/// the cpal callback, so it never blocks; on underrun it emits silence.
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

    fn fill(&mut self, consumer: &mut ringbuf::HeapCons<f32>, out: &mut [f32]) {
        let mut filled = 0;
        while filled + self.out_channels <= out.len() {
            // interpolation reads input frames floor(pos) and
            // floor(pos)+1 — top up until both are available
            while (self.buf.len() / self.in_channels) as f64 <= self.pos + 1.0 {
                let mut tmp = [0f32; 8192];
                // the ring only ever holds whole input frames
                let pulled = consumer.pop_slice(&mut tmp);
                if pulled == 0 {
                    // underrun: keep the buffered samples for the next
                    // callback (they're the next in sequence, not stale)
                    // and play silence for now
                    for sample in &mut out[filled..] {
                        *sample = 0.0;
                    }
                    return;
                }
                self.buf.extend(&tmp[..pulled]);
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
    ) -> Result<Box<dyn AudioSink>, AudioError> {
        self.stop();

        let capacity =
            (format.sample_rate as f64 * format.channels as f64 * RING_CAPACITY_SECONDS) as usize;
        let ring = HeapRb::<f32>::new(capacity.max(1));
        let (producer, mut consumer) = ring.split();

        let device_id = device.map(str::to_owned);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AudioError>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

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

            let err_fn = |e| log::error!("audio playback stream error: {e}");

            let stream =
                if config.channels == format.channels && config.sample_rate == format.sample_rate {
                    device.build_output_stream(
                        config,
                        move |data: &mut [f32], _| {
                            let filled = consumer.pop_slice(data);
                            for sample in &mut data[filled..] {
                                *sample = 0.0;
                            }
                        },
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
                        move |data: &mut [f32], _| resampler.fill(&mut consumer, data),
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
        Ok(Box::new(RingSink {
            producer,
            overflow_count: 0,
        }))
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

/// discards pushed samples; used in tests and as a last-resort fallback
/// with no audio hardware. Same role as `capture::dummy::DummyCapture`.
///
/// A clock thread drains the virtual playback buffer at the format's
/// nominal rate, like a real output device would: tests can detect a
/// starving pipeline (e.g. pops returning fewer samples than the tick
/// consumes) via [`DummyMetrics::underrun_ticks`] instead of only
/// counting what was pushed.
pub struct DummyPlayback {
    metrics: DummyMetrics,
    clock_stop_tx: Option<mpsc::Sender<()>>,
    clock_thread: Option<JoinHandle<()>>,
}

/// live counters of a running [`DummyPlayback`] stream. Cheap to clone;
/// all clones point at the same counters.
#[derive(Clone, Default)]
pub struct DummyMetrics {
    samples_received: Arc<Mutex<usize>>,
    /// samples pushed but not yet "played" by the virtual clock
    queued_samples: Arc<std::sync::atomic::AtomicUsize>,
    /// clock ticks that found the virtual buffer empty (playback would
    /// have output silence here on real hardware)
    underrun_ticks: Arc<std::sync::atomic::AtomicU64>,
    /// total clock ticks so far
    clock_ticks: Arc<std::sync::atomic::AtomicU64>,
}

impl DummyMetrics {
    pub fn samples_received(&self) -> Arc<Mutex<usize>> {
        self.samples_received.clone()
    }

    pub fn underrun_ticks(&self) -> u64 {
        self.underrun_ticks
            .load(std::sync::atomic::Ordering::Relaxed)
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
    ) -> Result<Box<dyn AudioSink>, AudioError> {
        self.stop();

        // fresh stream: reset the clock counters (samples_received
        // stays cumulative — tests hold that handle across restarts)
        use std::sync::atomic::Ordering::Relaxed;
        self.metrics.queued_samples.store(0, Relaxed);
        self.metrics.underrun_ticks.store(0, Relaxed);
        self.metrics.clock_ticks.store(0, Relaxed);

        // virtual output device clock: drains the queue at the nominal
        // rate and records every tick that would have underrun
        let samples_per_tick =
            (format.sample_rate as u64 * DUMMY_CLOCK_MS / 1000) as usize * format.channels as usize;
        let metrics = self.metrics.clone();
        let (stop_tx, stop_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            loop {
                if stop_rx
                    .recv_timeout(std::time::Duration::from_millis(DUMMY_CLOCK_MS))
                    .is_ok()
                {
                    return;
                }
                let queued = metrics
                    .queued_samples
                    .load(std::sync::atomic::Ordering::Relaxed);
                if queued >= samples_per_tick {
                    metrics.queued_samples.store(
                        queued - samples_per_tick,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                } else {
                    metrics
                        .queued_samples
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                    metrics
                        .underrun_ticks
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                metrics
                    .clock_ticks
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        self.clock_stop_tx = Some(stop_tx);
        self.clock_thread = Some(join);

        Ok(Box::new(DummySink {
            metrics: self.metrics.clone(),
        }))
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

struct DummySink {
    metrics: DummyMetrics,
}

impl AudioSink for DummySink {
    fn push(&mut self, samples: &[f32]) {
        *self.metrics.samples_received.lock().expect("lock") += samples.len();
        self.metrics
            .queued_samples
            .fetch_add(samples.len(), std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn dummy_playback_counts_underruns_when_starved() {
        let mut playback = DummyPlayback::new();
        let metrics = playback.metrics();
        let mut sink = playback
            .start(
                None,
                AudioFormat {
                    sample_rate: 48_000,
                    channels: 1,
                },
            )
            .expect("start");
        // feed less than the clock consumes: 10ms of audio every ~30ms
        for _ in 0..5 {
            sink.push(&[0.0f32; 480]);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        playback.stop();
        assert!(metrics.clock_ticks() >= 3);
        assert!(
            metrics.underrun_ticks() > 0,
            "a starved sink must record underruns"
        );
    }

    #[test]
    fn dummy_playback_keeps_up_when_fed() {
        let mut playback = DummyPlayback::new();
        let metrics = playback.metrics();
        let mut sink = playback
            .start(
                None,
                AudioFormat {
                    sample_rate: 48_000,
                    channels: 1,
                },
            )
            .expect("start");
        // prebuffer, then feed exactly the consumed rate (20ms of
        // audio every 20ms, like the receiver's tick thread)
        sink.push(&[0.0f32; 480 * 8]);
        for _ in 0..10 {
            sink.push(&[0.0f32; 960]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        playback.stop();
        assert_eq!(metrics.underrun_ticks(), 0, "a fed sink must not underrun");
    }

    #[test]
    fn linear_resampler_interpolates_when_upsampling() {
        let ring = HeapRb::<f32>::new(64);
        let (mut producer, mut consumer) = ring.split();
        producer.push_slice(&[0.0, 0.5, 1.0, 0.5]);
        // step = 0.5 input frames per output frame
        let mut resampler = LinearResampler::new(48_000, 1, 96_000, 1);
        let mut out = [0f32; 5];
        resampler.fill(&mut consumer, &mut out);
        assert_eq!(out, [0.0, 0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn linear_resampler_replicates_mono_into_stereo() {
        let ring = HeapRb::<f32>::new(64);
        let (mut producer, mut consumer) = ring.split();
        producer.push_slice(&[1.0, 2.0, 3.0]);
        let mut resampler = LinearResampler::new(48_000, 1, 48_000, 2);
        let mut out = [0f32; 4];
        resampler.fill(&mut consumer, &mut out);
        assert_eq!(out, [1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn linear_resampler_plays_silence_on_underrun() {
        let ring = HeapRb::<f32>::new(64);
        let (_producer, mut consumer) = ring.split();
        let mut resampler = LinearResampler::new(48_000, 2, 48_000, 2);
        let mut out = [1f32; 8];
        resampler.fill(&mut consumer, &mut out);
        assert!(out.iter().all(|sample| *sample == 0.0));
    }
}
