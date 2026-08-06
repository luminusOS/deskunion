//! Reorders/deduplicates arriving Opus frames by sequence number and
//! smooths playback timing against sender/receiver clock drift.
//! See `deskunion/DESKUNION_AUDIO_PLAN.md` §3.6.

use std::collections::HashMap;

use crate::AudioError;
use crate::codec::{Decoder, FRAME_MS, FRAME_SAMPLES, Resampler, SAMPLE_RATE};

pub const DEFAULT_TARGET_MS: u32 = 80;
pub const MIN_BUFFER_MS: u32 = 20;
pub const MAX_BUFFER_MS: u32 = 200;

/// how many frames ahead of `next_seq` we'll buffer before treating an
/// arrival as implausibly far in the future rather than real jitter —
/// guards against unbounded memory growth from a wildly out-of-order
/// or hostile peer
const MAX_WINDOW_FRAMES: u32 = (MAX_BUFFER_MS / FRAME_MS) * 4;

/// give up concealing and jump ahead to whatever's actually arrived
/// after this many consecutive PLC frames (plano §3.6, "drop/duplicate
/// de frame como fallback bruto")
const MAX_CONSECUTIVE_PLC: u32 = MAX_BUFFER_MS / FRAME_MS;

/// ±0.1% nudge per correction step, per plano §3.6
const DRIFT_STEP: f64 = 0.001;
/// don't correct for ordinary jitter noise — only once the smoothed
/// occupancy strays this many frames from target
const HYSTERESIS_FRAMES: f64 = 1.0;
/// EMA smoothing factor for the occupancy average (closer to 0 = smoother)
const EMA_ALPHA: f64 = 0.2;

/// how often (in pops) to sample occupancy and drive drift correction —
/// plano §3.6 step 1 wants ~500ms, not every 20ms tick: sampling faster
/// than the buffer can actually respond makes the loop overcorrect.
const SAMPLE_EVERY_N_POPS: u32 = 500 / FRAME_MS;

/// Pure control-loop logic for drift correction, decoupled from actual
/// audio/network so it's testable with synthetic occupancy sequences.
///
/// Sign convention: a negative return means "buffer's too full, drain
/// it faster" (shrink the playback resample ratio); positive means
/// "buffer's too empty, drain it slower" (grow the ratio) — see
/// [`Resampler::set_ratio_relative`].
pub struct DriftController {
    target_frames: f64,
    occupancy_ema: f64,
    primed: bool,
}

impl DriftController {
    pub fn new(target_ms: u32) -> Self {
        Self {
            target_frames: target_ms as f64 / FRAME_MS as f64,
            occupancy_ema: 0.0,
            primed: false,
        }
    }

    /// feed a fresh occupancy sample (in frames); returns a relative
    /// ratio adjustment to apply to the playback resampler, if the
    /// smoothed average has drifted outside the hysteresis band.
    pub fn sample(&mut self, occupancy_frames: u32) -> Option<f64> {
        let occ = occupancy_frames as f64;
        self.occupancy_ema = if self.primed {
            EMA_ALPHA * occ + (1.0 - EMA_ALPHA) * self.occupancy_ema
        } else {
            self.primed = true;
            occ
        };
        let error = self.occupancy_ema - self.target_frames;
        if error > HYSTERESIS_FRAMES {
            Some(-DRIFT_STEP)
        } else if error < -HYSTERESIS_FRAMES {
            Some(DRIFT_STEP)
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        self.occupancy_ema = 0.0;
        self.primed = false;
    }
}

/// Receive-side jitter buffer: takes arriving Opus frames in whatever
/// order/timing the network delivers them, and pops them back out at a
/// steady one-frame-per-tick rate with reordering, loss concealment,
/// duplicate rejection, and drift-corrected playback speed.
pub struct JitterBuffer {
    channels: u16,
    decoder: Decoder,
    drift_resampler: Resampler,
    drift: DriftController,
    packets: HashMap<u32, Vec<u8>>,
    next_seq: Option<u32>,
    /// becomes true on the first `pop()`. Before that, `push()` may
    /// still retreat `next_seq` backward to admit reordered arrivals
    /// that came in before we'd picked a starting point; after it,
    /// `next_seq` only ever advances (real duplicate/loss tracking).
    started: bool,
    consecutive_plc: u32,
    pop_count: u32,
}

impl JitterBuffer {
    pub fn new(channels: u16, target_ms: u32) -> Result<Self, AudioError> {
        Ok(Self {
            channels,
            decoder: Decoder::new(channels)?,
            // input_rate == SAMPLE_RATE: starts at ratio 1.0, only
            // moved away from it by drift correction
            drift_resampler: Resampler::new(SAMPLE_RATE, channels)?,
            drift: DriftController::new(target_ms),
            packets: HashMap::new(),
            next_seq: None,
            started: false,
            consecutive_plc: 0,
            pop_count: 0,
        })
    }

    /// number of arrived-but-not-yet-popped frames currently buffered
    pub fn occupancy(&self) -> u32 {
        self.packets.len() as u32
    }

    /// insert an arrived Opus frame. Duplicates and frames already
    /// popped, or implausibly far in the future, are dropped.
    pub fn push(&mut self, seq: u32, payload: &[u8]) {
        let Some(next) = self.next_seq else {
            self.next_seq = Some(seq);
            self.packets.insert(seq, payload.to_vec());
            return;
        };
        // wrapping distance from the next-expected seq: small values
        // are real jitter (arrived early), huge values (near u32::MAX)
        // are actually behind us post-wraparound — i.e. a duplicate or
        // a frame we already popped.
        let forward = seq.wrapping_sub(next);
        if !self.started {
            // haven't started popping yet, so `next` is only a
            // provisional anchor from whichever frame happened to
            // arrive first — it may not be the earliest. A small
            // *backward* distance here means a genuinely earlier frame
            // arrived late; retreat the anchor to include it instead
            // of wrongly treating it as "far in the future".
            let backward = next.wrapping_sub(seq);
            if backward != 0 && backward <= MAX_WINDOW_FRAMES {
                self.next_seq = Some(seq);
            }
            if forward <= MAX_WINDOW_FRAMES || backward <= MAX_WINDOW_FRAMES {
                self.packets.entry(seq).or_insert_with(|| payload.to_vec());
            }
        } else if forward <= MAX_WINDOW_FRAMES {
            self.packets.entry(seq).or_insert_with(|| payload.to_vec());
        }
    }

    /// pop and decode the next frame in sequence order, concealing a
    /// missing frame with Opus PLC, then applying the current drift
    /// correction. Call this once per [`FRAME_MS`] playback tick.
    pub fn pop(&mut self) -> Result<Vec<f32>, AudioError> {
        let Some(mut next) = self.next_seq else {
            return Ok(vec![0.0; FRAME_SAMPLES * self.channels as usize]);
        };
        self.started = true;

        if self.consecutive_plc > MAX_CONSECUTIVE_PLC {
            if let Some(&catch_up) = self
                .packets
                .keys()
                .min_by_key(|&&seq| seq.wrapping_sub(next))
            {
                next = catch_up;
                self.consecutive_plc = 0;
            }
        }

        let pcm = match self.packets.remove(&next) {
            Some(payload) => {
                self.consecutive_plc = 0;
                self.decoder.decode_frame(&payload)?
            }
            None => {
                self.consecutive_plc += 1;
                self.decoder.decode_frame(&[])?
            }
        };
        self.next_seq = Some(next.wrapping_add(1));

        self.pop_count = self.pop_count.wrapping_add(1);
        if self.pop_count.is_multiple_of(SAMPLE_EVERY_N_POPS) {
            if let Some(adjust) = self.drift.sample(self.occupancy()) {
                // best-effort: a rejected ratio (outside the resampler's
                // configured bound) means real drift far beyond what a
                // ±0.1% nudge is meant to handle — leave the ratio as
                // it is rather than tearing down the whole stream over
                // a cosmetic correction failing.
                let _ = self.drift_resampler.set_ratio_relative(adjust);
            }
        }
        self.drift_resampler.push(&pcm)
    }

    /// drop all buffered state — call on `AudioControl::Stop` and on
    /// reconnect (plano §3.6 step 5).
    pub fn reset(&mut self) {
        self.packets.clear();
        self.next_seq = None;
        self.started = false;
        self.consecutive_plc = 0;
        self.pop_count = 0;
        self.drift.reset();
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::codec::{DEFAULT_BITRATE, Encoder};

    fn encode_frames(channels: u16, count: u32) -> Vec<Vec<u8>> {
        let mut enc = Encoder::new(channels, DEFAULT_BITRATE).expect("encoder");
        (0..count)
            .map(|i| {
                let pcm: Vec<f32> = (0..FRAME_SAMPLES * channels as usize)
                    .map(|n| {
                        let f = 200.0 + i as f32 * 10.0;
                        (2.0 * std::f32::consts::PI * f * n as f32 / SAMPLE_RATE as f32).sin() * 0.5
                    })
                    .collect();
                enc.encode_frame(&pcm).expect("encode")
            })
            .collect()
    }

    #[test]
    fn in_order_frames_pop_in_order() {
        let frames = encode_frames(1, 5);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        for (seq, payload) in frames.iter().enumerate() {
            jb.push(seq as u32, payload);
        }
        for _ in 0..frames.len() {
            let out = jb.pop().expect("pop");
            assert!(!out.is_empty());
        }
        assert_eq!(jb.occupancy(), 0);
    }

    #[test]
    fn reordered_frames_still_pop_in_sequence_order() {
        let frames = encode_frames(1, 4);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        // push out of order: 2, 0, 3, 1
        for &seq in &[2u32, 0, 3, 1] {
            jb.push(seq, &frames[seq as usize]);
        }
        assert_eq!(jb.occupancy(), 4);
        for _ in 0..4 {
            jb.pop().expect("pop");
        }
        assert_eq!(jb.occupancy(), 0);
    }

    #[test]
    fn missing_frame_is_concealed_not_fatal() {
        let frames = encode_frames(1, 3);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        // seq 1 never arrives
        jb.push(0, &frames[0]);
        jb.push(2, &frames[2]);

        // `pop()`'s output passes through the drift resampler, which
        // buffers in its own chunks — output length isn't pinned to
        // FRAME_SAMPLES per call (e.g. interpolator warm-up on the
        // first call), just close to it. The point of this test is
        // that a missing frame doesn't error out.
        let f0 = jb.pop().expect("pop seq 0");
        assert!(!f0.is_empty());
        // seq 1 is missing: PLC still returns usable audio, no error
        let f1 = jb.pop().expect("pop seq 1 (concealed)");
        assert!(!f1.is_empty());
        let f2 = jb.pop().expect("pop seq 2");
        assert!(!f2.is_empty());
    }

    #[test]
    fn duplicate_frame_is_ignored() {
        let frames = encode_frames(1, 1);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        jb.push(0, &frames[0]);
        jb.push(0, &frames[0]); // duplicate
        assert_eq!(jb.occupancy(), 1);
    }

    #[test]
    fn late_arrival_after_pop_is_dropped_not_reinserted() {
        let frames = encode_frames(1, 2);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        jb.push(0, &frames[0]);
        jb.pop().expect("pop seq 0");
        // seq 0 arrives again after we've already moved past it
        jb.push(0, &frames[0]);
        assert_eq!(jb.occupancy(), 0);
    }

    #[test]
    fn sequence_number_wraparound_does_not_break_ordering() {
        let frames = encode_frames(1, 3);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        // start right before u32::MAX wraps to 0
        jb.push(u32::MAX - 1, &frames[0]);
        jb.push(u32::MAX, &frames[1]);
        jb.push(0, &frames[2]);
        assert_eq!(jb.occupancy(), 3);
        for _ in 0..3 {
            let out = jb.pop().expect("pop across wraparound");
            assert!(!out.is_empty());
        }
        assert_eq!(jb.next_seq, Some(1));
    }

    #[test]
    fn reset_clears_all_state() {
        let frames = encode_frames(1, 2);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        jb.push(0, &frames[0]);
        jb.push(1, &frames[1]);
        jb.reset();
        assert_eq!(jb.occupancy(), 0);
        assert!(jb.next_seq.is_none());
    }

    #[test]
    fn drift_controller_converges_to_target_with_synthetic_clock_drift() {
        // model a sender clock running faster than the receiver's
        // nominal tick rate by a realistic amount — plano §3.6 cites
        // 4-100ppm as the range worth caring about; 100ppm is its
        // "underrun in minutes if uncorrected" worst case.
        let mut controller = DriftController::new(DEFAULT_TARGET_MS);
        let target = DEFAULT_TARGET_MS as f64 / FRAME_MS as f64;
        let mut occupancy = target;
        let arrival_rate = 1.0001; // 100ppm fast
        let mut consume_rate: f64 = 1.0;

        // 100_000 ticks @ 20ms = ~33 minutes simulated, matching the
        // "minutes" timescale the plan itself reasons about for 100ppm
        for tick in 0..100_000 {
            occupancy += arrival_rate;
            occupancy -= consume_rate;
            occupancy = occupancy.max(0.0);
            // sample at the same ~500ms cadence `JitterBuffer::pop`
            // uses (see `SAMPLE_EVERY_N_POPS`) — sampling every tick
            // overcorrects faster than the buffer can respond.
            if tick % SAMPLE_EVERY_N_POPS != 0 {
                continue;
            }
            if let Some(adjust) = controller.sample(occupancy as u32) {
                // negative adjust ("too full") speeds consumption up;
                // positive ("too empty") slows it down — see
                // `DriftController`'s doc comment for the convention.
                // Clamped to the same [0.909, 1.1] band the real
                // resampler enforces (plano §3.3's cpal/rubato setup).
                consume_rate = (consume_rate * (1.0 - adjust)).clamp(0.909, 1.1);
            }
        }

        assert!(
            (occupancy - target).abs() < 5.0,
            "occupancy={occupancy} target={target}"
        );
    }

    #[test]
    fn drift_controller_is_quiet_within_hysteresis() {
        let mut controller = DriftController::new(DEFAULT_TARGET_MS);
        let target = DEFAULT_TARGET_MS / FRAME_MS;
        assert!(controller.sample(target).is_none());
        assert!(controller.sample(target).is_none());
    }
}
