# 10 — The IPC seam: envelopes, stale-dropping, and mirroring without re-enforcing

**Concept.** A process boundary (IPC, RPC, HTTP — the pattern is identical)
forces three design decisions most codebases make by accident:

1. **Errors as values, not exceptions.** An exception thrown across a
   boundary arrives untyped, unlocalized, and duplicated — every caller grows
   its own try/catch with its own guesses. A closed `{ok, value} | {ok:false,
   error:{code,message}}` envelope makes failure part of the signature, and a
   *closed set* of codes makes UI behavior exhaustive.
2. **Events for the many, return values for the one.** Most outcomes fan out
   as events — but exactly one outcome ("your request was refused, nothing is
   coming") can *only* travel as the call's return value, because refused work
   emits nothing. Miss this and your UI waits forever for an event that will
   never arrive.
3. **The client mirrors state; it never re-enforces it.** Both sides know the
   rules, but only one side may *act* on them. A frontend that re-runs the
   backend's decisions ("id must match", "recording caps at 120 s") will
   eventually act on stale information and fight its own server — lesson 07's
   cap bug was exactly this. The frontend's job is filtering and rendering:
   drop what is stale, adopt what is authoritative, enforce nothing twice.

**Where this repo stakes its life on it.** The Tauri boundary: `src/bridge.ts`
(the single place the UI touches Tauri), `src/state/reducer.ts` (the pure
mirror), `src/state/useSession.ts` (the wiring), and on the Rust side the
envelope/event mappers in `src-tauri/src/commands.rs` and `events.rs`.

## One error path, made in one place

```ts
// src/bridge.ts:28-42
/**
 * A rejected invoke (IPC breakage, missing command) would otherwise be a
 * second error path every caller has to remember to catch; folding it into
 * the envelope keeps exactly one.
 */
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<Envelope<T>> {
  try {
    return await invoke<Envelope<T>>(cmd, args);
  } catch (err) {
    return {
      ok: false,
      error: { code: 'internal', message: err instanceof Error ? err.message : String(err) },
    };
  }
}
```

The core already returns envelopes for *its* failures (§4 — "rather than
throwing across the boundary, validation errors included"); `call` folds the
*transport's* failures into the same shape, so `bridge.test.ts` can pin the
strongest property a client API can have: "never rejects: every command
method resolves even when invoke throws." Downstream code has one branch,
`env.ok`, and the error codes are a closed set the ErrorBox enumerates —
with `aborted` mapped to *render nothing*, because a user-initiated cancel
must never earn a banner (§4, `ErrorBox.test.tsx`).

Two adjacent details repay reading: `stopSession` sends `{ sessionId: id }`
because Tauri camelCases the Rust parameter name — "sending `{ id }` makes
every stop fail with a missing-key error the UI can only render as
'internal'" (bridge.ts:49-52; pinned by the bridge test that asserts exact
command names and argument shapes). And `cancelSession` is fire-and-forget
with a swallowed rejection (bridge.ts:54-58): "cancellation legitimately
races session teardown, so a rejection here is expected noise" — mirroring
the core's §4 contract that invalid cancel ids do nothing, never error.

## The one outcome that must be a return value

The stop contract (§4, §5.3) is the seam's sharpest edge. Every *successful*
stop outcome arrives later as events (`stt:partial`, `llm:delta`, `llm:done`).
But a stop that was *refused* — unknown id, already ended, already stopping,
still connecting — emits nothing, ever. So `StopOutcome::NotTaken` crosses the
boundary as an error envelope, and the hook acts on the return value:

```ts
// src/state/useSession.ts:89-92
const env = await bridge.stopSession(id);
// An error envelope means the stop was not taken: nothing will ever be
// emitted for this session, so sitting in "Finalizing…" would hang.
if (!env.ok) dispatch({ type: 'record/stopRejected', key, error: env.error });
```

The reducer's `stopRejected` (reducer.ts:216-221) unsticks the UI, retiring a
captured transcript rather than discarding it. Pinned end to end: core
`stop_unknown_id_is_not_taken` (return value, zero events), shell
`stop_not_taken_is_an_error_envelope`, frontend `stopRejected unsticks
finalizing` and `returns to idle immediately instead of hanging in
finalizing; transcript retired`.

## Stale-dropping: mirror the id, trust nothing else

Every event carries `{ sessionId }` (§4), and the reducer's filter is four
lines:

```ts
// src/state/reducer.ts:157-160
/** True when the event belongs to the session currently tracked. */
function isCurrent(state: SessionState, sessionId: SessionId): boolean {
  return state.activeId !== null && state.activeId === sessionId;
}
```

The `!== null` half encodes the subtler rule. Between dispatching
`record/start` and the start call resolving, `activeId` is null — events are
already arriving (the core emits the id before any network round-trip), but
the frontend hasn't *adopted* an id yet. Those events are dropped, **not
buffered**: "a buffered event could belong to a session the user already
superseded" (reducer.ts:42-48). Adoption itself is guarded by the attempt key
(lesson 02), so a superseded start's resolution can't install its id either.
The reducer returns the *same state object* for every dropped input, which is
what lets tests assert "ignored" by identity — a discipline worth stealing.

Now the mirroring rule, negatively stated: find what the reducer *doesn't* do.
It doesn't validate that a `done`'s transcript matches what it accumulated —
it overwrites with the core's authoritative copy (reducer.ts:332-337). It
doesn't refuse a delta arriving while `ui === 'recording'` — it treats it as
proof the core capped first, and adopts it (reducer.ts:297-317, lesson 07).
It doesn't re-check the 120 s cap against the core, doesn't time out sessions,
doesn't decide supersession outcomes — it *requests* (`cancelSession`,
`stopSession`) and then believes what the return values and surviving events
say. `stt:partial` replaces rather than appends (reducer.ts:284-294) because
the event is *defined* as "full transcript so far, not a delta" (§4) — the
mirror implements the contract's shape, not its own accumulation logic.

The same humility shows in the UI's smallest details: the style chips light
"the persisted style returned by the save, not the clicked chip" (§9) —
`lights the chip the SAVE returned` in App.test.tsx feeds a save that coerces
the value and asserts the UI follows the response. The click is a request;
the response is the truth.

## Exercises

**Reading 1.** `bridge.on` (bridge.ts:60-77) is eleven lines, and three of
them exist for one race. Which race, and why can't the caller handle it?

<details><summary>Answer</summary>

Tauri's `listen()` resolves *asynchronously* — so a component can unmount
(and call the returned unsubscribe) before registration completes. Without
the `disposed` flag, the late-resolving `un` would be stored into a variable
nobody will ever call again: a leaked listener that keeps applying events
"into an unmounted tree" (`unsubscribing before listen resolves still
detaches`, bridge.test.ts; `unmount unsubscribes every bridge listener`,
useSession.test.ts). The caller can't fix this because the caller never sees
the pending promise — the bridge returns a synchronous unsubscribe function,
so the bridge must absorb the async registration inside it. It is lesson 02's
claim-before-await in miniature: `disposed = true` is the claim; the `.then`
re-checks it at install time.
</details>

**Reading 2.** The event mapper test
`no_payload_ever_leaks_the_kind_tag` (src-tauri/src/events.rs) asserts the
serde `kind` tag is stripped from every payload. The enum is
`#[serde(tag = "kind")]` (core/src/session/mod.rs:54-56) — why does the tag
exist at all, and why must it not cross the boundary?

<details><summary>Answer</summary>

Inside the core, `SessionEvent` is one enum so the machine, the sink trait,
and the tests can treat "an event" as one type; serde needs the tag to make
the enum round-trip as JSON in core-side tests. But on the wire, the event
*name* (`stt:partial`, `llm:delta` — `event_name()`, mod.rs:88-97) already
carries the discriminant: Tauri delivers name and payload separately, and the
frontend's `EventMap` types each payload without a `kind` field. Leaking the
tag would "silently change every payload shape the UI destructures" — every
`EventMap[K]` type would be wrong, TypeScript wouldn't notice (extra fields
pass structural typing), and the drift would surface only when someone
destructured `kind` by accident. The shell test pins the wire shape as its
own contract, independent of how the core happens to model events internally
— which is exactly what a seam test is for.
</details>

**Break it.** In `reducer.ts` (line 158-160), make the filter trusting:

```ts
function isCurrent(state: SessionState, sessionId: SessionId): boolean {
  return true;
}
```

Run `npm test -- reducer` and watch two named guards fail: `drops events whose
sessionId is not the tracked session` (five event types with a wrong id must
return the identical state object) and `drops events arriving before the id
was adopted (activeId still null)`. Then run the useSession suite for the
integration-level casualties (`asking during a streaming answer supersedes it:
old session cancelled, its events dropped` — the zombie delta now appends to
the *new* entry).

The why is the whole seam in one failure: supersession (§5.1) is implemented
in the core by killing the loser's gate, but events already in flight through
the IPC queue — emitted before the kill, delivered after — are structurally
unstoppable. The id filter is the frontend's half of the supersession
contract: the core guarantees *at-most-one live emitter going forward*; the
frontend guarantees *stale deliveries change nothing*. Neither half suffices
alone, and the frontend half is mirroring at its purest — not re-deciding who
won, just refusing to listen to the loser. With it gone, a superseded
session's late `done` repaints "an answer the user already abandoned — the
single most confusing failure in v2" (machine.rs:1249-1251), except now on
the other side of the boundary where the core's tests can't see it.
