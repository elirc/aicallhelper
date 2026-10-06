# Docs index

What each document is for, and which one to open first.

Path shorthand used throughout these docs: `core/src/...` means
`src-tauri/core/src/...` (the `app-core` crate); `src/...` is the frontend at
the repo root.

Two of these are **normative** — they pin behavior, and changing them is a
product decision:

- **[SPEC.md](SPEC.md)** — the product contract the app was built against.
  Code comments cite its section numbers (§5.5, §6.4, …). Where it pins an
  exact string, timeout, or ordering rule, that is load-bearing behavior, not
  a suggestion.
- **[TESTING.md](TESTING.md)** — every test in the repo (core, shell and
  frontend), what it verifies, and *why it exists* — the failure mode it
  guards. Also how each suite runs. The test counts live there and only
  there; every other document says "(count in TESTING.md)".

The rest explain, diagnose, or look forward:

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — how the pieces fit: the
  crate/module map and the path a question takes from Record press to
  streamed answer.
- **[adr/](adr/)** — architecture decision records: the choices that could
  have gone another way, and why they didn't. Sixteen so far, the newest
  two covering session outcomes/adoption and settings revisions.
- **[learn/](learn/)** — a guided tour of the codebase, for coming back to it
  cold after months away.
- **[TROUBLESHOOTING.md](TROUBLESHOOTING.md)** — symptom → cause → fix for
  things that break at runtime (audio, keys, hotkey, providers, window
  placement, free local voice).
- **[DEVELOPMENT.md](DEVELOPMENT.md)** — the dev loop: building, running,
  testing, the IPC surface and which thread each command runs on, and the
  recipes for adding a provider or changing the prompt.
- **[RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md)** — what must pass before a
  build is called dependable: the deterministic gates (also run by CI,
  [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)), the packaged
  NSIS build, and the manual checks on real hardware; plus the results
  ledger, the only place a hardware validation is recorded.
- **[SECURITY.md](SECURITY.md)** — the threat model and the rules that enforce
  it: DPAPI-encrypted keys, plaintext profiles, untrusted model output,
  loopback-only local services, nothing sensitive logged.
- **[IDEAS.md](IDEAS.md)** — the roadmap: ranked future features, feasibility
  sketched against this architecture, and the trap version of each one *not*
  to build.
- **[FREE_VOICE_MODE.md](FREE_VOICE_MODE.md)** — setting up and testing the
  free local voice mode (Ollama + Qwen3.5 2B for answers, Moonshine for
  English speech), its limits, and where its logs are. Its measured results
  are in [FREE_VOICE_TEST_RESULTS.md](FREE_VOICE_TEST_RESULTS.md); the model
  survey that chose those runtimes is [LOCAL_MODELS_REPORT.md](LOCAL_MODELS_REPORT.md).

## Where to start

- **Cold start / relearning the codebase** — root [README](../README.md) →
  [ARCHITECTURE.md](ARCHITECTURE.md) → [learn/](learn/) →
  [SPEC.md](SPEC.md) §5 (the session state machine is the crown jewels; the
  rest of the code exists to feed it).
- **Something is broken** — [TROUBLESHOOTING.md](TROUBLESHOOTING.md) first;
  if the symptom implicates an invariant, [SPEC.md](SPEC.md) says what the
  behavior *should* be and [TESTING.md](TESTING.md) names the test that pins
  it. Free local voice has its own section there and a setup guide in
  [FREE_VOICE_MODE.md](FREE_VOICE_MODE.md).
- **Changing behavior** — [SPEC.md](SPEC.md) first, always. If the change
  contradicts the spec, the spec gets edited in the same change — the code
  citing § numbers only works while the numbers tell the truth. Then find the
  pinning test in [TESTING.md](TESTING.md); if there isn't one, that's the
  first thing to write.
- **Deciding what to build next** — [IDEAS.md](IDEAS.md), which already ranks
  a top 3 and lists the anti-goals. The reviews of the v3.1 work, and the
  agreed actions from them, are indexed in
  [`fabledocs/README.md`](../fabledocs/README.md).
- **Cutting a release** — [RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md), top
  to bottom, then add a row to its ledger.
