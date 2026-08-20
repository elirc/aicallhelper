# 06 — Realtime audio constraints: the callback, the bounded channel, and drift-free resampling

**Concept.** A realtime audio callback runs on a thread the OS schedules
against a hardware deadline measured in milliseconds. Miss the deadline and
the *device* glitches — not just your app; on WASAPI shared mode, every app
on the system. The rules on that thread are the same as for signal handlers
and interrupt context:

- **No blocking**: no mutex that anything slower holds, no channel `send` that
  can park, no syscall that can wait.
- **No unbounded work**: allocation is tolerable only if bounded and rare;
  I/O never.
- **Get out fast**: move data across a *non-blocking, bounded* hand-off and do
  everything else — logging, metrics, network — on a plain thread.

And the hand-off forces a policy decision every senior engineer should be able
to defend: when the consumer stalls, you either block (never, here), grow
without bound (a memory leak with a deadline), or **drop** — and choose
*which end* to drop from.

**Where this repo stakes its life on it.**
`src-tauri/core/src/audio/capture.rs` (the WASAPI loopback path) and
`resample.rs` (the format conversion that runs *on* the callback).

## The callback's one exit

```rust
// capture.rs:96-105 (inside Pipeline::feed_f32)
self.frames.push(&self.resampled, |frame| {
    // This runs on WASAPI's realtime thread: blocking or locking here
    // glitches every audio app on the system, so completed frames are
    // handed to a plain thread over a bounded channel. try_send never
    // blocks; if the consumer has stalled, the price is a dropped frame,
    // never a stalled audio engine. rms and the sink call both
    // happen on the forwarder thread for the same reason.
    let _ = tx.try_send(frame);
});
```

Every clause is one of the rules above. The channel is
`sync_channel(FRAME_QUEUE_DEPTH)` with depth 32 — "~4 s of 128 ms frames …
enough to ride out a UI hiccup, small enough that a wedged consumer cannot
grow memory without bound" (capture.rs:27-30). `try_send` converts
backpressure into frame drops, which for live speech is correct: late audio is
worthless. Even RMS — a multiply-add loop — is exiled to the forwarder thread
(`forward_frames`, capture.rs:312-319), because the callback budget is not
"cheap things allowed", it is "nothing that can surprise you".

The steady state allocates almost nothing: `resampled`/`scratch` buffers are
reused across callbacks ("capacity converges after the first few periods",
capture.rs:76-79), and the one allocation per 128 ms frame is deliberate,
bounded, and justified in place (capture.rs:54-57 — ownership must transfer to
the sink anyway).

Two more constraints shape the file:

- **cpal's `Stream` is `!Send`** — so it is "built, owned, and dropped on one
  dedicated thread; the handle only signals that thread" (capture.rs:247-249).
  Stopping is *dropping a channel sender* (`LoopbackHandle`,
  capture.rs:321-340): the owning thread's `recv` wakes, the stream drops,
  WASAPI releases the device — and `Option::take` makes stop/Drop idempotent
  (lesson 01's move, again).
- **WASAPI tells you nothing when the default device moves.** Plug in a
  headset mid-recording and WASAPI keeps capturing the old device — the app
  would "silently record silence for the rest of the call". So the owning
  thread's park doubles as a 2 s heartbeat (capture.rs:260-286) polling the
  default device's name, with the decision extracted pure:

  ```rust
  // capture.rs:220-226
  /// report only when a CURRENT default exists and differs. No
  /// default at all means the device died — the stream's own error callback
  /// owns that report, and duplicating it here would double-toast the user.
  fn default_device_moved(original: &str, current: Option<&str>) -> bool {
      matches!(current, Some(current) if current != original)
  }
  ```

  The error callback itself is latched (`reported`, capture.rs:155-171)
  because WASAPI "can fire the error callback repeatedly while a device is
  unplugged, and a storm of identical toasts reads as a crash loop."

## Drift-free resampling: integers where floats would lie

The resampler converts whatever the device plays (44.1 k/48 k, stereo/5.1,
f32/i16/u16) to Deepgram's 16 kHz mono. The dangerous requirement is hidden in
the chunking: WASAPI delivers arbitrary buffer sizes, and the output must be
**exactly** the same as if the whole signal had arrived in one buffer
(resample.rs:43-47) — "per-buffer resampling that resets or rounds its
position accumulates drift (measurably out of sync within minutes against
Deepgram's clock) and clicks at every buffer boundary."

The read position is therefore kept as an exact rational, never a float:

```rust
// resample.rs:53-63
/// Fractional part of the read position as an exact integer numerator over
/// SAMPLE_RATE. Kept in integers, and carried across calls, so the
/// position never drifts no matter how the input is chunked — floating
/// point accumulation loses a little on every buffer and the loss only grows.
frac_num: u32,
/// Last mono sample of the previous push, so an interpolation window that
/// straddles a buffer boundary still has both endpoints instead of
/// snapping to the nearest sample (an audible click, every buffer).
carry: Option<f32>,
```

The advance is `frac_num += src_rate; idx += frac_num / SAMPLE_RATE;
frac_num %= SAMPLE_RATE` (resample.rs:139-142) — pure integer arithmetic, so
10 s fed as one buffer or as 100 ragged chunks produces bit-identical output
(`non_integer_ratio_44100_does_not_drift_across_chunk_boundaries`,
`many_tiny_buffers_match_one_big_buffer`).

The downmix carries a recently-audited decision:

```rust
// resample.rs:96-107 (abridged comment)
// Downmix: average the SPEECH channels, not all of them. In the
// WAVEFORMATEXTENSIBLE channel order the first three are FL, FR, FC —
// where calls and dialog live. On a 5.1/7.1 device an equal average
// over all channels divides the voice by 6 or 8 ... quiet enough to cost
// transcription accuracy.
let used_ch = ch.min(3);
```

And the scalar conversions are little essays in numeric honesty: i16→f32
divides by 32768 (exact power of two) so a 16 kHz mono i16 stream round-trips
bit-exactly (resample.rs:9-14, `sixteen_khz_mono_passthrough_is_bit_exact`);
u16 silence sits at 32768, not 0 (resample.rs:16-21); and f32→i16 clamps
because "float audio may legally exceed ±1.0 … a wrapping cast would turn a
slightly hot sample into a full-scale polarity flip that sounds like broken
hardware" (resample.rs:33-39).

## Exercises

**Reading 1.** The pre-open buffer in the Deepgram driver drops the *oldest*
frames past its 15 s cap (deepgram.rs:47-51), while the capture channel drops
the *newest* frames when its 32-slot queue is full (`try_send` failing). Both
are drop policies on the same audio path. Why opposite ends?

<details><summary>Answer</summary>

They answer different questions. The pre-open buffer exists because *the
connect is slow, not the speech wrong* — when it overflows, the newest audio
"is the speech the user is asking about right now", so you shed history from
the back. The capture channel exists to protect the realtime thread from a
stalled consumer — you cannot drop *older* frames from the producer side of a
SPSC queue without locking or draining it (which would block), and a stalled
consumer means real-time delivery has already failed, so the marginal frame is
the right loss. Policy follows mechanism: drop-oldest needs an owned deque
(the driver has one, deepgram.rs:228-239); drop-newest is what a full bounded
channel gives you for free, non-blockingly.
</details>

**Reading 2.** `FrameAccumulator::push` never pads a short frame with silence
— it holds the remainder for the next callback (capture.rs:44-61). What are
the *two* distinct corruptions the comment says padding would cause, and why
does `never_emits_a_short_frame` matter to Deepgram specifically?

<details><summary>Answer</summary>

"Padding injects an audible click" — a run of zeros spliced into speech is a
discontinuity Deepgram hears — "and shifts everything after it by the pad
length": every subsequent sample arrives later than its true time, a permanent
timeline error that compounds per pad. Deepgram is opened at
`linear16 / 16000 / mono` with fixed ~128 ms binary frames (§6.1); it has no
framing metadata to recover from — the byte stream *is* the timeline. The
accumulator's contract (exactly `FRAME_SAMPLES` per emit, remainder retained,
order preserved) is pinned by four tests precisely because "frame boundaries
never align with device buffers in practice"
(`emits_exactly_n_frames_for_n_times_frame_samples`).
</details>

**Break it.** In `Resampler::push` (resample.rs:107), downmix all channels
instead of the speech channels:

```rust
let used_ch = ch;
```

Run `cargo test -p app-core surround_downmix_uses_the_speech_channels_not_all_of_them`.

It fails: the test builds 5.1 audio with dialog on the center channel
(FL FR FC LFE RL RR) and asserts it survives at exactly 1/3 — the FL+FR+FC
average — while pure LFE/surround content mixes to zero. With the edit, the
center-channel voice comes out at 1/6 amplitude (divided by six channels,
five of them near-silent) and the assertion trips. The reason this is a test
and not a code-review nicety: on a stereo device everything works, on most
setups everything works, and the failure — a user whose default output is a
5.1 receiver getting mysteriously worse transcription — would never be traced
back to a downmix divisor. This was one of the audited fixes; the test is
what keeps the next "simplify the average" cleanup from reintroducing it.
