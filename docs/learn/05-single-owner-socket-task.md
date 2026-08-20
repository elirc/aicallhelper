# 05 — A single-owner socket task: the Deepgram driver

**Concept.** When several parties need a connection — audio producers writing,
a UI reading transcripts, a stop path closing, an abort path killing — the
lock-based answer (`Mutex<WebSocket>`) invites deadline inversion, split-brain
error handling, and "who closes it?" bugs. The actor answer gives the socket
to **one task** that owns it from dial to death; everyone else talks to that
task through channels and flags. The payoff is not elegance — it is that
hard properties become *structural*:

- **At-most-once error reporting**: every failure has exactly one
  classification site, inside the one task that can observe it.
- **No teardown races**: the socket dies exactly where it lived.
- **Realtime-safe producers**: a sender never touches the socket, so it never
  blocks on it.

**Where this repo stakes its life on it.**
`src-tauri/core/src/stt/deepgram.rs`, whose header is the thesis statement:

```rust
// deepgram.rs:3-12
//! One background task owns the socket end to end: it dials, flushes audio
//! that arrived while the socket was still opening, pumps audio and
//! keepalives out, pumps transcript frames in, and drains the tail on close.
//! A single owner means every failure has exactly one classification site, so
//! `SttSink::on_error` cannot fire twice and an abort cannot race a report.
//!
//! The capture side never waits on any of this: `send_audio` is one
//! allocation and an unbounded-channel push, because it runs on a realtime
//! audio callback thread where blocking causes glitches in the capture itself.
```

## The handle is thin; the driver is everything

`connect` (deepgram.rs:72-105) spawns the driver and returns *before the
handshake resolves* — deliberately, because "capture starts the instant the
user clicks Record, and the first words land while the socket is still
opening. They buffer in the channel and flush at open rather than being lost"
(deepgram.rs:100-104). The handle the caller gets holds no socket at all:

```rust
// deepgram.rs:132-136
struct DeepgramStream {
    audio_tx: mpsc::UnboundedSender<Vec<u8>>,   // audio in
    shared: Arc<Shared>,                        // flags + transcript + wake
    done: watch::Receiver<bool>,                // "the driver has exited"
}
```

Every operation is a message or a flag, never socket I/O:

- `send_audio` (deepgram.rs:140-146): one allocation, one unbounded push.
- `finalize` (deepgram.rs:148-163): sets `close_requested`, wakes the driver,
  then waits on the `done` watch with the 5 s cap. "The flag, not this
  caller, triggers the CloseStream: however many finalize() calls race, the
  driver crosses into its drain exactly once" — idempotence by construction,
  pinned by `finalize_is_idempotent_and_sends_one_close_stream`.
- `abort` (deepgram.rs:165-172): "Just flags: the driver sees them and dies
  without a close handshake."
- `Drop` (deepgram.rs:175-181) forwards to `abort`, "so the driver task
  cannot outlive its owner."

And the driver's exit is announced on *every* path, including panics:

```rust
// deepgram.rs:92-98
tokio::spawn(async move {
    run(request, driver_shared, sink, audio_rx).await;
    // Every finalize() — present and future — waits on this. Sent on
    // every exit path, including panicky ones (a dropped sender also
    // wakes the watch), so a stop can never hang on a dead driver.
    let _ = done_tx.send(true);
});
```

## One classification site, two gates

The driver `run` is a straight line with one select loop: dial → flush the
pre-open buffer (oldest-dropped at 15 s) → pump loop (audio out, keepalives,
frames in) → drain. Failure reporting is a pair of tiny functions whose
*gating condition is the whole design*:

| Site | Gated on | Why |
|------|----------|-----|
| `report` (dial failures + every pump-phase death) | `aborted` **only** | At every call site CloseStream has not been sent yet, so a death "can never be 'our own close's doing'" — gating on `close_requested` too would swallow a genuine death that raced a Stop: after open that hands back a truncated transcript with no error (the exact §5.5 failure), and before open it launders an abandoned question into `no_speech` where an honest `stt_connect` names the real culprit. |
| `finalize_death` | `aborted` **only** | A death *during* the drain, when `close_requested` is by definition true — a separate function exists precisely because the drain's success path (the server's own Close) needs distinguishing from its failures. |

(Historical note: an earlier `report_dial` gated dial failures on
`aborted || close_requested`, on the theory that a stop abandons the dial.
That was overturned when it emerged the pre-open channel holds the user's
entire captured question — a stop now *waits the dial out* within the
finalize cap and flushes the buffer, so only `abort()` abandons a dial. The
tests `a_stop_during_the_dial_still_delivers_the_buffered_question` and
`a_dial_that_fails_after_a_stop_surfaces_stt_connect` pin the new behavior.)

The comment at deepgram.rs:436-440 records why the broad gate is safe to drop:
the one self-caused death it protected against — a KeepAlive written after
CloseStream — is *structurally impossible* now, because the keepalive interval
is dropped before the drain starts (deepgram.rs:331-333) and its tick
re-checks the flags before sending (283-292). And "every report site returns
from the driver immediately" is what keeps `on_error` at-most-once: after any
report the task is gone, so there is no code left to report twice. The session
layer still wears a belt over these braces — `SessionSttSink::errored`
(machine.rs:428-445) latches the first error "even against a misbehaving
implementation" — but the guarantee is made in the driver, not enforced at
the consumer.

Two protocol subtleties worth reading closely:

- **"Established" means the server spoke, not that the handshake succeeded**
  (deepgram.rs:254-260): Deepgram accepts the WebSocket upgrade and then
  rejects bad keys by *closing* (1008 + `DATA-xxxx`), usually with no Error
  frame. Only a first server frame separates "connected" from "about to be
  rejected", so a close before it is classified `SttConnect`, after it
  `SttError` (`close_death`, 465-478). Pinned by
  `close_1008_before_open_is_a_connect_failure_and_finalize_is_instant`.
- **The drain flushes queued audio before CloseStream** (deepgram.rs:334-345):
  frames captured between the pump's last poll and the stop "carry the final
  words of the question — the part the user is asking about — and dropping
  them silently truncates the transcript's tail."

## Exercises

**Reading 1.** The dial phase (deepgram.rs:193-203) polls the connect future
in a loop that also wakes on `shared.wake` and re-checks the flags — but never
aborts the connect future on a flag. Compare with lesson 02's "loss discovered
at install time". What does the loop's `return` actually do to the in-flight
handshake, and why is that fine here when it wasn't in machine.rs?

<details><summary>Answer</summary>

Returning drops the `connect` future mid-handshake, which closes the
underlying TCP socket via its destructor — here that is safe *because the
driver owns the dial exclusively*: no other party holds or expects that
socket, so dropping it leaks nothing and orphans nobody. In machine.rs the
connect was performed by a shared connector on behalf of a session that might
lose a race — aborting there would leave a stream the loser was contractually
obliged to tear down. Ownership decides: abandon-by-drop is correct when you
own the resource outright; finish-and-check is required when the result must
be handed to a shared registry. Note also the driver checks flags *before*
selecting (196-198), so a stop that landed before the dial even started never
dials at all.
</details>

**Reading 2.** The keepalive tick re-checks `close_requested` and `aborted`
at the moment it fires (deepgram.rs:283-292), even though the main loop
already checks both at the top of every iteration (264-268). What
interleaving makes the loop-top check insufficient?

<details><summary>Answer</summary>

The select is parked *inside* an iteration: the loop-top check ran, found
nothing, and the task went to sleep on the select. A stop can land while it is
parked, setting `close_requested` and notifying `wake` — but the keepalive
tick may become ready in the same wakeup, and select picks among ready
branches without re-running the loop-top check. A KeepAlive written to a
socket the server is now closing errors, "fabricating a 'lost connection' out
of a stop that is actually succeeding" (§6.1). The re-check makes the tick
itself flag-aware. Pinned by `no_keepalive_is_sent_once_close_is_requested`,
whose paused-clock setup is intricate enough to have earned a note in
`docs/TESTING.md` ("Known test-environment notes").
</details>

**Break it.** In the drain (deepgram.rs:334-361), move the CloseStream send
*above* the queued-audio flush — i.e. reorder to:

```rust
if ws.send(Message::Text(CLOSE_STREAM.to_string())).await.is_err() {
    finalize_death(&shared, &*sink);
    return;
}
while let Ok(bytes) = audio_rx.try_recv() {
    if ws.send(Message::Binary(bytes)).await.is_err() {
        finalize_death(&shared, &*sink);
        return;
    }
}
```

Run `cargo test -p app-core audio_queued_at_stop_is_flushed_before_close_stream`.

It fails: the scripted server records message order, and the test queues 50
frames at the instant of finalize, then asserts all 50 arrive as binary
*before* the `CloseStream` text frame. With the order flipped, CloseStream
tells Deepgram "the audio is over — flush and close" while the last ~half
second of the question is still sitting in the channel; the real server would
transcribe a question missing its final words and nothing anywhere would
error. This is the quietest failure mode in the whole pipeline — no exception,
no close code, just an answer to a subtly different question — which is why
the ordering is pinned by a wire-order assertion rather than left to the
prose in §6.1. (TESTING.md notes this exact drain drop was a real bug: "the
old drain dropped them at the loop break.")
