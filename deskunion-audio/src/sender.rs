//! Capture → resample → encode pipeline. See
//! `deskunion/DESKUNION_AUDIO_PLAN.md` §3.4/§5.4.
//!
//! Threading (§5.4): the capture backend's `on_data` callback is the
//! real-time audio thread — it must never block, allocate, or do I/O.
//! Here it does exactly one thing: push samples into a lock-free ring.
//! A separate, ordinary OS thread drains that ring and does the actual
//! work (resample, Opus encode, invoking `on_frame`), where blocking
//! and allocating are fine.

use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::AudioError;
use crate::capture::{self, AudioCapture};
use crate::codec::{Encoder, FRAME_SAMPLES, Resampler, SAMPLE_RATE};

/// invoked with (seq, ts_ms, opus payload) for each encoded frame, from
/// the encode thread — not the real-time capture callback.
pub type FrameCallback = Box<dyn FnMut(u32, u32, &[u8]) + Send>;

/// generous vs. one 20ms frame — just needs to absorb scheduling
/// jitter between the capture callback and the encode thread
const RING_CAPACITY_SECONDS: f64 = 1.0;
/// how long the encode thread sleeps when the ring is empty, before
/// checking again
const IDLE_SLEEP: Duration = Duration::from_millis(5);

pub struct AudioSender {
    capture: Box<dyn AudioCapture>,
    encode_stop_tx: Option<mpsc::Sender<()>>,
    encode_thread: Option<JoinHandle<()>>,
}

impl AudioSender {
    /// `capture` is not yet started; this method starts it. `channels`
    /// is the wire channel count (what the encoder and, if needed, the
    /// resampler target) — the capture device's own channel count may
    /// differ from it in principle, but in practice devices report
    /// what they are, so mismatches surface as a resample/encode error
    /// rather than being silently handled.
    pub fn start(
        backend: capture::Backend,
        device: Option<&str>,
        channels: u16,
        bitrate: i32,
        mut on_frame: FrameCallback,
    ) -> Result<Self, AudioError> {
        let mut capture = capture::create(backend);

        let ring_capacity = (SAMPLE_RATE as f64 * channels as f64 * RING_CAPACITY_SECONDS) as usize;
        let ring = HeapRb::<f32>::new(ring_capacity.max(1));
        let (mut producer, mut consumer) = ring.split();

        let format = capture.start(
            device,
            Box::new(move |data: &[f32]| {
                let pushed = producer.push_slice(data);
                if pushed < data.len() {
                    // real-time thread: never block/log-and-block, just drop
                }
            }),
        )?;

        let mut resampler = if format.sample_rate == SAMPLE_RATE {
            None
        } else {
            Some(Resampler::new(format.sample_rate, channels)?)
        };
        let mut encoder = Encoder::new(channels, bitrate)?;

        let (encode_stop_tx, encode_stop_rx) = mpsc::channel::<()>();
        let channels_usize = channels as usize;
        let frame_len = FRAME_SAMPLES * channels_usize;

        let encode_thread = thread::spawn(move || {
            let mut pull_buf = vec![0f32; frame_len.max(960)];
            let mut frame_accum: Vec<f32> = Vec::with_capacity(frame_len * 2);
            let mut seq: u32 = 0;
            let started = Instant::now();

            loop {
                if encode_stop_rx.try_recv().is_ok() {
                    break;
                }
                let popped = consumer.pop_slice(&mut pull_buf);
                if popped == 0 {
                    thread::sleep(IDLE_SLEEP);
                    continue;
                }
                let chunk = &pull_buf[..popped];
                let resampled = match &mut resampler {
                    Some(r) => match r.push(chunk) {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("audio resample error: {e}");
                            continue;
                        }
                    },
                    None => chunk.to_vec(),
                };
                frame_accum.extend_from_slice(&resampled);

                while frame_accum.len() >= frame_len {
                    let frame: Vec<f32> = frame_accum.drain(..frame_len).collect();
                    match encoder.encode_frame(&frame) {
                        Ok(payload) => {
                            let ts_ms = started.elapsed().as_millis() as u32;
                            on_frame(seq, ts_ms, &payload);
                            seq = seq.wrapping_add(1);
                        }
                        Err(e) => log::warn!("opus encode error: {e}"),
                    }
                }
            }
        });

        Ok(Self {
            capture,
            encode_stop_tx: Some(encode_stop_tx),
            encode_thread: Some(encode_thread),
        })
    }

    pub fn stop(&mut self) {
        self.capture.stop();
        if let Some(tx) = self.encode_stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.encode_thread.take() {
            let _ = join.join();
        }
    }
}

impl Drop for AudioSender {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod test {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::codec::DEFAULT_BITRATE;

    #[test]
    fn dummy_capture_produces_encoded_frames() {
        let frames: Arc<Mutex<Vec<(u32, u32, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let frames_cb = frames.clone();

        let mut sender = AudioSender::start(
            capture::Backend::Dummy,
            None,
            1,
            DEFAULT_BITRATE,
            Box::new(move |seq, ts_ms, payload| {
                frames_cb
                    .lock()
                    .expect("lock")
                    .push((seq, ts_ms, payload.len()));
            }),
        )
        .expect("start sender");

        // DummyCapture emits 20ms chunks of silence; give the encode
        // thread real wall-clock time to accumulate a full frame.
        thread::sleep(Duration::from_millis(300));
        sender.stop();

        let got = frames.lock().expect("lock");
        assert!(!got.is_empty(), "expected at least one encoded frame");
        // sequence numbers must be contiguous starting at 0
        for (i, (seq, _, len)) in got.iter().enumerate() {
            assert_eq!(*seq, i as u32);
            assert!(*len > 0);
        }
    }
}
