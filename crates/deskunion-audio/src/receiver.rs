//! Jitter buffer → playback pipeline. See
//! `deskunion/DESKUNION_AUDIO_PLAN.md` §3.4.
//!
//! Network arrivals come in from async/service code via [`push_frame`],
//! which just takes a mutex briefly. The output device pulls the other
//! end: its callback pops the jitter buffer through [`JitterSource`], so
//! playback advances on the device's clock and nothing in between can
//! drain. This keeps the crate runtime-agnostic (see `AGENTS.md`'s crate
//! boundaries) with no thread of its own.
//!
//! An earlier version had a thread pop on `thread::sleep(20ms)` and push
//! into a ring the device drained. `sleep` only ever overshoots, so the
//! ring lost a little on every tick and playback went permanently silent
//! once it ran dry — the "plays the first seconds, then stops" failure.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

use crate::codec::{FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE};
use crate::jitter::JitterBuffer;
use crate::playback::{self, AudioPlayback, AudioSource};
use crate::{AudioError, AudioFormat};

pub struct AudioReceiver {
    playback: Box<dyn AudioPlayback>,
    jitter: Arc<Mutex<JitterBuffer>>,
    level_bits: Arc<AtomicU32>,
    /// device callbacks that had to emit silence because the jitter
    /// lock was held by a network push at that instant
    contended: Arc<AtomicU64>,
}

/// pops the jitter buffer from inside the output device's callback.
///
/// `try_lock` rather than `lock`: a network push holds the mutex for the
/// few microseconds it takes to insert a packet, and blocking the
/// realtime thread on it would risk an xrun. Losing that race costs one
/// block of silence, and the counter makes it visible.
struct JitterSource {
    jitter: Arc<Mutex<JitterBuffer>>,
    /// one decoded frame, refilled as the device consumes it
    frame: Vec<f32>,
    /// how much of `frame` has already been handed to the device
    frame_at: usize,
    level_bits: Arc<AtomicU32>,
    contended: Arc<AtomicU64>,
}

impl AudioSource for JitterSource {
    fn fill(&mut self, out: &mut [f32]) {
        let mut filled = 0;
        while filled < out.len() {
            if self.frame_at == self.frame.len() {
                let popped = match self.jitter.try_lock() {
                    Ok(mut jitter) => jitter.pop_into(&mut self.frame),
                    Err(_) => {
                        self.contended.fetch_add(1, Ordering::Relaxed);
                        out[filled..].fill(0.0);
                        break;
                    }
                };
                if popped.is_err() {
                    // a decode failure is not recoverable within this
                    // callback; `stats()` reports it out of band
                    self.contended.fetch_add(1, Ordering::Relaxed);
                    out[filled..].fill(0.0);
                    break;
                }
                self.frame_at = 0;
            }
            let n = (out.len() - filled).min(self.frame.len() - self.frame_at);
            out[filled..filled + n].copy_from_slice(&self.frame[self.frame_at..self.frame_at + n]);
            self.frame_at += n;
            filled += n;
        }

        let rms = if out.is_empty() {
            0.0
        } else {
            (out.iter().map(|sample| sample * sample).sum::<f32>() / out.len() as f32)
                .sqrt()
                .clamp(0.0, 1.0)
        };
        self.level_bits.store(rms.to_bits(), Ordering::Relaxed);
    }
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
        let jitter = Arc::new(Mutex::new(JitterBuffer::new(channels, target_ms)?));
        let level_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let contended = Arc::new(AtomicU64::new(0));

        playback.start(
            device,
            AudioFormat {
                sample_rate: SAMPLE_RATE,
                channels,
            },
            Box::new(JitterSource {
                jitter: jitter.clone(),
                frame: vec![0.0; FRAME_SAMPLES * channels as usize],
                // start empty so the first callback pops a frame
                frame_at: FRAME_SAMPLES * channels as usize,
                level_bits: level_bits.clone(),
                contended: contended.clone(),
            }),
        )?;

        Ok(Self {
            playback,
            jitter,
            level_bits,
            contended,
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
        self.playback.stop();
    }

    /// false once the playback backend reported a stream error. A dead
    /// cpal stream never calls its data callback again, so the owner
    /// must drop this receiver and start a new one — everything
    /// upstream (jitter buffer, connection, stats) still looks healthy.
    pub fn is_healthy(&self) -> bool {
        !self.playback.failed()
    }

    /// device callbacks that emitted silence because the jitter lock was
    /// contended or a frame failed to decode
    pub fn contended_callbacks(&self) -> u64 {
        self.contended.load(Ordering::Relaxed)
    }
}

impl Drop for AudioReceiver {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod test {
    use std::thread;
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

        // give the dummy device's clock real wall-clock time to pull
        // the buffer dry
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
        // push far more than the device could pull in one block, then
        // reset before it gets a chance
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
