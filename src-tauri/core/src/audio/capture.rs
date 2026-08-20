//! WASAPI loopback capture of the default render device — what the other
//! person on the call is saying — plus a no-op capture for machines without
//! audio hardware.
//!
//! cpal 0.15's WASAPI backend enables loopback implicitly: building an
//! *input* stream on a *render* (output) device sets
//! AUDCLNT_STREAMFLAGS_LOOPBACK (verified against cpal-0.15.3
//! src/host/wasapi/device.rs). That is why this module asks the host for the
//! default OUTPUT device and then records from it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BuildStreamError, SampleFormat, StreamError};

use crate::audio::resample::{extend_i16_to_f32, extend_u16_to_f32, Resampler};
use crate::audio::{rms, AudioCapture, AudioHandle, AudioSink, FRAME_SAMPLES, SAMPLE_RATE};
use crate::error::{AppError, AppResult};

/// Written for the person on the call (error.rs voice): says what to do next.
const MSG_NO_PLAYBACK_DEVICE: &str = "No audio playback device is active, so call audio can't be \
     captured. Enable a speaker or headphone output in Windows sound settings and try again.";

/// ~4 s of 128 ms frames may queue before frames are dropped: enough to ride
/// out a UI hiccup, small enough that a wedged consumer cannot grow memory
/// without bound.
const FRAME_QUEUE_DEPTH: usize = 32;

/// How much lost audio turns a hiccup into a different question: 8 × 128 ms ≈
/// 1 s, a hole big enough to swallow a clause. Below it the loss stays silent
/// by design — a 128 ms gap does not change what the transcript asks, and
/// failing the session over it would cost the user more than the gap does.
const DROP_REPORT_FRAMES: usize = 8;

/// The threshold-and-latch decision for dropped frames, factored out of the
/// forwarder thread so it is tested with no device anywhere near the test: the
/// realtime callback may only count, so all the judgement lives here.
#[derive(Default)]
struct DropGate {
    lost: usize,
    reported: bool,
}

impl DropGate {
    /// Adds `dropped` newly lost frames and returns the running total exactly
    /// once — on the call that crosses the threshold. Latched afterwards
    /// because a consumer that fell behind keeps dropping frames, and a storm
    /// of identical errors tells the user nothing the first one did not.
    fn record(&mut self, dropped: usize) -> Option<usize> {
        self.lost += dropped;
        if self.reported || self.lost < DROP_REPORT_FRAMES {
            return None;
        }
        self.reported = true;
        Some(self.lost)
    }
}

/// Written for the person on the call (error.rs voice): names the size of the
/// hole, since "some audio was lost" is not something a user can act on.
fn frames_lost_message(frames: usize) -> String {
    let seconds = frames as f32 * FRAME_SAMPLES as f32 / SAMPLE_RATE as f32;
    format!(
        "About {seconds:.1} s of call audio was lost because the app couldn't keep up, so the \
         transcript would have a gap. Close other heavy apps and press Record again."
    )
}

/// Accumulates 16 kHz mono samples into exactly [`FRAME_SAMPLES`]-sample
/// frames. Factored out of the cpal callback so the framing logic is testable
/// with no audio device anywhere near the test.
pub struct FrameAccumulator {
    buf: Vec<i16>,
}

impl FrameAccumulator {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(FRAME_SAMPLES) }
    }

    /// Emits every complete frame via `emit`; keeps any remainder for the
    /// next call. A short frame is never padded with silence: padding injects
    /// an audible click and shifts everything after it by the pad length.
    pub fn push(&mut self, samples: &[i16], mut emit: impl FnMut(Vec<i16>)) {
        let mut rest = samples;
        while !rest.is_empty() {
            let take = (FRAME_SAMPLES - self.buf.len()).min(rest.len());
            self.buf.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.buf.len() == FRAME_SAMPLES {
                // Ownership must transfer to the sink anyway, so this is one
                // fixed-size allocation per 128 ms — bounded, and far below
                // what a WASAPI period can absorb.
                let frame = std::mem::replace(&mut self.buf, Vec::with_capacity(FRAME_SAMPLES));
                emit(frame);
            }
        }
    }
}

impl Default for FrameAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything the realtime data callback owns: format conversion, resampling,
/// framing, and the non-blocking hand-off out of the callback.
struct Pipeline {
    resampler: Resampler,
    frames: FrameAccumulator,
    /// Reused across callbacks so the steady state does no per-callback
    /// allocation (capacity converges after the first few periods).
    resampled: Vec<i16>,
    scratch: Vec<f32>,
    frame_tx: SyncSender<Vec<i16>>,
    /// Shared with the forwarder thread, which is where a dropped frame is
    /// judged. A dropped frame is a hole in the transcript, and losing it
    /// without a trace is the one thing §5.5 forbids.
    dropped: Arc<AtomicUsize>,
}

impl Pipeline {
    fn new(
        src_rate: u32,
        src_channels: u16,
        frame_tx: SyncSender<Vec<i16>>,
        dropped: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            resampler: Resampler::new(src_rate, src_channels),
            frames: FrameAccumulator::new(),
            resampled: Vec::new(),
            scratch: Vec::new(),
            frame_tx,
            dropped,
        }
    }

    fn feed_f32(&mut self, interleaved: &[f32]) {
        self.resampled.clear();
        self.resampler.push(interleaved, &mut self.resampled);
        let tx = &self.frame_tx;
        let dropped = &self.dropped;
        self.frames.push(&self.resampled, |frame| {
            // This runs on WASAPI's realtime thread: blocking or locking here
            // glitches every audio app on the system, so completed frames are
            // handed to a plain thread over a bounded channel. try_send never
            // blocks; if the consumer has stalled, the price is a dropped
            // frame, never a stalled audio engine. rms and the sink call both
            // happen on the forwarder thread for the same reason.
            if tx.try_send(frame).is_err() {
                // One relaxed fetch_add is all a realtime thread may spend on
                // this — no allocation, no lock, no message. Relaxed is
                // enough because nothing here is ordered against the frames:
                // the count only has to arrive, and the forwarder's next take
                // (or the one after it) picks it up.
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    fn feed_i16(&mut self, data: &[i16]) {
        // Taking the scratch buffer out sidesteps borrowing self twice while
        // still reusing its allocation every callback.
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        extend_i16_to_f32(data, &mut scratch);
        self.feed_f32(&scratch);
        self.scratch = scratch;
    }

    fn feed_u16(&mut self, data: &[u16]) {
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        extend_u16_to_f32(data, &mut scratch);
        self.feed_f32(&scratch);
        self.scratch = scratch;
    }
}

/// The one function that touches device selection and stream construction,
/// kept small and isolated so an API correction at integration time is a
/// one-place edit. Must be called on the thread that will own the stream:
/// cpal's `Stream` is deliberately `!Send`.
fn open_loopback_stream(
    frame_tx: SyncSender<Vec<i16>>,
    dropped: Arc<AtomicUsize>,
    sink: Arc<dyn AudioSink>,
) -> AppResult<(cpal::Stream, String)> {
    let host = cpal::default_host();
    // The OUTPUT device on purpose: recording an input stream from a render
    // device is how cpal's WASAPI backend exposes loopback (see module docs).
    let device = host
        .default_output_device()
        .ok_or_else(|| AppError::internal(MSG_NO_PLAYBACK_DEVICE))?;
    // `default_input_config` returns StreamTypeNotSupported for render
    // devices, so the loopback stream must be opened at the device's own
    // output mix format; the Resampler absorbs whatever that turns out to be.
    let supported = device.default_output_config().map_err(|e| {
        AppError::internal(format!(
            "Could not read the playback device's audio format ({e}). {MSG_NO_PLAYBACK_DEVICE}"
        ))
    })?;
    let sample_format = supported.sample_format();
    let config = supported.config();

    let mut pipeline = Pipeline::new(config.sample_rate.0, config.channels, frame_tx, dropped);

    // Latched so the sink hears about a dead device exactly once: WASAPI can
    // fire the error callback repeatedly while a device is unplugged, and a
    // storm of identical toasts reads as a crash loop to the user.
    let mut reported = false;
    let on_error = move |err: StreamError| {
        if reported {
            return;
        }
        reported = true;
        let message = match err {
            StreamError::DeviceNotAvailable => MSG_NO_PLAYBACK_DEVICE.to_string(),
            e => format!(
                "Audio capture stopped unexpectedly ({e}). \
                 Check that a playback device is still active, then restart capture."
            ),
        };
        sink.on_error(AppError::internal(message));
    };

    // The shared-mode mix format is f32 on effectively every Windows machine,
    // but WASAPI is allowed to report i16/u16 and cpal will hand those over
    // verbatim, so all three are wired up rather than trusted away.
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| pipeline.feed_f32(data),
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| pipeline.feed_i16(data),
            on_error,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| pipeline.feed_u16(data),
            on_error,
            None,
        ),
        other => {
            return Err(AppError::internal(format!(
                "The playback device uses an unsupported sample format ({other}). \
                 Try selecting a different default output device in Windows."
            )))
        }
    }
    .map_err(|e| match e {
        BuildStreamError::DeviceNotAvailable => AppError::internal(MSG_NO_PLAYBACK_DEVICE),
        e => AppError::internal(format!(
            "Could not start loopback capture on the playback device ({e})."
        )),
    })?;

    stream.play().map_err(|e| {
        AppError::internal(format!("Could not start the audio capture stream ({e})."))
    })?;
    // The device name identifies which device this stream is bolted to, so
    // the watch loop can notice the DEFAULT moving somewhere else.
    Ok((stream, device.name().unwrap_or_default()))
}

/// How often the owning thread checks whether the default render device moved.
const DEVICE_POLL_PERIOD: Duration = Duration::from_secs(2);

/// Pure decision for the device watcher, split out so it is testable without
/// a device: report only when a CURRENT default exists and differs. No
/// default at all means the device died — the stream's own error callback
/// owns that report, and duplicating it here would double-toast the user.
fn default_device_moved(original: &str, current: Option<&str>) -> bool {
    matches!(current, Some(current) if current != original)
}

/// Loopback capture of the default render device via cpal/WASAPI.
#[derive(Default)]
pub struct WasapiLoopbackCapture;

impl AudioCapture for WasapiLoopbackCapture {
    /// Opens the device synchronously: a missing or broken playback device is
    /// returned as an `Err` here (same message the runtime path uses), while
    /// deaths after a successful start arrive once via `sink.on_error`.
    fn start(&self, sink: Arc<dyn AudioSink>) -> AppResult<Box<dyn AudioHandle>> {
        let (ready_tx, ready_rx) = mpsc::channel::<AppResult<()>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (frame_tx, frame_rx) = mpsc::sync_channel::<Vec<i16>>(FRAME_QUEUE_DEPTH);
        // One counter per capture, so the latch inside the forwarder's gate
        // resets with every Record press rather than staying stuck for the
        // lifetime of the app.
        let dropped = Arc::new(AtomicUsize::new(0));

        // Frames leave the realtime callback through a bounded channel and are
        // delivered to the sink from this plain thread, where the sink is free
        // to take locks or allocate without touching audio timing.
        let frame_sink = sink.clone();
        let frame_drops = dropped.clone();
        spawn_thread("audio-frames", move || {
            forward_frames(frame_rx, frame_sink, frame_drops)
        })?;

        // cpal's `Stream` is `!Send`, so it is built, owned, and dropped on
        // one dedicated thread; the handle only signals that thread.
        spawn_thread("audio-loopback", move || {
            let (stream, device_name) = match open_loopback_stream(frame_tx, dropped, sink.clone())
            {
                Ok(opened) => {
                    let _ = ready_tx.send(Ok(()));
                    opened
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Parked until the handle drops its sender (stop or Drop) — but
            // with a heartbeat: WASAPI keeps capturing from the ORIGINAL
            // device after the user switches defaults (plugs in a headset),
            // so the app would silently record silence for the rest of the
            // call. cpal surfaces no event for a default switch, so the
            // default's identity is polled and one honest error is surfaced
            // when it moves. This thread is not realtime — a ~ms COM
            // enumeration every 2 s costs nothing audible.
            loop {
                match stop_rx.recv_timeout(DEVICE_POLL_PERIOD) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let current = cpal::default_host()
                            .default_output_device()
                            .and_then(|d| d.name().ok());
                        if default_device_moved(&device_name, current.as_deref()) {
                            sink.on_error(AppError::internal(format!(
                                "The default playback device changed to \"{}\" while \
                                 recording, so call audio is no longer being captured. \
                                 Press Record to capture the new device.",
                                current.unwrap_or_default()
                            )));
                            break;
                        }
                    }
                }
            }
            drop(stream);
        })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Box::new(LoopbackHandle { stop: Mutex::new(Some(stop_tx)) })),
            Ok(Err(e)) => Err(e),
            // The thread died before reporting: only reachable if it panicked
            // inside cpal, so surface it rather than hang the caller.
            Err(_) => Err(AppError::internal(
                "The audio capture thread failed to start. Try restarting the app.",
            )),
        }
    }
}

fn spawn_thread(
    name: &str,
    body: impl FnOnce() + Send + 'static,
) -> AppResult<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
        .map_err(|e| AppError::internal(format!("Could not start the {name} thread ({e}).")))
}

fn forward_frames(
    frame_rx: Receiver<Vec<i16>>,
    sink: Arc<dyn AudioSink>,
    dropped: Arc<AtomicUsize>,
) {
    let mut gate = DropGate::default();
    // Known limit: drops are judged when the NEXT frame is delivered, so a
    // sink wedged inside on_frame forever, or frames lost after the very last
    // delivery, are never reported from here. Accepted: a permanently wedged
    // sink means the session is already lost to a bigger failure, and the
    // trailing-loss window is one frame hand-off wide.
    //
    // Exits when every sender is gone — i.e. when the stream (and with it the
    // data callback) has been dropped — after draining what was queued.
    while let Ok(frame) = frame_rx.recv() {
        // Taken (not read) so every fetch_add in the callback lands in exactly
        // one of these takes: scattered single-frame losses still add up to
        // the truth instead of each looking like a harmless blip.
        if let Some(lost) = gate.record(dropped.swap(0, Ordering::Relaxed)) {
            // Reported before the frame that follows the hole, because the
            // hole already happened. The machine's phase-aware device_error is
            // exactly the right policy for this: while recording it fails the
            // session honestly (§5.5 — a transcript with a hole in it answers
            // a different question than the user asked), and after stop it
            // ignores the error, so a late hiccup can never kill an answer
            // that is already streaming.
            sink.on_error(AppError::internal(frames_lost_message(lost)));
        }
        let level = rms(&frame);
        sink.on_frame(frame, level);
    }
}

/// Stopping means dropping the stop sender: the stream thread's `recv` wakes,
/// the stream drops, WASAPI releases the device. `Option::take` makes both
/// `stop()` and `Drop` idempotent — the second caller finds nothing to drop.
struct LoopbackHandle {
    stop: Mutex<Option<Sender<()>>>,
}

impl AudioHandle for LoopbackHandle {
    fn stop(&self) {
        if let Ok(mut guard) = self.stop.lock() {
            guard.take();
        }
    }
}

impl Drop for LoopbackHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts successfully and emits nothing, so the app and its tests run on
/// machines with no audio device at all (CI, headless VMs).
#[derive(Default)]
pub struct NullCapture;

impl AudioCapture for NullCapture {
    fn start(&self, _sink: Arc<dyn AudioSink>) -> AppResult<Box<dyn AudioHandle>> {
        Ok(Box::new(NullHandle))
    }
}

struct NullHandle;

impl AudioHandle for NullHandle {
    fn stop(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_watch_reports_only_a_real_move() {
        // WHY: the watcher's one job is telling "the default moved" apart
        // from "the default died" — the stream's own error callback owns the
        // death report, and double-reporting reads as a crash loop.
        assert!(default_device_moved("Speakers", Some("Headset")));
        assert!(!default_device_moved("Speakers", Some("Speakers")));
        assert!(!default_device_moved("Speakers", None));
    }

    // No test here may open a real device: everything device-shaped goes
    // through NullCapture, and the framing logic is exercised directly.

    fn collect(acc: &mut FrameAccumulator, samples: &[i16]) -> Vec<Vec<i16>> {
        let mut frames = Vec::new();
        acc.push(samples, |f| frames.push(f));
        frames
    }

    #[test]
    fn emits_only_complete_frames_and_retains_the_remainder() {
        let mut acc = FrameAccumulator::new();
        let frames = collect(&mut acc, &vec![7i16; FRAME_SAMPLES + 5]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), FRAME_SAMPLES);

        // The 5 leftovers must still be there: 2043 more completes a frame.
        let frames = collect(&mut acc, &vec![7i16; FRAME_SAMPLES - 5]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), FRAME_SAMPLES);
    }

    #[test]
    fn emits_exactly_n_frames_for_n_times_frame_samples() {
        let mut acc = FrameAccumulator::new();
        let mut total = 0usize;
        // Ragged chunk sizes so frame boundaries never align with pushes.
        for chunk in vec![1i16; 4 * FRAME_SAMPLES].chunks(777) {
            acc.push(chunk, |f| {
                assert_eq!(f.len(), FRAME_SAMPLES);
                total += 1;
            });
        }
        assert_eq!(total, 4);
    }

    #[test]
    fn never_emits_a_short_frame() {
        let mut acc = FrameAccumulator::new();
        let frames = collect(&mut acc, &vec![1i16; FRAME_SAMPLES - 1]);
        assert!(frames.is_empty(), "a short frame was emitted");
        let frames = collect(&mut acc, &[]);
        assert!(frames.is_empty());
    }

    #[test]
    fn frame_content_is_passed_through_in_order() {
        let mut acc = FrameAccumulator::new();
        let src: Vec<i16> = (0..FRAME_SAMPLES as i32 * 2).map(|i| (i % 30_000) as i16).collect();
        let mut frames = Vec::new();
        for chunk in src.chunks(999) {
            acc.push(chunk, |f| frames.push(f));
        }
        let flat: Vec<i16> = frames.concat();
        assert_eq!(flat, src);
    }

    #[derive(Default)]
    struct CountingSink {
        frames: AtomicUsize,
        errors: AtomicUsize,
        messages: Mutex<Vec<String>>,
    }

    impl AudioSink for CountingSink {
        fn on_frame(&self, _pcm: Vec<i16>, _rms: f32) {
            self.frames.fetch_add(1, Ordering::SeqCst);
        }
        fn on_error(&self, error: AppError) {
            self.errors.fetch_add(1, Ordering::SeqCst);
            self.messages.lock().unwrap().push(error.message);
        }
    }

    #[test]
    fn null_capture_starts_emits_nothing_and_stop_is_idempotent() {
        let sink = Arc::new(CountingSink::default());
        let handle = NullCapture.start(sink.clone()).expect("null capture must start");
        handle.stop();
        handle.stop();
        drop(handle);
        assert_eq!(sink.frames.load(Ordering::SeqCst), 0);
        assert_eq!(sink.errors.load(Ordering::SeqCst), 0);
    }

    // ---- dropped frames (§5.5: no silent holes in the transcript) ----------

    #[test]
    fn a_brief_hiccup_stays_silent() {
        // WHY: 128 ms of missing audio cannot change what the transcript asks,
        // and killing a live session over it costs the user their question —
        // strictly worse than the gap it would be reporting.
        let mut gate = DropGate::default();
        for _ in 0..DROP_REPORT_FRAMES - 1 {
            assert_eq!(gate.record(1), None);
        }
    }

    #[test]
    fn crossing_the_threshold_reports_exactly_once() {
        // WHY: a consumer that fell behind keeps dropping frames for as long
        // as it is behind; the user needs one honest error, not a toast storm
        // that reads as a crash loop.
        let mut gate = DropGate::default();
        assert_eq!(gate.record(DROP_REPORT_FRAMES), Some(DROP_REPORT_FRAMES));
        assert_eq!(gate.record(DROP_REPORT_FRAMES), None);
        assert_eq!(gate.record(1), None);
        assert_eq!(gate.record(0), None);
    }

    #[test]
    fn scattered_single_frame_losses_accumulate_into_one_report() {
        // WHY: the counter is taken frame by frame, so a second of loss almost
        // never arrives as one big number — if the small takes did not add up,
        // an arbitrarily long hole could stay invisible forever.
        let mut gate = DropGate::default();
        let mut reports = Vec::new();
        for _ in 0..DROP_REPORT_FRAMES * 3 {
            if let Some(lost) = gate.record(1) {
                reports.push(lost);
            }
        }
        assert_eq!(reports, vec![DROP_REPORT_FRAMES]);
    }

    #[test]
    fn a_report_names_the_size_of_the_gap_and_what_to_do() {
        // WHY: "audio was lost" is not actionable; the number is what tells
        // the user whether their question survived.
        let message = frames_lost_message(DROP_REPORT_FRAMES);
        assert!(message.contains("1.0 s"), "got {message}");
        assert!(message.contains("Record"), "got {message}");
    }

    fn drain_forwarder(pre_dropped: usize, frames: usize) -> Arc<CountingSink> {
        let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(FRAME_QUEUE_DEPTH);
        let dropped = Arc::new(AtomicUsize::new(pre_dropped));
        let sink = Arc::new(CountingSink::default());
        for _ in 0..frames {
            tx.send(vec![0i16; FRAME_SAMPLES]).expect("the queue must have room");
        }
        // Dropping the sender is what ends the loop, so this runs to
        // completion on the test thread — no device, no sleeping.
        drop(tx);
        forward_frames(rx, sink.clone(), dropped);
        sink
    }

    #[test]
    fn the_forwarder_reports_a_big_loss_once_and_still_delivers_its_frames() {
        let sink = drain_forwarder(DROP_REPORT_FRAMES, 3);
        assert_eq!(sink.errors.load(Ordering::SeqCst), 1);
        assert!(sink.messages.lock().unwrap()[0].contains("lost"));
        // The frames that DID make it are still audio the transcript needs.
        assert_eq!(sink.frames.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn the_forwarder_stays_silent_under_the_threshold() {
        let sink = drain_forwarder(DROP_REPORT_FRAMES - 1, 3);
        assert_eq!(sink.errors.load(Ordering::SeqCst), 0);
        assert_eq!(sink.frames.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_full_queue_counts_every_dropped_frame() {
        // WHY: the try_send-failure increment is the single line that turns a
        // silent transcript hole into a countable one (§5.5). Nothing else
        // pins it: a refactor of the hand-off that forgets the counter would
        // pass every other test while losing audio invisibly again. 16 kHz
        // mono is a bit-exact passthrough (resample.rs proves it), so three
        // frames of samples into a capacity-1 channel must land one frame and
        // count exactly the other two.
        let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(1);
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut pipeline = Pipeline::new(16_000, 1, tx, dropped.clone());

        pipeline.feed_f32(&vec![0.25f32; FRAME_SAMPLES * 3]);

        assert_eq!(dropped.load(Ordering::Relaxed), 2, "two of three frames must be counted");
        assert_eq!(rx.try_iter().count(), 1, "the frame that fit still flows");
    }
}
