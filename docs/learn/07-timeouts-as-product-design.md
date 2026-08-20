# 07 — Timeouts as product design: the §3 budget and the 120 s cap bug

**Concept.** Timeouts are usually treated as configuration — someone picks
30 s, it lives in a constant, nobody can say why. In a latency-promised
product they are *design*: each stage gets a budget derived from what the user
was promised, each timer is measured from a product-meaningful instant, each
expiry maps to a *specific* error the user can act on, and progress **disarms**
the timers that no longer describe a risk. And when two components both keep a
clock — a core and a UI — exactly one of them may be authoritative, because
two clocks that both act is a race with a user in the middle.

**Where this repo stakes its life on it.** The promise is ~1 s
stop-to-first-word (§1). The budget (§3), all enforced in the core:

```rust
// core/src/session/mod.rs:130-146
pub const STT_FINALIZE: Duration = Duration::from_secs(5);
pub const STT_CONNECT: Duration = Duration::from_secs(5);
pub const LLM_FIRST_TOKEN: Duration = Duration::from_secs(10);
pub const LLM_TOTAL: Duration = Duration::from_secs(60);
pub const MAX_RECORDING: Duration = Duration::from_secs(120);
```

## Which timer is armed when

```
Record ─────────────────────────── Stop ──────────────────────────── Done
│                                   │
│ MAX_RECORDING (120 s) ──────────► │  ◄── the latency clock starts here
│ STT_CONNECT (5 s) ─► (connect)    │
                                    │ STT_FINALIZE (5 s) ─────►
                                    │ LLM_FIRST_TOKEN (10 s) ──────► (disarmed by first delta)
                                    │ LLM_TOTAL (60 s) ─────────────────────────►
```

- **STT_CONNECT** wraps the connect await (machine.rs:495) *and* the driver's
  own dial (deepgram.rs:193) — "a hung connect with no cap is a Record button
  that never answers and never fails" (`connect_timeout_surfaces_stt_connect`).
- **MAX_RECORDING** is pinned at the top of the record loop
  (machine.rs:526-527) — armed the moment recording begins, which becomes the
  crux of the case study below.
- Everything else is measured from **one captured instant**: `stopped_at`,
  sent through the stop oneshot *under the slot lock* (machine.rs:250-256,
  lesson 01). Deadlines are then absolute:

```rust
// machine.rs:590, 695-696
let finalize_deadline = stopped_at + limits::STT_FINALIZE;
...
let first_token_deadline = stopped_at + limits::LLM_FIRST_TOKEN;
let total_deadline = stopped_at + limits::LLM_TOTAL;
```

`stopped_at + LIMIT`, not `now() + LIMIT` — the budget is charged from when
the *user* stopped, so time spent finalizing eats into the LLM's first-token
allowance. That is a product statement: the user was promised
stop-to-first-word, and no stage gets to reset the clock in its own favor.
Conversely, recording time contributes *nothing*
(`metrics_are_measured_from_the_stop_instant`: "recording time is the user
talking, not us working — counting it makes a long question look like a slow
answer").

## Disarm-on-progress, and one error per expiry

```rust
// machine.rs:708-716 (run_answer's select, abridged)
first = &mut first_rx, if first_armed => {
    first_armed = false;
    // The first delta both timestamps the headline metric and
    // DISARMS the first-token timeout (§5.9): once tokens flow,
    // only the total cap applies.
    if let Ok(at) = first {
        first_token_at = Some(at);
    }
}
_ = tokio::time::sleep_until(first_token_deadline), if first_token_at.is_none() => { ... }
```

The first-token timer answers exactly one question — *has the model started?*
— so the first delta makes it meaningless, and the `if first_token_at.is_none()`
guard removes the branch from the select. v2 got this wrong and "truncated
healthy answers at the 10 s mark" (`first_delta_disarms_the_first_token_timeout`).
The total timer, by contrast, ignores progress on purpose: "an answer that
trickles forever is worse than a clean failure — the total cap runs regardless
of progress" (`total_timeout_reports_llm_timeout`).

Each expiry has its own code — `SttTimeout`, `LlmFirstTokenTimeout`,
`LlmTimeout` — because "a hung flush must fail with its own code or the user
debugs the wrong stage" (`finalize_timeout_reports_stt_timeout`). And every
timeout arm cancels the in-flight work *then* fails through the slot-owned
path (machine.rs:717-740), so lesson 01's gate suppresses any delta that
races the error.

The budget is also *defended*, not just enforced: prewarm fires on start, on
stop (synchronously, before the finalize await — machine.rs:262-266), and on
ask, so "the TLS handshake overlaps the STT flush" instead of landing inside
the latency window (`prewarm_fires_on_start_stop_and_ask` asserts the stop
prewarm happens *before* finalize resolves).

## Case study: the 120 s cap, or why exactly one clock may act

The cap is "a stop, not a failure" (§3: "auto-stop, then answer normally"):
the core's `MAX_RECORDING` branch
flips the slot to Finishing, prewarms, and answers normally
(machine.rs:559-584) — behaving "exactly like a user stop". It emits **no
event** for the auto-stop itself.

The frontend keeps its own recording clock for the mm:ss timer, so it too
crosses 120 s — later than the core (its clock starts after the IPC
round-trip) and unreliably (WebView2 throttles background timers). The old
frontend, on crossing the cap, did the obvious thing: issued `stop_session`.
The core answered `NotTaken` — its own cap had already taken the stop — and
the frontend treated the refusal as failure and tore down,
"null[ing] activeId and dropp[ing] every capped recording's answer"
(reducer.ts:264-276). Two clocks, both acting, one user-visible disaster.

The fix assigns authority. The core is authoritative (its timer is armed
first, and only it *can* stop the pipeline); the frontend cap becomes purely
cosmetic:

```ts
// src/state/reducer.ts:276 (the cap tick)
return { ...state, elapsedMs, ui: 'finalizing', rms: 0, hitRecordingCap: true };
```

— flip the label, latch the explanation flag, **keep `activeId`/`liveKey`**,
issue nothing, and let the core's answer events land. And because even the
cosmetic clock can lose the race, the reducer accepts answer events while
still nominally `recording`:

```ts
// src/state/reducer.ts:298-303
// 'recording' is accepted too ... a delta while we still think we are
// recording means the core beat our clock to the 120 s auto-stop (§3)
// without emitting an event for it. The delta itself is the authoritative
// "recording over" signal — dropping it is how a capped recording would
// lose its answer.
```

Pinned on both sides: core
`max_recording_auto_stops_and_answers_normally` (auto-stop prewarms like a
user stop; a later manual stop is `NotTaken`; metrics count from the auto-stop
instant), frontend `caps locally at 120 s: NO stop issued, the core auto-stop
answer lands intact`, `caps at true wall-clock 120 s even when timer fires are
throttled and lumpy`, and `adopts an answer delta arriving while still
"recording"` (useSession.test.ts).

## Exercises

**Reading 1.** The cap branch in the core (machine.rs:559-584) is unusually
paranoid: on firing it first `try_recv`s the stop channel, then calls
`begin_finishing`, and if *that* fails, `try_recv`s again. Reconstruct the two
races this dance closes.

<details><summary>Answer</summary>

Race one: a user stop and the cap timer fire in the same instant, stop first —
the stop already flipped the phase and sent the instant, so the first
`try_recv` finds it and the driver proceeds as a normal stop (the cap defers
to the user's timestamp). Race two: the cap wins the select but a stop is
accepted *between* the select resolving and the cap acting —
`begin_finishing` fails (phase is already Finishing), and because "the instant
was sent under the lock, it is here" (machine.rs:573-574), the second
`try_recv` is guaranteed to find the user's instant. `Err(Closed)` on either
recv means the slot tore the session down — exit silent. Every branch resolves
to exactly one of: user's instant, cap's instant, or silence; there is no path
where both act. It is lesson 01's discipline (state transitions under the
lock, instants travel with them) paying off in the nastiest corner.
</details>

**Reading 2.** The finalize cap appears twice: `finalize()` waits at most
`STT_FINALIZE` on the done-watch (deepgram.rs:160), and the driver caps its
own drain with the same constant (deepgram.rs:396-398) — *and* the session
machine holds its own `finalize_deadline` select branch (machine.rs:628-641).
Why three enforcement points for one 5 s budget?

<details><summary>Answer</summary>

Different failure domains. The machine's deadline is the *authoritative*
product timeout: it fires `SttTimeout` to the user and aborts the stream — it
must exist even if the STT implementation is buggy, because the machine can
only trust its own clock (the trait boundary means `finalize()` could hang
forever). The handle-side timeout keeps `finalize()` itself from parking
eternally on a driver that never signals done — defense against the same hang
from inside the implementation, returning "whatever it has". The driver-side
cap on the drain prevents a *task leak*: without it, a server that never
closes leaves the driver alive after everyone stopped caring
(deepgram.rs:395-397 — "this task must not linger forever"). One budget,
three owners: product error, caller liveness, resource cleanup.
</details>

**Break it.** In `run_answer` (machine.rs:717), remove the disarm guard from
the first-token branch:

```rust
_ = tokio::time::sleep_until(first_token_deadline) => {
```

Run `cargo test -p app-core first_delta_disarms_the_first_token_timeout`.

It fails: the test scripts a delta just inside the 10 s cap and later deltas
beyond it. With the guard gone, the deadline branch stays armed after tokens
flow, fires at 10 s into a healthy streaming answer, cancels it, and emits
`LlmFirstTokenTimeout` — the v2 bug, verbatim: an answer the user is actively
reading truncated by a timer that had already been answered. The paused-clock
harness makes this deterministic (`advance` walks virtual time through each
scripted delay in order). The test's *why* line is the whole lesson in one
sentence: "once tokens flow only the total cap applies" — a timeout is a
question, and a question that has been answered must be disarmed, not merely
tolerated.
