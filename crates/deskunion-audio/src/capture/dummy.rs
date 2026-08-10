use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{AudioCapture, CaptureCallback};
use crate::{AudioDevice, AudioError, AudioFormat};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 1;
const CHUNK_MS: u64 = 20;

/// silence-generating backend used in tests and as a last-resort
/// fallback when no real audio device is available. Same role as
/// `input-capture::dummy` / `input-emulation::dummy`.
pub struct DummyCapture {
    stop_tx: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl DummyCapture {
    pub fn new() -> Self {
        Self {
            stop_tx: None,
            join: None,
        }
    }
}

impl Default for DummyCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCapture for DummyCapture {
    fn devices(&self) -> Result<Vec<AudioDevice>, AudioError> {
        Ok(vec![AudioDevice {
            id: "dummy".to_owned(),
            name: "Dummy (silence)".to_owned(),
            is_monitor: false,
            is_default: true,
        }])
    }

    fn start(
        &mut self,
        _device: Option<&str>,
        mut on_data: CaptureCallback,
    ) -> Result<AudioFormat, AudioError> {
        self.stop();
        let (stop_tx, stop_rx) = mpsc::channel();
        let chunk_len = (SAMPLE_RATE as u64 * CHUNK_MS / 1000) as usize * CHANNELS as usize;
        let join = thread::spawn(move || {
            let silence = vec![0.0f32; chunk_len];
            loop {
                on_data(&silence);
                if stop_rx
                    .recv_timeout(Duration::from_millis(CHUNK_MS))
                    .is_ok()
                {
                    return;
                }
            }
        });
        self.stop_tx = Some(stop_tx);
        self.join = Some(join);
        Ok(AudioFormat {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
        })
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
