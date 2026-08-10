//! Opus encode/decode at the wire format (48kHz, 20ms frames), plus a
//! resampler to convert a capture device's native rate to it. See
//! `deskunion/DESKUNION_AUDIO_PLAN.md` §3.2/§3.3.

use rubato::{FastFixedIn, PolynomialDegree, Resampler as _};

use crate::AudioError;

/// wire sample rate — Opus's native rate, avoids a resample at decode
pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
/// samples per channel in one 20ms frame at [`SAMPLE_RATE`]
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize * FRAME_MS as usize) / 1000;
pub const DEFAULT_BITRATE: i32 = 96_000;
/// generous upper bound for one encoded frame; Opus recommends >= 4000
/// bytes of scratch space even though real frames are far smaller
const MAX_PACKET_SIZE: usize = 4000;

fn opus_channels(channels: u16) -> opus::Channels {
    if channels >= 2 {
        opus::Channels::Stereo
    } else {
        opus::Channels::Mono
    }
}

pub struct Encoder {
    inner: opus::Encoder,
    channels: u16,
}

impl Encoder {
    pub fn new(channels: u16, bitrate: i32) -> Result<Self, AudioError> {
        let mut inner = opus::Encoder::new(
            SAMPLE_RATE,
            opus_channels(channels),
            opus::Application::Audio,
        )?;
        inner.set_bitrate(opus::Bitrate::Bits(bitrate))?;
        Ok(Self { inner, channels })
    }

    /// encode exactly one frame: `FRAME_SAMPLES * channels` interleaved
    /// f32 samples at [`SAMPLE_RATE`]. Panics (debug-only) if `pcm` is
    /// the wrong length — callers own framing (see `sender.rs`).
    pub fn encode_frame(&mut self, pcm: &[f32]) -> Result<Vec<u8>, AudioError> {
        debug_assert_eq!(pcm.len(), FRAME_SAMPLES * self.channels as usize);
        Ok(self.inner.encode_vec_float(pcm, MAX_PACKET_SIZE)?)
    }
}

pub struct Decoder {
    inner: opus::Decoder,
    channels: u16,
}

impl Decoder {
    pub fn new(channels: u16) -> Result<Self, AudioError> {
        let inner = opus::Decoder::new(SAMPLE_RATE, opus_channels(channels))?;
        Ok(Self { inner, channels })
    }

    /// decode one frame. Pass an empty `payload` to conceal a lost
    /// packet — Opus's packet-loss concealment synthesizes a plausible
    /// continuation instead of silence.
    pub fn decode_frame(&mut self, payload: &[u8]) -> Result<Vec<f32>, AudioError> {
        let mut out = vec![0f32; FRAME_SAMPLES * self.channels as usize];
        let decoded_frames = self.inner.decode_float(payload, &mut out, false)?;
        out.truncate(decoded_frames * self.channels as usize);
        Ok(out)
    }
}

/// converts interleaved PCM from a capture device's native rate to
/// [`SAMPLE_RATE`]. Buffers partial input internally since callers feed
/// arbitrarily-sized chunks (whatever cpal's callback handed over) but
/// rubato consumes fixed-size chunks.
pub struct Resampler {
    inner: FastFixedIn<f32>,
    channels: usize,
    chunk_size: usize,
    /// initial ratio (`SAMPLE_RATE / input_rate`); the reference point
    /// for absolute drift-correction factors in [`Resampler::set_ratio`]
    ratio: f64,
    /// interleaved input samples not yet forming a full `chunk_size` chunk
    pending: Vec<f32>,
    in_planar: Vec<Vec<f32>>,
}

impl Resampler {
    pub fn new(input_rate: u32, channels: u16) -> Result<Self, AudioError> {
        let channels = channels as usize;
        let ratio = SAMPLE_RATE as f64 / input_rate as f64;
        // 10ms of input per chunk; arbitrary — just needs to be small
        // enough to keep latency low and divide evenly into f32 math
        let chunk_size = (input_rate as usize / 100).max(1);
        let inner = FastFixedIn::new(ratio, 1.1, PolynomialDegree::Cubic, chunk_size, channels)?;
        Ok(Self {
            inner,
            channels,
            chunk_size,
            ratio,
            pending: Vec::new(),
            in_planar: vec![vec![0.0; chunk_size]; channels],
        })
    }

    /// no-op passthrough for the common case where the device is
    /// already at the wire sample rate
    pub fn passthrough(interleaved: &[f32]) -> Vec<f32> {
        interleaved.to_vec()
    }

    /// set the resample ratio to an absolute multiplier of the initial
    /// ratio (e.g. `0.999` = 0.1% fewer output samples per input
    /// sample). Used for clock-drift correction — see
    /// `jitter::DriftController`. Unlike rubato's
    /// `set_resample_ratio_relative`, which multiplies the *original*
    /// ratio on every call, this maps directly onto rubato's absolute
    /// `set_resample_ratio`, so callers can integrate corrections over
    /// time. `max_resample_ratio_relative` at construction (currently
    /// 1.1) bounds how far this can move.
    pub fn set_ratio(&mut self, factor: f64) -> Result<(), AudioError> {
        self.inner.set_resample_ratio(self.ratio * factor, true)?;
        Ok(())
    }

    /// feed interleaved samples at the input rate; returns resampled
    /// interleaved samples at [`SAMPLE_RATE`] for every full chunk
    /// accumulated so far — may be shorter than a full Opus frame, or
    /// empty if not enough input has accumulated yet.
    pub fn push(&mut self, interleaved: &[f32]) -> Result<Vec<f32>, AudioError> {
        self.pending.extend_from_slice(interleaved);
        let mut out = Vec::new();
        let frame_len = self.chunk_size * self.channels;
        while self.pending.len() >= frame_len {
            for (ch, buf) in self.in_planar.iter_mut().enumerate() {
                for (frame, sample) in buf.iter_mut().enumerate() {
                    *sample = self.pending[frame * self.channels + ch];
                }
            }
            let resampled = self.inner.process(&self.in_planar, None)?;
            let frames_out = resampled.first().map(Vec::len).unwrap_or(0);
            out.reserve(frames_out * self.channels);
            for frame in 0..frames_out {
                for channel in &resampled {
                    out.push(channel[frame]);
                }
            }
            self.pending.drain(0..frame_len);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn sine(freq: f32, samples: usize, sample_rate: f32) -> Vec<f32> {
        (0..samples)
            .map(|n| (2.0 * std::f32::consts::PI * freq * n as f32 / sample_rate).sin() * 0.5)
            .collect()
    }

    fn interleave_mono(samples: &[f32]) -> Vec<f32> {
        samples.to_vec()
    }

    #[test]
    fn roundtrip_silence() {
        let mut enc = Encoder::new(1, DEFAULT_BITRATE).expect("encoder");
        let mut dec = Decoder::new(1).expect("decoder");
        let pcm = vec![0.0f32; FRAME_SAMPLES];
        let packet = enc.encode_frame(&pcm).expect("encode");
        let out = dec.decode_frame(&packet).expect("decode");
        assert_eq!(out.len(), FRAME_SAMPLES);
    }

    #[test]
    fn roundtrip_sine_is_close() {
        let mut enc = Encoder::new(1, DEFAULT_BITRATE).expect("encoder");
        let mut dec = Decoder::new(1).expect("decoder");
        let pcm = interleave_mono(&sine(440.0, FRAME_SAMPLES, SAMPLE_RATE as f32));
        let packet = enc.encode_frame(&pcm).expect("encode");
        let out = dec.decode_frame(&packet).expect("decode");
        assert_eq!(out.len(), FRAME_SAMPLES);
        // lossy codec: check energy survived roughly intact rather than
        // sample-exact equality
        let rms_in = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
        let rms_out = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(
            (rms_in - rms_out).abs() < 0.1,
            "rms_in={rms_in} rms_out={rms_out}"
        );
    }

    #[test]
    fn roundtrip_white_noise() {
        let mut enc = Encoder::new(2, DEFAULT_BITRATE).expect("encoder");
        let mut dec = Decoder::new(2).expect("decoder");
        // deterministic pseudo-noise, no external RNG dependency
        let mut state = 0x1234_5678u32;
        let pcm: Vec<f32> = (0..FRAME_SAMPLES * 2)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect();
        let packet = enc.encode_frame(&pcm).expect("encode");
        let out = dec.decode_frame(&packet).expect("decode");
        assert_eq!(out.len(), FRAME_SAMPLES * 2);
    }

    #[test]
    fn plc_fills_a_lost_frame() {
        let mut enc = Encoder::new(1, DEFAULT_BITRATE).expect("encoder");
        let mut dec = Decoder::new(1).expect("decoder");
        let pcm = interleave_mono(&sine(440.0, FRAME_SAMPLES, SAMPLE_RATE as f32));
        let packet = enc.encode_frame(&pcm).expect("encode");

        // decode one real frame so the decoder has state to conceal from
        dec.decode_frame(&packet).expect("decode");
        // simulate a lost packet
        let concealed = dec.decode_frame(&[]).expect("plc decode");
        assert_eq!(concealed.len(), FRAME_SAMPLES);
    }

    #[test]
    fn resampler_produces_roughly_the_expected_frame_count() {
        let mut r = Resampler::new(44_100, 1).expect("resampler");
        let input = vec![0.0f32; 44_100]; // 1 second @ 44.1kHz
        let out = r.push(&input).expect("push");
        // ~1 second @ 48kHz, within a couple percent (fixed chunking
        // means the last partial chunk is buffered, not emitted yet)
        let expected = SAMPLE_RATE as usize;
        assert!(
            out.len() > expected * 95 / 100 && out.len() <= expected,
            "got {} expected ~{}",
            out.len(),
            expected
        );
    }

    #[test]
    fn resampler_passthrough_is_identity() {
        let input = vec![0.1f32, -0.2, 0.3];
        assert_eq!(Resampler::passthrough(&input), input);
    }

    #[test]
    fn set_ratio_takes_absolute_factors_within_the_configured_bound() {
        let mut r = Resampler::new(SAMPLE_RATE, 1).expect("resampler");
        // drift-sized factors around 1.0 are accepted
        r.set_ratio(1.001).expect("within bound");
        r.set_ratio(0.998).expect("within bound");
        // beyond `max_resample_ratio_relative` (1.1) rubato rejects it
        assert!(r.set_ratio(1.5).is_err());
        assert!(r.set_ratio(0.5).is_err());
    }
}
