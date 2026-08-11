//! Reorders/deduplicates arriving Opus frames by sequence number and
//! smooths playback timing against sender/receiver clock drift.
//! See `deskunion/DESKUNION_AUDIO_PLAN.md` §3.6.

use std::collections::HashMap;

use crate::AudioError;
use crate::codec::{Decoder, FRAME_MS, FRAME_SAMPLES};

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

/// don't correct for ordinary jitter noise — only once the smoothed
/// occupancy strays this many frames from target
const HYSTERESIS_FRAMES: f64 = 1.0;
/// EMA smoothing factor for the occupancy average (closer to 0 = smoother)
const EMA_ALPHA: f64 = 0.2;

/// how often (in pops) to sample occupancy and drive drift correction —
/// plano §3.6 step 1 wants ~500ms, not every 20ms tick: sampling faster
/// than the buffer can actually respond makes the loop overcorrect.
const SAMPLE_EVERY_N_POPS: u32 = 500 / FRAME_MS;

/// a drift correction applied by [`JitterBuffer::pop`]. The buffer is
/// popped at a fixed one-frame-per-tick cadence, so the only way to
/// steer its occupancy is to consume a frame more (`Drop`) or fewer
/// (`Stretch`) than the cadence — plano §3.6's "drop/duplicate de
/// frame". (An earlier revision nudged a playback resample ratio
/// instead; that ratio never fed back into occupancy, so the buffer
/// ratcheted to the window cap while the shrunken pops starved the
/// playback ring — the "audio stops after the first seconds" bug.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftCorrection {
    /// buffer drifting above target (sender faster than the playback
    /// tick): skip one frame
    Drop,
    /// buffer drifting below target (sender slower): hold one frame,
    /// replayed via concealment
    Stretch,
}

/// Pure control-loop logic for drift correction, decoupled from actual
/// audio/network so it's testable with synthetic occupancy sequences.
/// Returns a correction only once the smoothed occupancy strays outside
/// the hysteresis band around the target.
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

    /// feed a fresh occupancy sample (in frames); returns the correction
    /// to apply, if the smoothed average has drifted outside the
    /// hysteresis band.
    pub fn sample(&mut self, occupancy_frames: u32) -> Option<DriftCorrection> {
        let occ = occupancy_frames as f64;
        self.occupancy_ema = if self.primed {
            EMA_ALPHA * occ + (1.0 - EMA_ALPHA) * self.occupancy_ema
        } else {
            self.primed = true;
            occ
        };
        let error = self.occupancy_ema - self.target_frames;
        if error > HYSTERESIS_FRAMES {
            Some(DriftCorrection::Drop)
        } else if error < -HYSTERESIS_FRAMES {
            Some(DriftCorrection::Stretch)
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
    drift: DriftController,
    packets: HashMap<u32, Vec<u8>>,
    next_seq: Option<u32>,
    startup_frames: u32,
    /// becomes true on the first `pop()`. Before that, `push()` may
    /// still retreat `next_seq` backward to admit reordered arrivals
    /// that came in before we'd picked a starting point; after it,
    /// `next_seq` only ever advances (real duplicate/loss tracking).
    started: bool,
    consecutive_plc: u32,
    /// the previous pop ended in a `Stretch` correction: the next pop
    /// re-tries the same sequence number and conceals it with PLC. That
    /// concealment is intentional, not a network loss — it must not
    /// count toward `packets_lost`/`consecutive_plc`.
    stretch_pending: bool,
    pop_count: u32,
    packets_lost: u64,
    /// frames intentionally skipped by `Drop` drift corrections
    /// (diagnostics; not network loss)
    correction_drops: u64,
}

impl JitterBuffer {
    pub fn new(channels: u16, target_ms: u32) -> Result<Self, AudioError> {
        Ok(Self {
            channels,
            decoder: Decoder::new(channels)?,
            drift: DriftController::new(target_ms),
            packets: HashMap::new(),
            next_seq: None,
            startup_frames: (target_ms / FRAME_MS).max(1),
            started: false,
            consecutive_plc: 0,
            stretch_pending: false,
            pop_count: 0,
            packets_lost: 0,
            correction_drops: 0,
        })
    }

    /// number of arrived-but-not-yet-popped frames currently buffered
    pub fn occupancy(&self) -> u32 {
        self.packets.len() as u32
    }

    pub fn packets_lost(&self) -> u64 {
        self.packets_lost
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
        } else if self.consecutive_plc >= MAX_CONSECUTIVE_PLC && self.packets.is_empty() {
            // We advanced through PLC while the source was paused/stalled.
            // Re-anchor to the resumed stream and prebuffer again instead of
            // classifying every future arrival as permanently late.
            self.next_seq = Some(seq);
            self.packets.insert(seq, payload.to_vec());
            self.started = false;
            self.consecutive_plc = 0;
        }
    }

    /// pop and decode the next frame in sequence order, concealing a
    /// missing frame with Opus PLC. Call this once per [`FRAME_MS`]
    /// playback tick. Every call returns exactly
    /// [`FRAME_SAMPLES`] × channels samples — the playback sink relies
    /// on a constant per-tick sample count to keep its ring fed.
    pub fn pop(&mut self) -> Result<Vec<f32>, AudioError> {
        let mut out = vec![0.0; FRAME_SAMPLES * self.channels as usize];
        self.pop_into(&mut out)?;
        Ok(out)
    }

    /// [`JitterBuffer::pop`] into a caller-owned buffer of exactly
    /// `FRAME_SAMPLES * channels` samples. This is the form the playback
    /// callback uses: it runs on the output device's realtime thread, so
    /// it must not allocate.
    pub fn pop_into(&mut self, out: &mut [f32]) -> Result<(), AudioError> {
        debug_assert_eq!(out.len(), FRAME_SAMPLES * self.channels as usize);
        let Some(mut next) = self.next_seq else {
            out.fill(0.0);
            return Ok(());
        };
        if !self.started && self.occupancy() < self.startup_frames {
            out.fill(0.0);
            return Ok(());
        }
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

        // the flag excuses *this* pop, whatever it turns out to be, and
        // is spent either way: a `Stretch` rewinds onto the slot just
        // popped, and a duplicate of that frame arriving before the next
        // tick (`forward == 0`, so `push` accepts it) is popped here
        // instead. Leaving the flag set would then excuse the next
        // genuine loss, hiding it from `packets_lost` and -- worse --
        // from `consecutive_plc`, which drives the catch-up re-anchor.
        let stretch_hold = std::mem::take(&mut self.stretch_pending);
        match self.packets.remove(&next) {
            Some(payload) => {
                self.consecutive_plc = 0;
                self.decoder.decode_frame_into(&payload, out)?;
            }
            None => {
                // an intentional hold is concealed like any missing
                // frame, but it is not a network loss
                if !stretch_hold {
                    self.consecutive_plc += 1;
                    self.packets_lost += 1;
                }
                self.decoder.decode_frame_into(&[], out)?;
            }
        };
        self.next_seq = Some(next.wrapping_add(1));

        self.pop_count = self.pop_count.wrapping_add(1);
        if self.pop_count.is_multiple_of(SAMPLE_EVERY_N_POPS) {
            match self.drift.sample(self.occupancy()) {
                Some(DriftCorrection::Drop) => {
                    // skip the next sequence number entirely: whether the
                    // frame is already buffered or still in flight, it
                    // never reaches playback — one frame drained
                    let skipped = self.next_seq.expect("next_seq set above");
                    self.packets.remove(&skipped);
                    self.next_seq = Some(skipped.wrapping_add(1));
                    self.correction_drops += 1;
                    log::debug!(
                        "jitter buffer above target; dropped a frame to catch up (occupancy {}, {} drops so far)",
                        self.occupancy(),
                        self.correction_drops
                    );
                }
                Some(DriftCorrection::Stretch) => {
                    // rewind onto the slot just popped: the packet is
                    // gone, so the next tick conceals it with PLC — one
                    // frame of extra latency
                    self.next_seq = Some(next);
                    self.stretch_pending = true;
                    log::debug!(
                        "jitter buffer below target; stretching by one frame (occupancy {})",
                        self.occupancy()
                    );
                }
                None => {}
            }
        }
        Ok(())
    }

    /// drop all buffered state — call on `AudioControl::Stop` and on
    /// reconnect (plano §3.6 step 5).
    pub fn reset(&mut self) {
        self.packets.clear();
        self.next_seq = None;
        self.started = false;
        self.consecutive_plc = 0;
        self.stretch_pending = false;
        self.pop_count = 0;
        self.packets_lost = 0;
        self.correction_drops = 0;
        self.drift.reset();
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::codec::{DEFAULT_BITRATE, Encoder, SAMPLE_RATE};

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
    fn waits_for_target_before_starting_playback() {
        let frames = encode_frames(1, 4);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");

        for (seq, payload) in frames.iter().take(3).enumerate() {
            jb.push(seq as u32, payload);
            let out = jb.pop().expect("startup silence");
            assert!(out.iter().all(|sample| *sample == 0.0));
            assert_eq!(jb.occupancy(), (seq + 1) as u32);
        }

        jb.push(3, &frames[3]);
        jb.pop().expect("playback starts at target occupancy");
        assert_eq!(jb.occupancy(), 3);
    }

    #[test]
    fn recovers_after_a_prolonged_source_stall() {
        let frames = encode_frames(1, 4);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        for (seq, payload) in frames.iter().enumerate() {
            jb.push(seq as u32, payload);
        }
        for _ in 0..4 {
            jb.pop().expect("initial playback");
        }
        for _ in 0..=MAX_CONSECUTIVE_PLC {
            jb.pop().expect("loss concealment during stall");
        }

        let resumed = encode_frames(1, 4);
        for (offset, payload) in resumed.iter().enumerate() {
            jb.push(100 + offset as u32, payload);
        }
        assert_eq!(jb.next_seq, Some(100));
        assert!(!jb.started, "resumed stream must prebuffer again");
        jb.pop().expect("resumed playback");
        assert_eq!(jb.occupancy(), 3);
    }

    #[test]
    fn reports_concealed_packets_as_lost() {
        let frames = encode_frames(1, 4);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        for (seq, payload) in frames.iter().enumerate() {
            jb.push(seq as u32, payload);
        }
        for _ in 0..4 {
            jb.pop().expect("initial playback");
        }
        jb.pop().expect("loss concealment");
        assert_eq!(jb.packets_lost(), 1);
        jb.reset();
        assert_eq!(jb.packets_lost(), 0);
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
        let mut jb = JitterBuffer::new(1, MIN_BUFFER_MS).expect("jitter buffer");
        // seq 1 never arrives
        jb.push(0, &frames[0]);
        jb.push(2, &frames[2]);

        // every pop returns exactly one frame of samples: real audio,
        // PLC concealment for the gap, then real audio again — a
        // missing frame doesn't error out and never shortens the pop
        let f0 = jb.pop().expect("pop seq 0");
        assert_eq!(f0.len(), FRAME_SAMPLES);
        let f1 = jb.pop().expect("pop seq 1 (concealed)");
        assert_eq!(f1.len(), FRAME_SAMPLES);
        let f2 = jb.pop().expect("pop seq 2");
        assert_eq!(f2.len(), FRAME_SAMPLES);
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
        let mut jb = JitterBuffer::new(1, MIN_BUFFER_MS).expect("jitter buffer");
        jb.push(0, &frames[0]);
        jb.pop().expect("pop seq 0");
        // seq 0 arrives again after we've already moved past it
        jb.push(0, &frames[0]);
        assert_eq!(jb.occupancy(), 0);
    }

    #[test]
    fn sequence_number_wraparound_does_not_break_ordering() {
        let frames = encode_frames(1, 3);
        let mut jb = JitterBuffer::new(1, MIN_BUFFER_MS).expect("jitter buffer");
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

    /// field-condition regression (Windows client -> Linux server, audio
    /// played for the first seconds then went permanently silent): the
    /// receiver's 20 ms tick always runs slightly slower than the sender's
    /// device-clocked frame production (`thread::sleep` overshoot / clock
    /// drift). Before the fix, drift correction adjusted a resample ratio
    /// that never fed back into buffer occupancy, so occupancy ratcheted
    /// up until arrivals hit the forward-window guard (dropping every new
    /// frame) while the ratio clamped and each pop returned ~9% fewer
    /// samples — the playback ring starved and the stream went silent.
    #[test]
    fn slow_receiver_drift_stays_bounded_and_full_rate() {
        let frames = encode_frames(1, 64);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        let target = DEFAULT_TARGET_MS / FRAME_MS;

        // virtual clocks: the sender produces a frame every 20 ms while
        // the receiver pops every 20.4 ms (2% slow)
        let mut pushed = 0u32;
        let mut sender_ms = 0f64;
        let mut receiver_ms = 0f64;
        let total_pops = 15_000; // ~5 virtual minutes at the receiver
        for pop in 0..total_pops {
            receiver_ms += 20.4;
            while sender_ms <= receiver_ms {
                jb.push(pushed, &frames[pushed as usize % frames.len()]);
                pushed += 1;
                sender_ms += 20.0;
            }
            let out = jb.pop().expect("pop");
            if pop > 500 {
                assert_eq!(
                    out.len(),
                    FRAME_SAMPLES,
                    "every pop must yield a full frame (playback starves otherwise)"
                );
            }
        }
        assert!(
            jb.occupancy() <= target + 4,
            "occupancy must stay bounded near target, got {}",
            jb.occupancy()
        );
        assert!(
            jb.packets_lost() < total_pops as u64 / 100,
            "packets lost: {}",
            jb.packets_lost()
        );
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
        let consume_rate = 1.0;

        // 100_000 ticks @ 20ms = ~33 minutes simulated, matching the
        // "minutes" timescale the plan itself reasons about for 100ppm
        let mut late_occupancy_sum = 0.0;
        let mut late_occupancy_max = 0.0f64;
        for tick in 0..100_000 {
            occupancy += arrival_rate;
            occupancy -= consume_rate;
            occupancy = occupancy.max(0.0);
            // measure only past the initial transient
            if tick >= 50_000 {
                late_occupancy_sum += occupancy;
                late_occupancy_max = late_occupancy_max.max(occupancy);
            }
            // sample at the same ~500ms cadence `JitterBuffer::pop`
            // uses (see `SAMPLE_EVERY_N_POPS`) — sampling every tick
            // overcorrects faster than the buffer can respond.
            if tick % SAMPLE_EVERY_N_POPS != 0 {
                continue;
            }
            match controller.sample(occupancy as u32) {
                // model the real correction path: `JitterBuffer` skips
                // a frame on Drop and holds one on Stretch, moving the
                // occupancy by a whole frame either way
                Some(DriftCorrection::Drop) => occupancy = (occupancy - 1.0).max(0.0),
                Some(DriftCorrection::Stretch) => occupancy += 1.0,
                None => {}
            }
        }

        let late_mean = late_occupancy_sum / 50_000.0;
        assert!(
            (late_mean - target).abs() < 2.0,
            "mean occupancy={late_mean} target={target}"
        );
        assert!(
            late_occupancy_max < target + 15.0,
            "occupancy must stay bounded, peaked at {late_occupancy_max} (target={target})"
        );
    }

    #[test]
    fn drift_controller_is_quiet_within_hysteresis() {
        let mut controller = DriftController::new(DEFAULT_TARGET_MS);
        let target = DEFAULT_TARGET_MS / FRAME_MS;
        assert!(controller.sample(target).is_none());
        assert!(controller.sample(target).is_none());
    }

    /// mirror image of the slow-receiver regression test above: a
    /// receiver ticking *faster* than the sender produces frames must
    /// hold frames (`Stretch`) instead of draining into permanent PLC.
    #[test]
    fn fast_receiver_drift_stays_bounded_via_stretch() {
        let frames = encode_frames(1, 64);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        let target = DEFAULT_TARGET_MS / FRAME_MS;

        // sender produces a frame every 20 ms; receiver pops every
        // 19.6 ms (2% fast)
        let mut pushed = 0u32;
        let mut sender_ms = 0f64;
        let mut receiver_ms = 0f64;
        let total_pops = 15_000;
        for pop in 0..total_pops {
            receiver_ms += 19.6;
            while sender_ms <= receiver_ms {
                jb.push(pushed, &frames[pushed as usize % frames.len()]);
                pushed += 1;
                sender_ms += 20.0;
            }
            let out = jb.pop().expect("pop");
            if pop > 500 {
                assert_eq!(out.len(), FRAME_SAMPLES);
            }
        }
        assert!(
            jb.occupancy() <= target + 4,
            "occupancy must stay bounded near target, got {}",
            jb.occupancy()
        );
        // stretches are concealed with PLC but must not count as
        // network loss; real losses stay near zero on a lossless link
        assert!(
            jb.packets_lost() < total_pops as u64 / 100,
            "packets lost: {}",
            jb.packets_lost()
        );
    }

    /// a `Stretch` correction replays the just-popped slot via PLC;
    /// that concealment is intentional and must not inflate the
    /// user-facing loss counter
    #[test]
    fn stretch_concealment_is_not_counted_as_loss() {
        let frames = encode_frames(1, 8);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        for (seq, payload) in frames.iter().enumerate() {
            jb.push(seq as u32, payload);
        }
        // drain everything so the buffer reads as "too empty"
        for _ in 0..8 {
            jb.pop().expect("pop");
        }
        let lost_before = jb.packets_lost();
        jb.drift.reset();
        // prime the EMA below the band: target is 4 frames at
        // DEFAULT_TARGET_MS, so an empty buffer is well outside it
        assert_eq!(jb.drift.sample(0), Some(DriftCorrection::Stretch));
        // apply exactly what pop() applies on a Stretch sample
        let next = jb.next_seq.expect("started");
        jb.next_seq = Some(next.wrapping_sub(1));
        jb.stretch_pending = true;
        let out = jb.pop().expect("stretch pop");
        assert_eq!(out.len(), FRAME_SAMPLES);
        assert_eq!(jb.packets_lost(), lost_before, "stretch PLC is not a loss");
    }

    /// a `Stretch` rewinds onto the slot just popped, so a duplicate of
    /// that frame is inside the accept window (`forward == 0`) and gets
    /// popped instead of the expected concealment. The pending flag is
    /// spent on that pop either way — left set, it silently excuses the
    /// next genuine loss.
    #[test]
    fn stretch_flag_does_not_leak_onto_the_next_real_loss() {
        let frames = encode_frames(1, 8);
        let mut jb = JitterBuffer::new(1, DEFAULT_TARGET_MS).expect("jitter buffer");
        for (seq, payload) in frames.iter().enumerate() {
            jb.push(seq as u32, payload);
        }
        for _ in 0..8 {
            jb.pop().expect("pop");
        }
        // apply exactly what pop() applies on a Stretch sample
        let next = jb.next_seq.expect("started");
        let replayed = next.wrapping_sub(1);
        jb.next_seq = Some(replayed);
        jb.stretch_pending = true;

        // ... but the frame turns up again before the next tick, so the
        // stretch pops a real packet rather than concealing
        jb.push(replayed, &frames[replayed as usize]);
        jb.pop().expect("stretch pop found a duplicate");

        // the very next frame is genuinely missing
        let lost_before = jb.packets_lost();
        jb.pop().expect("pop");
        assert_eq!(
            jb.packets_lost(),
            lost_before + 1,
            "a real loss was excused by a stale stretch flag"
        );
    }

    #[test]
    fn drop_correction_skips_exactly_one_frame() {
        // regression guard for the closed-loop drift fix: with the
        // buffer stuck above target, corrections must actually move
        // occupancy — the old resample-ratio nudges never did
        let frames = encode_frames(1, 90);
        let mut jb = JitterBuffer::new(1, MIN_BUFFER_MS).expect("jitter buffer");
        let mut next_push = 0u32;
        // keep occupancy above target + hysteresis so every sample
        // triggers a Drop (target is 1 frame at MIN_BUFFER_MS; the
        // sample is taken after `pop` consumed a frame, hence the
        // extra margin)
        let floor = MIN_BUFFER_MS / FRAME_MS + 3;
        // anchor the sequence counter before the first pop
        while jb.occupancy() < floor {
            jb.push(next_push, &frames[next_push as usize % frames.len()]);
            next_push += 1;
        }
        let first = jb.next_seq.expect("pushed");
        let pops = 3 * SAMPLE_EVERY_N_POPS;
        for _ in 0..pops {
            while jb.occupancy() < floor {
                jb.push(next_push, &frames[next_push as usize % frames.len()]);
                next_push += 1;
            }
            jb.pop().expect("pop");
        }
        let consumed = jb.next_seq.expect("next_seq").wrapping_sub(first);
        // one sequence number per pop, plus one extra per Drop sample
        assert_eq!(consumed, pops + 3);
        assert_eq!(jb.correction_drops, 3);

        jb.reset();
        assert_eq!(jb.correction_drops, 0, "reset clears the counter");
    }
}
