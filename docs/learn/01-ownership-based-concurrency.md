# 01 — Ownership-based concurrency: the slot, the Gate, and exactly-once release

**Concept.** In most codebases, "clean up exactly once" is a discipline: every
exit path must remember to call `release()`, and a code review is the only
thing standing between you and a double-free or a leaked slot. Ownership-based
design turns the discipline into a type-system consequence: the resource *is a
value*, taking it out of its container consumes it, and a second taker finds
nothing. You cannot forget what you cannot express.

**Where this repo stakes its life on it.** The session machine
(`src-tauri/core/src/session/machine.rs`) runs one question/answer pipeline at
a time, and §5.11 of the spec demands: *whatever the outcome (done, error,
abort), the active slot is released exactly once*. Six paths can end a session
— completion, provider error, STT death, timeout, cancel, supersession — and
any two of them can race. Nobody coordinates them. The types do.

## The one-slot registry

```rust
// machine.rs:36-46
/// The one-slot session registry. `Mutex<Option<Active>>` rather than a map:
/// the product rule is "one live pipeline", and encoding it in the type means
/// supersession cannot be forgotten on any path.
pub struct SessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    slot: Mutex<Option<Active>>,
    next_id: AtomicU64,
}
```

The product rule ("one live pipeline") is not a comment on a map — it *is* the
data structure. A `HashMap<SessionId, Active>` would make "two live sessions"
representable, and everything representable eventually happens. With
`Option<Active>`, installing a new session **necessarily** evicts the old one:

```rust
// machine.rs:124-126
fn replace(&self, active: Active) -> Option<Active> {
    self.slot.lock().unwrap().replace(active)
}
```

`Option::replace` hands back the previous occupant *by value*. The caller
(`start`, machine.rs:216-226) is now the exclusive owner of the superseded
session and must do something with it — the compiler will not let it silently
persist. It goes into `teardown`:

```rust
// machine.rs:106-117
fn teardown(active: Active) {
    active.gate.kill();
    active.cancel.cancel();
    if let Phase::Recording { stream } = &active.phase {
        stream.abort();
    }
    // Dropping `active` drops `stop_tx`, which unblocks a driver parked on the
    // stop signal so it can exit through its silent path.
}
```

Note the ordering comment at machine.rs:106-108: the gate dies *first*, so
nothing the teardown itself provokes — the socket death `abort()` causes, a
delta racing in on another task — can reach the UI. Order inside a function is
the one thing types can't enforce, so it gets a comment and a test instead.

## Release keyed by identity, so every path may call it

```rust
// machine.rs:155-168
/// The single point where a session leaves the slot on its own terms.
/// Because it is keyed by id, every terminal path can call it and the slot
/// is still released exactly once (§5.11) — a second call, or a call after
/// supersession already emptied the slot, is a no-op.
fn release_if_current(&self, id: SessionId) -> bool {
    let mut slot = self.slot.lock().unwrap();
    match slot.as_ref() {
        Some(active) if active.id == id => {
            *slot = None;
            true
        }
        _ => false,
    }
}
```

This is the compare-and-swap shape: *release only if you still own it*. It
makes release **idempotent** (safe to call from every terminal path without
coordination) and **exclusive** (the boolean tells you whether you were the
owner). Both terminal emitters are built on that boolean:

- `complete` (machine.rs:186-193) emits `done` only if `release_if_current`
  returned true — a superseded session's `done` "is the most misleading event
  there is (§5.1): it repaints an answer the user already abandoned."
- `fail` (machine.rs:170-183) captures `owned` first, kills the gate, then
  emits only `if owned && !error.is_aborted()`.

## Two more ownership tricks in the same file

**The stop signal is a `oneshot::Sender`.** §5.3 says stop is taken exactly
once. Look at how that is *enforced*:

```rust
// machine.rs:250-256 (inside SessionManager::stop, under the slot lock)
active.phase = Phase::Finishing;
// Send the stop instant while still holding the lock, so
// the driver can never observe Finishing without it.
if let Some(tx) = active.stop_tx.take() {
    let _ = tx.send(Instant::now());
}
```

Two layers: `Option::take` consumes the sender out of the struct (second stop
finds `None`), and `oneshot::Sender::send(self)` takes `self` by value — the
type cannot be used twice even if you tried. The stop *instant* travels on the
channel itself, so the metrics clock (§3: "the latency clock starts here")
is captured where the request landed, not where the driver task happened to
resume.

**The Gate is a latch, not a lock.**

```rust
// machine.rs:77-104 (abridged)
struct Gate {
    id: SessionId,
    sink: Arc<dyn EventSink>,
    dead: AtomicBool,
}
impl Gate {
    fn emit(&self, event: SessionEvent) {
        if !self.is_dead() {
            self.sink.emit(event);
        }
    }
    fn kill(&self) {
        self.dead.store(true, Ordering::Release);
    }
}
```

Deltas arrive on whatever task polls the provider's response body — they
cannot take the slot lock per token without contending with `push_audio` on
the audio path. So suppression is a one-way `AtomicBool`: kill is monotonic,
emit is a load. A delta that races in after the session's fate was decided
paints nothing (§5.9), no matter which thread it arrives on.

## The tests that pin it

All in `machine.rs` under `#[tokio::test(start_paused = true)]`, driven by
scripted STT/LLM fakes (see `docs/TESTING.md`, session section):

- `slot_release_after_done_leaves_no_ghost` — a start after done doesn't
  "supersede" a finished session.
- `slot_release_after_error_leaves_no_ghost` — the next Record press must not
  fight a corpse.
- `cancel_is_silent_and_releases_the_slot` — cancel: no events, slot free.
- `superseded_sessions_done_is_never_emitted` (machine.rs:1247) — the
  ownership check on `complete`.
- `first_token_timeout_fires_and_late_deltas_are_suppressed` — the Gate.

## Exercises

**Reading 1.** The `Phase` enum lives *inside* `Active`, inside the slot's
mutex (machine.rs:48-59) — not in the driver task that actually runs the
pipeline. Why must it live there?

<details><summary>Answer</summary>

Because the phase decides what `stop` and `push_audio` *mean*, and both are
called from outside the driver. `stop` must atomically observe
`Phase::Recording`, flip it to `Finishing`, and send the stop instant — all
under one lock acquisition (machine.rs:245-259) — or two concurrent stops
could both be "taken". The driver only learns about transitions after they
happened (the comment at machine.rs:52-54 says exactly this); if the driver
owned the phase, every external call would have to ask it, turning a lock
acquisition into a channel round-trip with a race window on both ends.
</details>

**Reading 2.** `fail()` kills the gate and then emits the error through
`gate.sink.emit(...)` directly (machine.rs:181), bypassing `Gate::emit`'s
dead-check. Why is the bypass correct — and why is killing the gate *before*
emitting the error the right order?

<details><summary>Answer</summary>

`fail` just killed the gate itself, so `gate.emit` would suppress its own
error — the bypass is the only way the error gets out. The order (kill, then
emit) closes the §5.9 window: a delta already in flight on another thread is
suppressed *before* the error event is emitted, so nothing can paint under an
error banner. The comment at machine.rs:177-179 states the intent; the
`DetachedDeltas` fake (machine.rs:1114-1123) exists to model exactly that
racing thread.
</details>

**Reading 3.** `teardown` notes that dropping `Active` drops `stop_tx`, which
"unblocks a driver parked on the stop signal" (machine.rs:114-116). Find the
driver code that handles this and explain why it exits silently rather than
erroring.

<details><summary>Answer</summary>

The record loop's stop branch (machine.rs:537-545): `stop = &mut stop_rx`
matching `Err(_)` — a `RecvError` means the sender was dropped, which "only
[happens] when the slot tore this session down". The driver aborts its stream
and returns without emitting: supersession and cancel are silent by contract
(§5.1, §5.10), and the sender-drop *is* the ownership signal — no extra flag,
no extra channel. The resource's destructor is the notification.
</details>

**Break it.** In `Gate::is_dead` (machine.rs:91-93), return `false`
unconditionally:

```rust
fn is_dead(&self) -> bool {
    false
}
```

Run `cargo test -p app-core first_token_timeout_fires_and_late_deltas_are_suppressed`.

It fails: the test scripts `DetachedDeltas` — a provider task that outlives
the pipeline's interest — advances the paused clock past the 10 s first-token
cap so `fail()` emits `LlmFirstTokenTimeout`, then lets the detached task push
a straggler delta. With the latch broken, that delta becomes a visible
`llm:delta` *after* the error event, and the assertion that nothing paints
after the error trips. The test exists because this is not a theoretical race:
deltas arrive from the response-body task, timeouts fire on the driver task,
and only the one-way latch — checked on the emitting side, not the deciding
side — makes their interleaving irrelevant.
