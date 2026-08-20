# ADR 011 — A closed event set with sessionId tagging + frontend stale-drop as the supersession mechanism

**Status**: accepted

## Context

Supersession (ADR 002) is only half a mechanism until the *receiving* side can
tell fresh events from stale ones. Events cross an async IPC boundary: by the
time the frontend handles an `llm:delta`, the session that produced it may
have been superseded, cancelled, or errored — and some of its events may
already be sitting in the queue. The core's emission gate kills events at the
source, but it cannot recall an event already emitted. v2's signature bug — a
superseded session's `done` repainting an answer the user abandoned — lived
exactly in that gap.

There is also a subtler window: events can arrive *before* the frontend's own
`start`/`ask` invoke has resolved with the session id, i.e. before the
frontend even knows which id it is tracking (SPEC §9 "events arriving before
the start/ask call resolved are dropped").

## Decision

- **A closed set of five events**, every one tagged `{ sessionId }`
  (SPEC §4; `src-tauri/core/src/session/mod.rs:52-98`): `stt:partial`,
  `llm:delta`, `llm:done`, `session:error`, `audio:level`. The wire mapping
  strips the serde `kind` tag so the event *name* carries the type and the
  payload stays the bare object the UI destructures
  (`src-tauri/src/events.rs:17-24`, pinned by `events.rs:137-156`).
- **The frontend adopts an id only from its own invoke resolution** — never
  from an event (`src/state/useSession.ts:54-77`,
  `src/state/reducer.ts:189-198`). Until adoption, `activeId` is null and
  `isCurrent` rejects everything (`reducer.ts:157-160`).
- **Every event handler drops non-current ids** (`reducer.ts:279-346`). This
  is the frontend's half of supersession: even if a stale event slips past the
  core's gate — or was queued before the gate died — it changes nothing.
- Attempt keys handle the in-flight window on the command side: a
  `startSession` that resolves after the user aborted or superseded finds its
  attempt key stale and cancels the orphan session instead of adopting it
  (`useSession.ts:44-48`, `66-71`).
- Two design choices keep dropping *safe*: `stt:partial` carries the **full
  transcript so far, not a delta** — replace, don't append
  (`reducer.ts:284-295`) — so a dropped or coalesced partial costs nothing;
  and `audio:level` is stateless per frame.

## Consequences

- Supersession is enforced twice, independently: the core's slot-ownership
  gate (ADR 002) and the frontend's id-drop. Either alone has a hole (the
  gate can't recall queued events; the frontend can't stop core-side work);
  together the v2 repaint bug is structurally impossible.
- The closed event set is what makes the frontend fully testable: the entire
  UI is driven through a fake bridge emitting exactly these five shapes
  (docs/TESTING.md, frontend section).
- `llm:delta` is the one append-only event, which is why the retry
  concatenation guard (ADR 006) and the delta-suppression-after-error rule
  (§5.9, gate killed before the error emits — `machine.rs:175-183`) both
  exist: appending is unforgiving of duplicates.

Costs, honestly:

- Adding an event is a four-place change: core enum, wire test, TS type, UI
  handler. Fine at five events; the friction is the contract working.
- Full-transcript partials trade bandwidth for idempotence — each partial
  re-sends everything heard so far. At speech rates this is noise.
- The id-drop rule has one deliberate exception carved *through* it: deltas
  arriving while the UI still thinks it is `recording` are accepted for the
  current id, because they are the only signal that the core's 120 s cap beat
  the local clock (ADR 012, `reducer.ts:297-306`).

## If revisited

If the event set grew or ordering ever mattered across event types, a
per-session monotonic sequence number in the envelope would subsume the
id-drop and add gap detection. At five events with one appender, that is
machinery without a customer.
