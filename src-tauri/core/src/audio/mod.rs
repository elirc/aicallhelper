//! System-audio (loopback) capture contract and its WASAPI implementation.

pub mod capture;
pub mod resample;

use std::sync::Arc;

use crate::error::AppResult;
use crate::AppError;

/// Deepgram is opened at this rate; the resampler targets it.
pub const SAMPLE_RATE: u32 = 16_000;
/// 2048 samples @ 16 kHz = 128 ms per frame (§3).
pub const FRAME_SAMPLES: usize = 2048;

/// Where captured audio goes.
pub trait AudioSink: Send + Sync + 'static {
    /// One ~128 ms frame of 16 kHz mono i16 PCM, plus its RMS in 0.0..=1.0 for
    /// the level meter.
    fn on_frame(&self, pcm: Vec<i16>, rms: f32);
    /// Capture can no longer be trusted: the device died (unplugged, format
    /// change), or enough frames were lost that the transcript would have a
    /// hole in it rather than merely a hiccup.
    ///
    /// At most once PER SOURCE — the stream's error callback, the
    /// default-device watcher, and the frame-drop gate each latch
    /// independently, so a consumer can see more than one call for one
    /// capture. The session machine absorbs this: its phase-aware
    /// `device_error` acts on the first report it owns and ignores the rest.
    fn on_error(&self, error: AppError);
}

/// A running capture. Dropping it must stop the device.
pub trait AudioHandle: Send + Sync {
    fn stop(&self);
}

/// Opens loopback capture of the default render device. Injected so the state
/// machine never touches a real device in tests.
pub trait AudioCapture: Send + Sync + 'static {
    fn start(&self, sink: Arc<dyn AudioSink>) -> AppResult<Box<dyn AudioHandle>>;
}

/// Root-mean-square of a frame, normalised to 0.0..=1.0.
pub fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    // Accumulate in f64: 2048 squared i32 samples overflow f32 precision well
    // before they overflow the range, which makes the meter drift quiet.
    let sum: f64 = pcm.iter().map(|&s| { let v = s as f64; v * v }).sum();
    let mean = sum / pcm.len() as f64;
    ((mean.sqrt()) / (i16::MAX as f64)).clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0; 512]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_of_full_scale_square_wave_is_one() {
        let pcm: Vec<i16> = (0..512).map(|i| if i % 2 == 0 { i16::MAX } else { -i16::MAX }).collect();
        assert!((rms(&pcm) - 1.0).abs() < 1e-4, "got {}", rms(&pcm));
    }

    #[test]
    fn rms_is_bounded_even_at_the_negative_rail() {
        // i16::MIN has a larger magnitude than i16::MAX; without the clamp the
        // meter can render past 100% and overflow its track.
        let pcm = vec![i16::MIN; 256];
        let r = rms(&pcm);
        assert!((0.0..=1.0).contains(&r), "got {r}");
    }

    #[test]
    fn rms_rises_with_amplitude() {
        let quiet = rms(&[1000; 512]);
        let loud = rms(&[20000; 512]);
        assert!(quiet < loud);
        assert!(quiet > 0.0);
    }
}
