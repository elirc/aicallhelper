# 08 — Fail-closed secrets and untrusted-input parsing: the store

**Concept.** Two rules govern client-side persistence, and both are about
choosing your failure direction in advance:

1. **Secrets fail closed.** Anything you cannot positively decode reads as
   *absent* — never "best effort", never pass-through. The alternative hands
   ciphertext (or a stranger's key, or a future format) to a downstream
   consumer as if it were the real thing, producing a failure far from its
   cause.
2. **Config fails open, per field.** A settings file is user-writable,
   survives upgrades, and gets edited by hand — it is untrusted *input*, not
   trusted *state*. One corrupt enum must cost the user that enum's default,
   not their resume and keys. Whole-file `Deserialize` with `?` is precisely
   the wrong tool: it converts one bad byte into total data loss.

Same store, opposite polarities, both deliberate. And underneath both: writes
must be **atomic**, because a half-written config file is indistinguishable
from a corrupt one — which rule 2 will then dutifully turn into defaults,
destroying everything.

**Where this repo stakes its life on it.** `src-tauri/core/src/store/` —
`secrets.rs` (DPAPI) and `settings.rs` (the file). The keys live *in* the
user-writable settings.json, so both rules apply to the same bytes.

## Secrets: prefix-dispatched, honestly labeled

```rust
// secrets.rs:22-32
pub const ENC_PREFIX: &str = "enc:";
pub const PLAIN_PREFIX: &str = "plain:";

/// Encode a secret for storage. Never fails: when DPAPI is unavailable the
/// marked plaintext fallback keeps the feature working.
pub fn protect(plaintext: &str) -> String {
    match dpapi_protect(plaintext.as_bytes()) {
        Some(blob) => format!("{ENC_PREFIX}{}", B64.encode(blob)),
        None => format!("{PLAIN_PREFIX}{}", B64.encode(plaintext.as_bytes())),
    }
}
```

The `plain:` fallback is the honest half of the design: if the OS keystore is
unavailable, the file *says so* rather than "silently pretending to be
encrypted" (secrets.rs:7-9). Decoding dispatches on the **stored** prefix,
never on current keystore availability (secrets.rs:11-13) — "a machine that
gained or lost DPAPI must still read what it wrote"
(`plain_prefix_decodes_by_stored_prefix_not_keystore_state`).

And the closed-fail is total:

```rust
// secrets.rs:36-49 (abridged)
pub fn unprotect(stored: &str) -> Option<String> {
    if let Some(b64) = stored.strip_prefix(ENC_PREFIX) {
        let blob = B64.decode(b64).ok()?;
        let plain = dpapi_unprotect(&blob)?;
        String::from_utf8(plain).ok()
    } else if let Some(b64) = stored.strip_prefix(PLAIN_PREFIX) {
        ...
    } else {
        // Unknown prefix — possibly a raw key pasted into the file by hand, or
        // a format from a future version. Either way we cannot vouch for it.
        None
    }
}
```

Every arrow points to `None`: bad base64, a DPAPI blob from another user or
machine (DPAPI is per-user *by design* — the copied-settings-file case,
secrets.rs:101-103), invalid UTF-8, unknown prefix. The doc comment states the
caller contract: "`None` means 'treat as unset' — the caller must never fall
back to using `stored` itself as the key" (secrets.rs:34-35). Why so absolute,
even for the tempting raw-pasted-key case? The test's why-line answers:
"handing a raw pasted key back 'works' until the prefix logic changes —
uniform rejection is safer" (`garbage_and_unknown_prefixes_read_as_unset`).
Downstream, the cost of failing open is concrete: "a baffling auth error
mid-call instead of a clear 'add your key' nudge" (secrets.rs:15-17).

The DPAPI FFI itself (secrets.rs:52-124) is worth reading once as a model of
careful unsafe: `CRYPTPROTECT_UI_FORBIDDEN` because "a surprise credential
prompt would hang the write with no window to answer it"; copy-then-`LocalFree`
because DPAPI allocates with LocalAlloc and "it leaks on every save" otherwise.

## The settings file: one question per field

```rust
// settings.rs:168-171
/// Parse file contents with per-field fallback. Every arm answers the same
/// question: "what does the user lose if only this value is garbage?" — and
/// the answer must always be "only this value".
fn settings_from_disk(text: &str) -> Settings { ... }
```

Read the arms (settings.rs:171-207) with that question in mind. Each field
gets its own tiny policy, and the differences are the design:

- **hotkey** distinguishes absent from empty (settings.rs:215-225): a
  present-but-empty string is "a choice — 'shortcut disabled' — and must not
  spring back to the default"; an *absent* field means "never configured" and
  does. Absent ≠ empty is a distinction serde flattens by default; here it is
  user intent (`empty_hotkey_means_disabled_and_never_reverts_to_default`).
- **window bounds drop as a unit** (settings.rs:196-206): "a good width welded
  to a defaulted height is a shape the user never chose". Per-field fallback
  has a grain size, and the grain is *the semantic unit*, not the JSON leaf.
  The explicit `is_object()` check is its own micro-lesson: "serde will
  happily read a struct out of a JSON array (`[1,2,3,4]` 'works'), and this
  app never writes one, so a non-object here is corruption by definition."
- **profile text is capped but never trimmed** (settings.rs:209-213), and the
  cap counts *characters*, not bytes — "a byte cut can split a UTF-8 sequence
  and turn a resume into invalid data on the next load" (settings.rs:153-157).

The headline test is `corrupt_answer_style_never_costs_the_resume_or_the_keys`
— "the named disaster from the spec": one bad enum string must not read as
"corrupt file" and wipe everything.

## Atomic writes, and cache-follows-disk

```rust
// settings.rs:129-137
// Write and flush the temp file completely before the rename, so the
// real file only ever transitions between two complete states — a
// half-written settings.json reads as "defaults" and silently destroys
// every setting including the keys.
let mut file = fs::File::create(&self.tmp_path)?;
file.write_all(&bytes)?;
file.sync_all()?;
drop(file);
fs::rename(&self.tmp_path, &self.path)
```

Write-to-temp-then-rename is the classic, but note the two companions that
make it complete. First, only `settings.json` is ever *read* — a stale `.tmp`
from a crashed write is dead data the next save overwrites
(settings.rs:46-48; `a_stale_tmp_file_does_not_corrupt_a_load_or_the_next_save`).
Second, **disk first, cache second** (settings.rs:97-102): the in-memory copy
updates only after the rename lands, "so a failed write leaves memory matching
disk" — the UI must never show settings that will silently vanish on restart
(`failed_patch_reports_an_error_and_leaves_memory_matching_disk`). The one
exception proves the rule: `save_window_bounds` (settings.rs:105-117) swallows
failures entirely, because it runs during shutdown and geometry is cosmetic —
"the only acceptable outcomes are 'saved' or 'silently didn't'".

Two more boundary rules complete the picture: cleared keys are *omitted* from
the JSON, never written as `""` ("an absent field and an unset key must mean
the same thing on the next load", settings.rs:242-243), and the view that
crosses the IPC boundary carries only `has*Key` booleans — key material never
reaches the webview (`view_never_exposes_key_material`, §8's write-only rule).

## Exercises

**Reading 1.** `apply_key_patch` (settings.rs:159-166) implements three-way
semantics: `None` leaves the stored key, empty-after-trim clears it, anything
else replaces it trimmed. Why can't the patch just carry the new value, with
`""` meaning "no change"?

<details><summary>Answer</summary>

Because the UI never *has* the current value to send back — keys are
write-only across the boundary, so every save of an untouched form would have
to either resend the key (it can't; it only knows `hasDeepgramKey: true`) or
clear it. `Option<String>` puts "didn't touch it" (`None`) and "deliberately
cleared it" (`Some("")`) in different variants, which is exactly the
distinction the settings form preserves — it "omits untouched key fields but
always sends the rest of the form" (SettingsView.test.tsx). The trim matters
too: pasted keys carry clipboard whitespace, and "an untrimmed key fails at
the provider with a confusing mid-call auth error" (`key_patch_values_are_trimmed`)
— the same fail-far-from-cause smell rule 1 exists to prevent.
</details>

**Reading 2.** `key_field` (settings.rs:227-232) chains `unprotect` with
`.filter(|k| !k.is_empty())`. `unprotect` already rejects everything
undecodable — what residual case does the filter close, and what would it
break?

<details><summary>Answer</summary>

A *validly encoded empty string*: `plain:` + base64 of `""` decodes cleanly to
`Some("")`. Without the filter, that loads as a present-but-empty key —
`has*Key` reports true to the UI ("would make has*Key lie", the comment
says), the first-run nudge doesn't show, and the provider call fails with an
auth error instead of the gate check `no_llm_key`/`no_stt_key` firing (both
providers check for an empty key *before* dialing —
`empty_api_key_returns_no_llm_key_without_connecting`). Fail-closed includes
the degenerate success: a decodable nothing is still nothing.
</details>

**Break it.** In `unprotect` (secrets.rs:44-48), make the unknown-prefix arm
helpful:

```rust
} else {
    Some(stored.to_string())
}
```

Run `cargo test -p app-core garbage_and_unknown_prefixes_read_as_unset`.

It fails on the raw-pasted-key case (and the unknown/case-wrong prefixes): the
"helpful" arm returns the stored string as the key. Notice what makes this
edit seductive — it *fixes a real user*, the one who hand-pasted a raw key
into settings.json, and it will appear to work in manual testing. The test's
why-line is the counterargument in one sentence: it "works" until the prefix
logic changes — at which point every stored value that fails to decode
(foreign-machine `enc:` blobs, future formats, mangled base64 that happens to
lack a known prefix) flows to a provider as an Authorization header, and the
user gets a 401 mid-interview with no path back to the actual cause. Uniform
rejection costs the hand-editor a re-paste in Settings; failing open costs
everyone else a debugging session pointed at the wrong system.
