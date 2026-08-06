use std::str::FromStr;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use cpal::DeviceId;
use cpal::device_description::{DeviceType, InterfaceType};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::{AudioCapture, CaptureCallback, is_monitor_name};
use crate::{AudioDevice, AudioError, AudioFormat};

/// Linux (PipeWire monitor nodes appear as regular input devices) and
/// macOS (CoreAudio loopback on >=14.6) capture, plus generic mic input
/// on any OS cpal supports. Runs the stream on a dedicated OS thread it
/// owns for the stream's lifetime, since `cpal::Stream` isn't `Send` and
/// must be built, played, and dropped on the same thread.
pub struct CpalCapture {
    stop_tx: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl CpalCapture {
    pub fn new() -> Self {
        Self {
            stop_tx: None,
            join: None,
        }
    }
}

impl Default for CpalCapture {
    fn default() -> Self {
        Self::new()
    }
}

fn is_monitor_device(device: &cpal::Device, name: &str) -> bool {
    if is_monitor_name(name) {
        return true;
    }
    // the backend-specific id (e.g. PipeWire's `sink_default`,
    // `alsa_output.*`) often carries the signal that the display name
    // doesn't — see `is_monitor_name` for why this matters on PipeWire.
    if device.id().is_ok_and(|id| is_monitor_name(id.id())) {
        return true;
    }
    device
        .description()
        .map(|d| {
            d.device_type() == DeviceType::Virtual || d.interface_type() == InterfaceType::Virtual
        })
        .unwrap_or(false)
}

fn find_device(host: &cpal::Host, id: &str) -> Option<cpal::Device> {
    DeviceId::from_str(id)
        .ok()
        .and_then(|id| host.device_by_id(&id))
}

impl AudioCapture for CpalCapture {
    fn devices(&self) -> Result<Vec<AudioDevice>, AudioError> {
        let host = cpal::default_host();
        let default_id = host.default_input_device().and_then(|d| d.id().ok());
        let mut out = Vec::new();
        for device in host.input_devices()? {
            let Ok(id) = device.id() else {
                continue;
            };
            let name = device.to_string();
            let is_default = default_id.as_ref() == Some(&id);
            let is_monitor = is_monitor_device(&device, &name);
            out.push(AudioDevice {
                id: id.to_string(),
                name,
                is_monitor,
                is_default,
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
            let host = cpal::default_host();
            let device = match &device_id {
                Some(id) => find_device(&host, id),
                None => host.default_input_device(),
            };
            let device = match device {
                Some(d) => d,
                None => {
                    let _ = format_tx.send(Err(AudioError::DeviceNotFound));
                    return;
                }
            };

            let config = match device.default_input_config() {
                Ok(c) => c,
                Err(e) => {
                    let _ = format_tx.send(Err(e.into()));
                    return;
                }
            };
            let format = AudioFormat {
                sample_rate: config.sample_rate(),
                channels: config.channels(),
            };
            let sample_format = config.sample_format();
            let stream_config: cpal::StreamConfig = config.into();
            let err_fn = |e| log::warn!("cpal capture stream error: {e}");

            let stream = match sample_format {
                cpal::SampleFormat::F32 => device.build_input_stream(
                    stream_config,
                    move |data: &[f32], _| on_data(data),
                    err_fn,
                    None,
                ),
                cpal::SampleFormat::I16 => device.build_input_stream(
                    stream_config,
                    move |data: &[i16], _| {
                        let converted: Vec<f32> =
                            data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                        on_data(&converted);
                    },
                    err_fn,
                    None,
                ),
                cpal::SampleFormat::U16 => device.build_input_stream(
                    stream_config,
                    move |data: &[u16], _| {
                        let converted: Vec<f32> = data
                            .iter()
                            .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                            .collect();
                        on_data(&converted);
                    },
                    err_fn,
                    None,
                ),
                sf => {
                    let _ = format_tx.send(Err(AudioError::UnsupportedSampleFormat(sf)));
                    return;
                }
            };

            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    let _ = format_tx.send(Err(e.into()));
                    return;
                }
            };

            if let Err(e) = stream.play() {
                let _ = format_tx.send(Err(e.into()));
                return;
            }

            let _ = format_tx.send(Ok(format));
            // block here for the stream's lifetime; `stream` (and thus
            // the audio callback) is dropped when this thread returns
            let _ = stop_rx.recv();
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
