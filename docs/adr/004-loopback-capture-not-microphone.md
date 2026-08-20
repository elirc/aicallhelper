# ADR 004 — Loopback capture of the render device, not the microphone

**Status**: accepted

## Context

The audio worth transcribing is what the *other person on the call* is saying.
On the user's machine that audio exists in exactly one place: the playback
path. The microphone hears the wrong things — the user's own voice, room
noise — and on a headset it may never hear the far end at all. Any
"join-the-call bot" approach was off the table: this tool must work with
Zoom/Teams/Meet/phone-through-speakers without touching the call (SPEC §1).

Windows offers WASAPI loopback for this. cpal 0.15 exposes it implicitly:
building an *input* stream on a *render* device sets
`AUDCLNT_STREAMFLAGS_LOOPBACK` — verified against cpal's WASAPI backend source
(`src-tauri/core/src/audio/capture.rs:1-9`).

## Decision

Capture the **default output device**, at its own mix format, and normalize in
software:

- `open_loopback_stream` asks for the default *output* device and opens an
  input stream on it (`capture.rs:131-150`). The device's mix format is
  whatever it is — f32/i16/u16, any rate, any channel count — and the
  `Resampler` absorbs it (`src-tauri/core/src/audio/resample.rs`).
- Downmix averages the **speech channels only** (FL, FR, FC — the first three
  in WAVEFORMATEXTENSIBLE order): on a 5.1/7.1 device an equal average over
  all channels divides the voice by 6–8, quiet enough to cost transcription
  accuracy (`resample.rs:97-119`).
- WASAPI keeps capturing from the *original* device after the user switches
  defaults (plugs in a headset), and cpal surfaces no event for the switch —
  so the stream-owning thread polls the default device's identity every 2 s
  and surfaces one honest error when it moves (`capture.rs:217-288`).
- Device deaths route through the state machine, not straight to the UI: the
  machine is phase-aware (a device death after stop must not kill an answer
  that is already streaming), owns one-error-per-session, and drops stale ids
  (`src-tauri/src/state.rs:63-70`, `machine.rs:355-390`).
- The realtime data callback does nothing but convert, frame, and `try_send`
  over a bounded channel — RMS and sink calls happen on a plain forwarder
  thread (`capture.rs:97-105`, `312-318`).

## Consequences

- Works with every call app, requires nothing of the call, and works with
  headphones — the capture is "what the user hears", which is exactly the
  product definition.
- The far end arrives clean: no room echo, no user's own voice contaminating
  the transcript.

Costs, honestly:

- **All system audio is captured**, not just the call: a notification chime or
  background music lands in the transcript. The user owns keeping other audio
  quiet while recording.
- Volume-coupled: mute the speakers and there is nothing to capture. The
  `no_speech` message points at this ("Make sure call audio is playing").
- The default-device switch detection is a poll, because the platform offers
  no event through cpal — a ~ms COM enumeration every 2 s, plus a 2 s worst
  case of silently recording the wrong device.
- Deeply Windows-specific. The `NullCapture` fallback exists so tests and
  headless machines run at all (`capture.rs:342-349`).

## If revisited

Windows 10 2004+ offers **process loopback**
(`ActivateAudioInterfaceAsync` + `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`):
capture only the call app's audio, immune to notification chimes and the
default-device problem. It costs a process picker (which app is the call?)
and leaves cpal for raw WASAPI. Worth doing only if mixed-in system sounds
measurably hurt transcripts in practice.
