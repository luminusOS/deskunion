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
}

impl AudioSink for RingSink {
    fn push(&mut self, samples: &[f32]) {
        let pushed = self.producer.push_slice(samples);
        if pushed < samples.len() {
            log::trace!(
                "playback ring full, dropped {} samples",
                samples.len() - pushed
            );
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

            let stream_config = cpal::StreamConfig {
                channels: format.channels,
                sample_rate: format.sample_rate,
                buffer_size: cpal::BufferSize::Default,
            };
            let err_fn = |e| log::warn!("cpal playback stream error: {e}");

            let stream = device.build_output_stream(
                stream_config,
                move |data: &mut [f32], _| {
                    let filled = consumer.pop_slice(data);
                    for sample in &mut data[filled..] {
                        *sample = 0.0;
                    }
                },
                err_fn,
                None,
            );

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
        Ok(Box::new(RingSink { producer }))
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
pub struct DummyPlayback {
    /// shared with whatever sink `start()` last returned — lets tests
    /// assert something was actually pushed without needing a concrete
    /// downcast of the `Box<dyn AudioSink>` `start()` returns.
    samples_received: Arc<Mutex<usize>>,
}

impl DummyPlayback {
    pub fn new() -> Self {
        Self {
            samples_received: Arc::new(Mutex::new(0)),
        }
    }

    pub fn samples_received(&self) -> Arc<Mutex<usize>> {
        self.samples_received.clone()
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
        _format: AudioFormat,
    ) -> Result<Box<dyn AudioSink>, AudioError> {
        Ok(Box::new(DummySink {
            samples_received: self.samples_received.clone(),
        }))
    }

    fn stop(&mut self) {}
}

struct DummySink {
    samples_received: Arc<Mutex<usize>>,
}

impl AudioSink for DummySink {
    fn push(&mut self, samples: &[f32]) {
        *self.samples_received.lock().expect("lock") += samples.len();
    }
}
