//! Jitter buffer → playback pipeline. See
//! `deskunion/DESKUNION_AUDIO_PLAN.md` §3.4.
//!
//! Network arrivals come in from async/service code via [`push_frame`],
//! which just takes a mutex briefly. A dedicated OS thread pops the
//! jitter buffer at a steady one-frame-per-tick cadence and forwards
//! decoded PCM to the playback sink — decoupled from any async runtime
//! so this crate stays runtime-agnostic (see `AGENTS.md`'s crate
//! boundaries).

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
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
    level_bits: Arc<AtomicU32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStats {
    pub latency_ms: u32,
    pub packets_lost: u64,
    pub level: f32,
}

impl AudioReceiver {
    pub fn start(
        backend: playback::Backend,
        device: Option<&str>,
        sample_rate: u32,
        channels: u16,
        target_ms: u32,
    ) -> Result<Self, AudioError> {
        Self::start_with(
            playback::create(backend),
            device,
            sample_rate,
            channels,
            target_ms,
        )
    }

    /// like [`AudioReceiver::start`] but with a caller-provided playback
    /// backend — tests plug in a `DummyPlayback` whose metrics they
    /// keep a handle to.
    pub fn start_with(
        mut playback: Box<dyn AudioPlayback>,
        device: Option<&str>,
        sample_rate: u32,
        channels: u16,
        target_ms: u32,
    ) -> Result<Self, AudioError> {
        // the Opus wire format is fixed at `SAMPLE_RATE` (see codec.rs);
        // a peer announcing anything else is a protocol mismatch we
        // can't honor without a second resampling stage — decode as the
        // wire rate and warn.
        if sample_rate != SAMPLE_RATE {
            log::warn!(
                "peer announced audio at {sample_rate} Hz, but the wire format is {SAMPLE_RATE} Hz; decoding as {SAMPLE_RATE} Hz"
            );
        }
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
        let level_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));

        let tick_thread = thread::spawn({
            let level_bits = level_bits.clone();
            move || tick_loop(jitter_tick, sink, tick_stop_rx, level_bits)
        });

        Ok(Self {
            playback,
            jitter,
            tick_stop_tx: Some(tick_stop_tx),
            tick_thread: Some(tick_thread),
            level_bits,
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

    pub fn stats(&self) -> AudioStats {
        let jitter = self.jitter.lock().expect("jitter lock poisoned");
        AudioStats {
            latency_ms: jitter.occupancy() * FRAME_MS,
            packets_lost: jitter.packets_lost(),
            level: f32::from_bits(self.level_bits.load(Ordering::Relaxed)),
        }
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
    level_bits: Arc<AtomicU32>,
) {
    let tick = Duration::from_millis(FRAME_MS as u64);
    let mut last_lost = 0u64;
    let mut underrun_logs = 0u64;
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let tick_start = Instant::now();

        let (pcm, lost) = {
            let mut jitter = jitter.lock().expect("jitter lock poisoned");
            let pcm = jitter.pop();
            let lost = jitter.packets_lost();
            (pcm, lost)
        };
        // a stall here is otherwise silent: the playback sink just gets
        // fewer/emptier pushes and the stream fades without any error
        if lost > last_lost {
            let concealed = lost - last_lost;
            last_lost = lost;
            underrun_logs += 1;
            if underrun_logs == 1 || underrun_logs.is_multiple_of(50) {
                log::debug!(
                    "audio receiver underrun: concealing missing frame(s) with PLC ({lost} lost so far, +{concealed})"
                );
            }
        }
        match pcm {
            Ok(pcm) => {
                let rms = if pcm.is_empty() {
                    0.0
                } else {
                    (pcm.iter().map(|sample| sample * sample).sum::<f32>() / pcm.len() as f32)
                        .sqrt()
                        .clamp(0.0, 1.0)
                };
                level_bits.store(rms.to_bits(), Ordering::Relaxed);
                sink.push(&pcm)
            }
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
            SAMPLE_RATE,
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
            SAMPLE_RATE,
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
