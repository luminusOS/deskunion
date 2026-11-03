use std::collections::VecDeque;

/// Sums independently clocked capture streams into one wall-clock paced stream.
///
/// A stream is only mixed once it has buffered `prime` samples, so the
/// delivery jitter of a WASAPI event (a few milliseconds either way) does not
/// punch holes into the output. A stream that runs dry drops out until it has
/// buffered again.
pub(super) struct Mixer {
    sources: Vec<Source>,
    prime: usize,
    max_backlog: usize,
    channels: usize,
}

#[derive(Default)]
struct Source {
    samples: VecDeque<f32>,
    primed: bool,
}

impl Mixer {
    /// `prime` and `max_backlog` are in interleaved samples
    pub(super) fn new(sources: usize, prime: usize, max_backlog: usize, channels: usize) -> Self {
        debug_assert!(prime < max_backlog);
        debug_assert!(channels > 0);
        Self {
            sources: (0..sources).map(|_| Source::default()).collect(),
            prime,
            max_backlog,
            channels: channels.max(1),
        }
    }

    /// forgets everything buffered, so the next audio starts a fresh mix
    pub(super) fn clear(&mut self) {
        for source in &mut self.sources {
            *source = Source::default();
        }
    }

    /// forgets buffered audio and priming state of a specific source
    pub(super) fn clear_source(&mut self, source: usize) {
        if let Some(source) = self.sources.get_mut(source) {
            *source = Source::default();
        }
    }

    pub(super) fn push(&mut self, source: usize, data: &[f32]) {
        let (prime, max_backlog, channels) = (self.prime, self.max_backlog, self.channels);
        let source = &mut self.sources[source];
        source.samples.extend(data);
        // a source producing faster than it is drained (clock drift) must
        // not grow latency without bound. Align excess removal to frame
        // boundaries to preserve channel ordering.
        if source.samples.len() > max_backlog {
            let excess = source.samples.len() - prime * 2;
            let excess_aligned = excess - (excess % channels);
            if excess_aligned > 0 {
                source.samples.drain(..excess_aligned);
            }
        }
        if source.samples.len() >= prime {
            source.primed = true;
        }
    }

    /// appends up to `count` mixed samples to `out`; appends nothing and
    /// returns `false` while no stream has audio buffered
    pub(super) fn mix(&mut self, out: &mut Vec<f32>, count: usize) -> bool {
        if !self.sources.iter().any(|source| source.primed) {
            return false;
        }
        let start = out.len();
        out.resize(start + count, 0.0);
        let mixed = &mut out[start..];
        for source in self.sources.iter_mut().filter(|source| source.primed) {
            let take = (count.min(source.samples.len()) / self.channels) * self.channels;
            for (slot, sample) in mixed.iter_mut().zip(source.samples.drain(..take)) {
                *slot += sample;
            }
            if source.samples.is_empty() {
                source.primed = false;
            }
        }
        for sample in mixed {
            *sample = sample.clamp(-1.0, 1.0);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Mixer;

    #[test]
    fn idle_mixer_produces_nothing() {
        let mut mixer = Mixer::new(2, 4, 32, 2);
        let mut out = Vec::new();
        assert!(!mixer.mix(&mut out, 8));
        assert!(out.is_empty());
    }

    #[test]
    fn source_waits_until_primed() {
        let mut mixer = Mixer::new(1, 4, 32, 2);
        let mut out = Vec::new();
        mixer.push(0, &[0.25, 0.25]);
        assert!(!mixer.mix(&mut out, 2));
        mixer.push(0, &[0.25, 0.25]);
        assert!(mixer.mix(&mut out, 2));
        assert_eq!(out, [0.25, 0.25]);
    }

    #[test]
    fn two_sources_are_summed() {
        let mut mixer = Mixer::new(2, 2, 32, 2);
        mixer.push(0, &[0.25, 0.25, 0.25, 0.25]);
        mixer.push(1, &[0.5, 0.5, 0.5, 0.5]);
        let mut out = Vec::new();
        assert!(mixer.mix(&mut out, 4));
        assert_eq!(out, [0.75; 4]);
    }

    #[test]
    fn one_source_plays_while_the_other_is_silent() {
        let mut mixer = Mixer::new(2, 2, 32, 2);
        mixer.push(1, &[0.5; 4]);
        let mut out = Vec::new();
        assert!(mixer.mix(&mut out, 4));
        assert_eq!(out, [0.5; 4]);
    }

    #[test]
    fn short_source_is_padded_with_silence_then_drops_out() {
        let mut mixer = Mixer::new(1, 2, 32, 2);
        mixer.push(0, &[0.5, 0.5]);
        let mut out = Vec::new();
        assert!(mixer.mix(&mut out, 4));
        assert_eq!(out, [0.5, 0.5, 0.0, 0.0]);
        out.clear();
        assert!(!mixer.mix(&mut out, 4));
    }

    #[test]
    fn sum_is_clamped_to_full_scale() {
        let mut mixer = Mixer::new(2, 2, 32, 2);
        mixer.push(0, &[0.9, -0.9]);
        mixer.push(1, &[0.9, -0.9]);
        let mut out = Vec::new();
        assert!(mixer.mix(&mut out, 2));
        assert_eq!(out, [1.0, -1.0]);
    }

    #[test]
    fn clearing_drops_buffered_audio_and_the_primed_state() {
        let mut mixer = Mixer::new(2, 2, 32, 2);
        mixer.push(0, &[0.5; 4]);
        mixer.push(1, &[0.5; 4]);
        mixer.clear();
        let mut out = Vec::new();
        assert!(!mixer.mix(&mut out, 4));
        mixer.push(1, &[0.25, 0.25]);
        assert!(mixer.mix(&mut out, 2));
        assert_eq!(out, [0.25, 0.25]);
    }

    #[test]
    fn clear_source_only_clears_specified_slot() {
        let mut mixer = Mixer::new(2, 2, 32, 2);
        mixer.push(0, &[0.5; 4]);
        mixer.push(1, &[0.25; 4]);
        mixer.clear_source(0);
        let mut out = Vec::new();
        assert!(mixer.mix(&mut out, 4));
        assert_eq!(out, [0.25; 4]);
    }

    #[test]
    fn backlog_beyond_the_limit_is_trimmed_to_the_newest_audio() {
        let mut mixer = Mixer::new(1, 2, 8, 2);
        let ramp: Vec<f32> = (0..12).map(|n| n as f32 / 100.0).collect();
        mixer.push(0, &ramp);
        let mut out = Vec::new();
        assert!(mixer.mix(&mut out, 4));
        assert_eq!(out, [0.08, 0.09, 0.10, 0.11]);
    }

    #[test]
    fn backlog_trimming_preserves_stereo_frame_alignment() {
        let mut mixer = Mixer::new(1, 2, 8, 2);
        // Push 13 samples with distinct L (0.2) and R (0.8) values:
        // [L0, R0, L1, R1, L2, R2, L3, R3, L4, R4, L5, R5, L6]
        let mut interleaved = Vec::new();
        for _ in 0..6 {
            interleaved.push(0.2); // Left
            interleaved.push(0.8); // Right
        }
        interleaved.push(0.2); // 13th sample (odd length)
        mixer.push(0, &interleaved);
        let mut out = Vec::new();
        assert!(mixer.mix(&mut out, 4));
        // Remainder after trimming must start on a Left channel (0.2), not Right (0.8)
        assert_eq!(
            out[0], 0.2,
            "First sample after trim must be an aligned Left channel"
        );
        assert_eq!(
            out[1], 0.8,
            "Second sample after trim must be an aligned Right channel"
        );
    }
}
