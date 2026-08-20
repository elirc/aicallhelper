//! Pure conversion from whatever the render device plays (any rate, any
//! channel count, f32/i16/u16) to the 16 kHz mono i16 Deepgram is opened at.
//!
//! Everything here is deterministic and device-free so it can be tested
//! exhaustively; the capture layer only wires it up.

use crate::audio::SAMPLE_RATE;

/// i16 → f32 divides by 32768 (a power of two, so the division is exact in
/// f32) rather than 32767: with the symmetric 32767 scale a 16 kHz i16 stream
/// no longer round-trips bit-exactly through the pipeline.
pub fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

/// u16 PCM puts silence at 32768, not 0. Subtracting the midpoint before
/// scaling keeps silence at 0.0; skipping it adds a full-scale DC offset that
/// pegs the level meter and saturates the resampler output.
pub fn u16_to_f32(s: u16) -> f32 {
    (s as f32 - 32768.0) / 32768.0
}

/// Slice variants append into a caller-owned buffer so the realtime capture
/// callback can reuse one allocation for its whole lifetime.
pub fn extend_i16_to_f32(src: &[i16], dst: &mut Vec<f32>) {
    dst.extend(src.iter().map(|&s| i16_to_f32(s)));
}

pub fn extend_u16_to_f32(src: &[u16], dst: &mut Vec<f32>) {
    dst.extend(src.iter().map(|&s| u16_to_f32(s)));
}

/// f32 → i16 with clamping: float audio may legally exceed ±1.0 (Windows
/// APOs, per-app boosts, limiters), and a wrapping cast would turn a slightly
/// hot sample into a full-scale polarity flip that sounds like broken
/// hardware.
fn f32_to_i16(v: f32) -> i16 {
    (v * 32768.0).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

/// Streaming linear-interpolation resampler with channel downmix.
///
/// Feed it interleaved f32 in arbitrary chunk sizes; it produces exactly the
/// same output as if the whole signal had arrived in one buffer. That
/// property is the entire design: per-buffer resampling that resets or rounds
/// its position accumulates drift (measurably out of sync within minutes
/// against Deepgram's clock) and clicks at every buffer boundary.
pub struct Resampler {
    src_rate: u32,
    src_channels: u16,
    /// Integer part of the read position, indexed into the conceptual stream
    /// `[carry, this push's mono samples...]`.
    idx: usize,
    /// Fractional part of the read position as an exact integer numerator over
    /// SAMPLE_RATE. Kept in integers, and carried across calls, so the
    /// position never drifts no matter how the input is chunked — floating
    /// point accumulation loses a little on every buffer and the loss only
    /// grows.
    frac_num: u32,
    /// Last mono sample of the previous push, so an interpolation window that
    /// straddles a buffer boundary still has both endpoints instead of
    /// snapping to the nearest sample (an audible click, every buffer).
    carry: Option<f32>,
}

impl Resampler {
    pub fn new(src_rate: u32, src_channels: u16) -> Self {
        Self { src_rate, src_channels, idx: 0, frac_num: 0, carry: None }
    }

    /// Feed interleaved f32 samples; appends 16 kHz mono i16 to `out`.
    pub fn push(&mut self, interleaved: &[f32], out: &mut Vec<i16>) {
        // A zero rate or channel count can only come from a misreported device
        // format; emitting nothing beats dividing by zero on a realtime thread.
        if self.src_rate == 0 || self.src_channels == 0 {
            return;
        }
        let ch = self.src_channels as usize;
        // cpal delivers whole frames; a ragged tail would be a backend bug, so
        // it is dropped rather than misread as a rotated channel order.
        let n = interleaved.len() / ch;
        if n == 0 {
            return;
        }

        // Fast path: already the target format, so skip the position machinery
        // entirely — a straight convert is both cheaper and bit-exact.
        if self.src_rate == SAMPLE_RATE && ch == 1 {
            out.reserve(n);
            out.extend(interleaved.iter().map(|&v| f32_to_i16(v)));
            return;
        }

        let carry = self.carry;
        let carry_len = usize::from(carry.is_some());
        let len = n + carry_len;
        // Downmix: average the SPEECH channels, not all of them. In the
        // WAVEFORMATEXTENSIBLE channel order the first three are FL, FR, FC —
        // where calls and dialog live. On a 5.1/7.1 device an equal average
        // over all channels divides the voice by 6 or 8 (LFE and surrounds
        // contribute near-silence), quiet enough to cost transcription
        // accuracy. Mono/stereo keep the plain average — conference apps often
        // pan the far end mostly into one channel, so taking channel 0 alone
        // would silently drop or halve the person we are transcribing.
        // Computed on demand (each source sample is read at most twice) so no
        // intermediate mono buffer is allocated.
        let used_ch = ch.min(3);
        let inv_ch = 1.0 / used_ch as f32;
        let mono = |i: usize| -> f32 {
            if i < carry_len {
                carry.unwrap()
            } else {
                let base = (i - carry_len) * ch;
                let mut sum = 0.0f32;
                for c in 0..used_ch {
                    sum += interleaved[base + c];
                }
                sum * inv_ch
            }
        };

        loop {
            // At an exact integer position only one sample is needed; at a
            // fractional position both interpolation endpoints must exist.
            // When the right endpoint is still in a future buffer we stop and
            // let `carry` bridge the gap on the next push.
            let ready = if self.frac_num == 0 { self.idx < len } else { self.idx + 1 < len };
            if !ready {
                break;
            }
            let s0 = mono(self.idx);
            let v = if self.frac_num == 0 {
                s0
            } else {
                let s1 = mono(self.idx + 1);
                s0 + (s1 - s0) * (self.frac_num as f32 / SAMPLE_RATE as f32)
            };
            out.push(f32_to_i16(v));
            // Exact rational advance by src_rate/SAMPLE_RATE source samples.
            self.frac_num += self.src_rate;
            self.idx += (self.frac_num / SAMPLE_RATE) as usize;
            self.frac_num %= SAMPLE_RATE;
        }

        // Re-anchor so the last mono sample of this push becomes index 0 of
        // the next. The loop's exit condition guarantees idx >= len - 1 here.
        self.carry = Some(mono(len - 1));
        self.idx -= len - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, amp: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32)
            .collect()
    }

    #[test]
    fn stereo_48k_yields_one_third_the_frames_and_averages_channels() {
        // Constant L=0.25 / R=0.75: any output sample that is not exactly the
        // average proves either a dropped channel or a broken downmix.
        let mut src = Vec::with_capacity(48_000 * 2);
        for _ in 0..48_000 {
            src.push(0.25);
            src.push(0.75);
        }
        let mut out = Vec::new();
        Resampler::new(48_000, 2).push(&src, &mut out);
        assert!((out.len() as i64 - 16_000).abs() <= 1, "got {}", out.len());
        assert!(out.iter().all(|&s| s == 16_384), "downmix average broken");
    }

    #[test]
    fn non_integer_ratio_44100_does_not_drift_across_chunk_boundaries() {
        // 10 s of input. The chunked run must equal the one-shot run exactly:
        // any per-buffer rounding shows up here as a length or content diff.
        let src = sine(44_100, 440.0, 0.5, 441_000);
        let mut whole = Vec::new();
        Resampler::new(44_100, 1).push(&src, &mut whole);

        let mut chunked = Vec::new();
        let mut r = Resampler::new(44_100, 1);
        for chunk in src.chunks(4_410) {
            r.push(chunk, &mut chunked);
        }

        assert!((whole.len() as i64 - 160_000).abs() <= 2, "got {}", whole.len());
        assert_eq!(whole.len(), chunked.len());
        assert_eq!(whole, chunked);
    }

    #[test]
    fn many_tiny_buffers_match_one_big_buffer() {
        // 100 uneven small chunks vs one push — same signal, same output.
        let src = sine(44_100, 1_000.0, 0.8, 44_100);
        let mut whole = Vec::new();
        Resampler::new(44_100, 1).push(&src, &mut whole);

        let mut chunked = Vec::new();
        let mut r = Resampler::new(44_100, 1);
        for chunk in src.chunks(441) {
            r.push(chunk, &mut chunked);
        }
        assert_eq!(whole, chunked);
    }

    #[test]
    fn one_khz_sine_survives_with_roughly_the_right_amplitude() {
        let src = sine(48_000, 1_000.0, 0.5, 48_000);
        let mut out = Vec::new();
        Resampler::new(48_000, 1).push(&src, &mut out);
        let peak = out.iter().map(|&s| (s as i32).abs()).max().unwrap();
        let expected = 16_384;
        assert!(
            (peak - expected).abs() <= expected / 20,
            "peak {peak}, expected ~{expected}"
        );
    }

    #[test]
    fn opposite_stereo_channels_average_to_silence() {
        // L=+1.0, R=-1.0 must cancel; +1.0 out would mean channel 0 was taken.
        let mut src = Vec::new();
        for _ in 0..1_000 {
            src.push(1.0);
            src.push(-1.0);
        }
        let mut out = Vec::new();
        Resampler::new(16_000, 2).push(&src, &mut out);
        assert!(!out.is_empty());
        assert!(out.iter().all(|&s| s == 0), "downmix did not cancel");
    }

    #[test]
    fn surround_downmix_uses_the_speech_channels_not_all_of_them() {
        // 5.1 with dialog on the center channel (FL FR FC LFE RL RR). An
        // equal average over all six divides the voice by 6 — quiet enough to
        // cost transcription accuracy — while FL+FR+FC/3 keeps it at 1/3.
        let mut src = Vec::new();
        for _ in 0..1_000 {
            src.extend_from_slice(&[0.0, 0.0, 0.9, 0.0, 0.0, 0.0]);
        }
        let mut out = Vec::new();
        Resampler::new(16_000, 6).push(&src, &mut out);
        assert!(!out.is_empty());
        let expected = (0.9_f32 / 3.0 * 32_768.0).round() as i16;
        assert!(
            out.iter().all(|&s| (s - expected).abs() <= 1),
            "center-channel dialog should survive at 1/3, got {:?} (want ~{expected})",
            &out[..4]
        );

        // And surround/LFE content stays out of the mix entirely.
        let mut src = Vec::new();
        for _ in 0..100 {
            src.extend_from_slice(&[0.0, 0.0, 0.0, 0.8, 0.8, 0.8]);
        }
        let mut out = Vec::new();
        Resampler::new(16_000, 6).push(&src, &mut out);
        assert!(out.iter().all(|&s| s == 0), "LFE/surround leaked into the mono mix");
    }

    #[test]
    fn i16_conversion_is_centred_and_round_trips() {
        assert_eq!(i16_to_f32(0), 0.0);
        assert_eq!(i16_to_f32(i16::MIN), -1.0);
        assert!(i16_to_f32(i16::MAX) > 0.999 && i16_to_f32(i16::MAX) < 1.0);
        let mut dst = Vec::new();
        extend_i16_to_f32(&[-100, 0, 100], &mut dst);
        assert_eq!(dst.len(), 3);
        assert_eq!(dst[1], 0.0);
        assert_eq!(dst[0], -dst[2]);
    }

    #[test]
    fn u16_conversion_is_centred() {
        // 32768 is unsigned silence; if it does not map to exactly 0.0 the
        // whole stream carries a DC offset.
        assert_eq!(u16_to_f32(32_768), 0.0);
        assert_eq!(u16_to_f32(0), -1.0);
        assert!(u16_to_f32(u16::MAX) > 0.999 && u16_to_f32(u16::MAX) < 1.0);
        let mut dst = Vec::new();
        extend_u16_to_f32(&[32_768, 0, 65_535], &mut dst);
        assert_eq!(dst[0], 0.0);
    }

    #[test]
    fn samples_past_full_scale_clamp_instead_of_wrapping() {
        // Fast path (16 kHz mono).
        let mut out = Vec::new();
        Resampler::new(16_000, 1).push(&[1.5, -1.5], &mut out);
        assert_eq!(out, vec![i16::MAX, i16::MIN]);

        // Generic path: a constant +1.5 signal stays +1.5 after interpolation,
        // so every output sample exercises the clamp.
        let mut out = Vec::new();
        Resampler::new(48_000, 1).push(&[1.5; 300], &mut out);
        assert!(!out.is_empty());
        assert!(out.iter().all(|&s| s == i16::MAX), "positive overshoot wrapped");
    }

    #[test]
    fn sixteen_khz_mono_passthrough_is_bit_exact() {
        let original: Vec<i16> = vec![i16::MIN, -12_345, -1, 0, 1, 12_345, i16::MAX];
        let mut as_f32 = Vec::new();
        extend_i16_to_f32(&original, &mut as_f32);
        let mut out = Vec::new();
        Resampler::new(16_000, 1).push(&as_f32, &mut out);
        assert_eq!(out, original);
    }

    #[test]
    fn upsampling_from_8k_roughly_doubles_and_preserves_level() {
        // One second of constant 0.25 at 8 kHz → ~16000 samples of the same
        // level; interpolation between equal endpoints must not ripple.
        let src = vec![0.25f32; 8_000];
        let mut out = Vec::new();
        Resampler::new(8_000, 1).push(&src, &mut out);
        assert!((out.len() as i64 - 16_000).abs() <= 2, "got {}", out.len());
        assert!(out.iter().all(|&s| (s - 8_192).abs() <= 1));
    }

    #[test]
    fn empty_input_is_a_no_op() {
        let mut out = Vec::new();
        Resampler::new(44_100, 2).push(&[], &mut out);
        assert!(out.is_empty());

        // State must also be untouched: interleaving empty pushes into a real
        // stream must not change the output in any way.
        let src = sine(44_100, 500.0, 0.5, 4_410);
        let mut plain = Vec::new();
        Resampler::new(44_100, 1).push(&src, &mut plain);
        let mut r = Resampler::new(44_100, 1);
        let mut with_empties = Vec::new();
        for chunk in src.chunks(441) {
            r.push(&[], &mut with_empties);
            r.push(chunk, &mut with_empties);
        }
        assert_eq!(with_empties, plain);
    }
}
