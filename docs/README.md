# Docs index

What each document is for, and which one to open first.

Two of these are **normative** — they pin behavior, and changing them is a
product decision:

- **[SPEC.md](SPEC.md)** — the product contract the app was built against.
  Code comments cite its section numbers (§5.5, §6.4, …). Where it pins an
  exact string, timeout, or ordering rule, that is load-bearing behavior, not
  a suggestion.
- **[TESTING.md](TESTING.md)** — every test in the repo (233 core + 34 shell +
  290 frontend), what it verifies, and *why it exists* — the failure mode it
  guards. Also how each suite runs.

The rest explain, diagnose, or look forward:

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — how the pieces fit: the
  crate/module map and the path a question takes from Record press to
  streamed answer.
- **[adr/](adr/)** — architecture decision records: the choices that could
  have gone another way, and why they didn't.
- **[learn/](learn/)** — a guided tour of the codebase, for coming back to it
  cold after months away.
- **[TROUBLESHOOTING.md](TROUBLESHOOTING.md)** — symptom → cause → fix for
  things that break at runtime (audio, keys, hotkey, providers, window).
- **[DEVELOPMENT.md](DEVELOPMENT.md)** — the dev loop: building, running,
  testing, releasing.
- **[SECURITY.md](SECURITY.md)** — the threat model and the rules that enforce
  it: DPAPI-encrypted keys, untrusted model output, nothing sensitive logged.
- **[IDEAS.md](IDEAS.md)** — the roadmap: ranked future features, feasibility
  sketched against this architecture, and the trap version of each one *not*
  to build.

## Where to start

- **Cold start / relearning the codebase** — root [README](../README.md) →
  [ARCHITECTURE.md](ARCHITECTURE.md) → [learn/](learn/) →
  [SPEC.md](SPEC.md) §5 (the session state machine is the crown jewels; the
  rest of the code exists to feed it).
- **Something is broken** — [TROUBLESHOOTING.md](TROUBLESHOOTING.md) first;
  if the symptom implicates an invariant, [SPEC.md](SPEC.md) says what the
  behavior *should* be and [TESTING.md](TESTING.md) names the test that pins
  it.
- **Changing behavior** — [SPEC.md](SPEC.md) first, always. If the change
  contradicts the spec, the spec gets edited in the same change — the code
  citing § numbers only works while the numbers tell the truth. Then find the
  pinning test in [TESTING.md](TESTING.md); if there isn't one, that's the
  first thing to write.
- **Deciding what to build next** — [IDEAS.md](IDEAS.md), which already ranks
  a top 3 and lists the anti-goals.
