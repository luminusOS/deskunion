//! Jitter buffer → playback pipeline. See
//! `deskunion/DESKUNION_AUDIO_PLAN.md` §3.4.
//!
//! Network arrivals come in from async/service code via [`push_frame`],
//! which just takes a mutex briefly. A dedicated OS thread pops the
//! jitter buffer at a steady one-frame-per-tick cadence and forwards
//! decoded PCM to the playback sink — decoupled from any async runtime
//! so this crate stays runtime-agnostic (see `AGENTS.md`'s crate
//! boundaries).

use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::codec::{FRAME_MS, SAMPLE_RATE};
use crate::jitter::JitterBuffer;
use crate::playback::{self, AudioPlayback, AudioSink};
use crate::{AudioError, AudioFormat};

pub struct AudioReceiver {
    playback: Box<dyn AudioPlayback>,
    jitter: Arc<Mutex<JitterBuffer>>,
    tick_stop_tx: Option<std::sync::mpsc::Sender<()>>,
    tick_thread: Option<JoinHandle<()>>,
}

impl AudioReceiver {
    pub fn start(
        backend: playback::Backend,
        device: Option<&str>,
        channels: u16,
        target_ms: u32,
    ) -> Result<Self, AudioError> {
        let mut playback = playback::create(backend);
        let sink = playback.start(
            device,
            AudioFormat {
                sample_rate: SAMPLE_RATE,
                channels,
            },
        )?;

        let jitter = Arc::new(Mutex::new(JitterBuffer::new(channels, target_ms)?));
        let jitter_tick = jitter.clone();
        let (tick_stop_tx, tick_stop_rx) = std::sync::mpsc::channel::<()>();

        let tick_thread = thread::spawn(move || tick_loop(jitter_tick, sink, tick_stop_rx));

        Ok(Self {
            playback,
            jitter,
            tick_stop_tx: Some(tick_stop_tx),
            tick_thread: Some(tick_thread),
        })
    }

    /// feed an arrived Opus frame into the jitter buffer. Cheap
    /// (a mutex lock, no I/O) — safe to call from an async task.
    pub fn push_frame(&self, seq: u32, payload: &[u8]) {
        self.jitter
            .lock()
            .expect("jitter lock poisoned")
            .push(seq, payload);
    }

    /// number of arrived-but-not-yet-played frames currently buffered
    pub fn occupancy(&self) -> u32 {
        self.jitter
            .lock()
            .expect("jitter lock poisoned")
            .occupancy()
    }

    /// drop all buffered audio — call on `AudioControl::Stop` and on
    /// reconnect.
    pub fn reset(&self) {
        self.jitter.lock().expect("jitter lock poisoned").reset();
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.tick_stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.tick_thread.take() {
            let _ = join.join();
        }
        self.playback.stop();
    }
}

impl Drop for AudioReceiver {
    fn drop(&mut self) {
        self.stop();
    }
}

fn tick_loop(
    jitter: Arc<Mutex<JitterBuffer>>,
    mut sink: Box<dyn AudioSink>,
    stop_rx: std::sync::mpsc::Receiver<()>,
) {
    let tick = Duration::from_millis(FRAME_MS as u64);
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let tick_start = Instant::now();

        let pcm = jitter.lock().expect("jitter lock poisoned").pop();
        match pcm {
            Ok(pcm) => sink.push(&pcm),
            Err(e) => log::warn!("jitter buffer pop error: {e}"),
        }

        let elapsed = tick_start.elapsed();
        if elapsed < tick {
            thread::sleep(tick - elapsed);
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use super::*;
    use crate::codec::{DEFAULT_BITRATE, Encoder, FRAME_SAMPLES};
    use crate::playback::DummyPlayback;

    #[test]
    fn pushed_frames_reach_playback() {
        // build a few real Opus frames the same way the jitter tests do
        let mut enc = Encoder::new(1, DEFAULT_BITRATE).expect("encoder");
        let frames: Vec<Vec<u8>> = (0..5)
            .map(|i| {
                let pcm: Vec<f32> = (0..FRAME_SAMPLES)
                    .map(|n| {
                        let f = 220.0 + i as f32 * 20.0;
                        (2.0 * std::f32::consts::PI * f * n as f32 / SAMPLE_RATE as f32).sin() * 0.5
                    })
                    .collect();
                enc.encode_frame(&pcm).expect("encode")
            })
            .collect();

        let receiver = AudioReceiver::start(
            playback::Backend::Dummy,
            None,
            1,
            crate::jitter::DEFAULT_TARGET_MS,
        )
        .expect("start receiver");

        for (seq, payload) in frames.iter().enumerate() {
            receiver.push_frame(seq as u32, payload);
        }

        // give the tick thread real wall-clock time to drain the
        // buffer at its 20ms cadence
        thread::sleep(Duration::from_millis(300));

        assert_eq!(
            receiver.occupancy(),
            0,
            "all pushed frames should be popped by now"
        );
    }

    #[test]
    fn reset_drops_buffered_frames() {
        let mut enc = Encoder::new(1, DEFAULT_BITRATE).expect("encoder");
        let pcm = vec![0.0f32; FRAME_SAMPLES];
        let payload = enc.encode_frame(&pcm).expect("encode");

        let receiver = AudioReceiver::start(
            playback::Backend::Dummy,
            None,
            1,
            crate::jitter::DEFAULT_TARGET_MS,
        )
        .expect("start receiver");
        // push far more than the tick thread could drain in one tick,
        // then reset before it gets a chance
        for seq in 0..50 {
            receiver.push_frame(seq, &payload);
        }
        receiver.reset();
        assert_eq!(receiver.occupancy(), 0);
    }

    #[test]
    fn dummy_playback_exposes_samples_received() {
        let dummy = DummyPlayback::new();
        let counter = dummy.samples_received();
        assert_eq!(*counter.lock().expect("lock"), 0);
    }
}
