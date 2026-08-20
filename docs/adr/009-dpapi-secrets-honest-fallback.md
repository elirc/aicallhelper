# ADR 009 — DPAPI-encrypted secrets with an honest `plain:` fallback, decode-by-stored-prefix, fail-closed

**Status**: accepted

## Context

The API keys live in the same user-writable `settings.json` as everything else
(SPEC §8) — a file that gets backed up, copied between machines, and opened in
editors. Windows DPAPI is the zero-friction encryption option: per-user, no
master password, no key management. But three realities complicate "just
encrypt it":

1. The keystore can be **unavailable** (broken profiles, odd environments,
   non-Windows dev builds). Refusing to store keys then would brick the app's
   whole purpose.
2. DPAPI ciphertext is **per-user, per-machine**. A settings file copied from
   another machine holds blobs that will never decrypt here.
3. Users paste **raw keys straight into the JSON** by hand. Whatever the code
   does with that value, it will do silently.

## Decision

Stored secrets are prefixed strings, and the prefix is the whole protocol
(`src-tauri/core/src/store/secrets.rs:1-49`):

- `enc:<base64>` — DPAPI ciphertext (with `CRYPTPROTECT_UI_FORBIDDEN`, because
  this runs headless inside a settings save — `secrets.rs:63-65`).
- `plain:<base64>` — the fallback when the keystore is unavailable. **Honestly
  labeled** rather than silently pretending to be encrypted: the app keeps
  working and the file tells the truth about what it holds.
- **Decode dispatches on the *stored* prefix, never on current keystore
  availability** (`secrets.rs:36-49`): a machine that gained or lost DPAPI
  must still read what it wrote.
- **Anything undecodable reads as unset** — foreign-machine blobs, unknown
  prefixes, mangled base64, and hand-pasted raw keys all `None` out
  (`secrets.rs:44-48`, pinned at `secrets.rs:186-193`). Failing closed beats
  handing ciphertext to a provider as if it were a key, which produces a
  baffling 401 mid-call instead of a clear "add your key" nudge.

Two neighbouring rules complete the story:

- **Write-only across the UI boundary**: the frontend only ever learns
  `hasDeepgramKey`-style booleans (`store/settings.rs:561-569`); a patch that
  omits a key leaves it untouched, an empty-after-trim value clears it
  (`settings.rs:65-67`, `523`).
- **Atomic writes** protect the file the keys live in: write
  `settings.json.tmp`, flush, rename; update the in-memory cache only after
  the write lands (`settings.rs:1-13`, `129-137`). A crash mid-write must not
  truncate every setting — including the keys — into defaults.

## Consequences

- Keys are never greppable out of the settings file, on any platform, in any
  keystore state.
- A copied settings file degrades gracefully: everything survives except the
  keys, which read as unset and prompt the first-run nudge.
- Non-Windows builds (CI) route through the `plain:` path, so the whole store
  and its tests behave identically everywhere (`secrets.rs:126-137`).

Costs, honestly:

- **Fail-closed swallows information.** A raw key pasted into the JSON is
  simply "unset" — no error explains why. The password-field placeholder
  ("saved — type to replace") is the only truth surface, and exactly one
  power user will be surprised exactly once.
- DPAPI means no export, no sync, no recovery: reinstalling Windows means
  re-pasting keys. For two API keys, acceptable.
- The `plain:` fallback is base64, not encryption. It is deliberately not
  pretending otherwise — the alternative (refusing to work) punishes the user
  for their environment.

## If revisited

Moving secrets into Windows Credential Manager (or a keyring crate) would take
them out of the file entirely — worth it only if `settings.json` ever starts
traveling through sync or backup tooling, because it trades away the
single-atomic-file simplicity. The prefix protocol would survive as the
migration marker.
