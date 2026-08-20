//! Secret material at rest (§8).
//!
//! API keys live in the same user-writable settings.json as everything else,
//! so they are never written raw. The stored form is a prefixed string:
//!
//!   * `enc:<base64>`   — DPAPI ciphertext, bound to the current Windows user.
//!   * `plain:<base64>` — fallback when the OS keystore is unavailable.
//!     Honestly labeled rather than silently pretending to be encrypted; the
//!     app keeps working and the file tells the truth about what it holds.
//!
//! Decoding dispatches on the *stored* prefix, never on current keystore
//! availability — a machine that gained or lost DPAPI must still read what it
//! wrote. Anything that cannot be decoded (a settings file copied from another
//! machine, an unknown prefix, mangled base64) reads as *unset*: failing
//! closed beats handing ciphertext to a provider as if it were a key, which
//! produces a baffling auth error mid-call instead of a clear "add your key"
//! nudge.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

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

/// Decode a stored secret. `None` means "treat as unset" — the caller must
/// never fall back to using `stored` itself as the key.
pub fn unprotect(stored: &str) -> Option<String> {
    if let Some(b64) = stored.strip_prefix(ENC_PREFIX) {
        let blob = B64.decode(b64).ok()?;
        let plain = dpapi_unprotect(&blob)?;
        String::from_utf8(plain).ok()
    } else if let Some(b64) = stored.strip_prefix(PLAIN_PREFIX) {
        let bytes = B64.decode(b64).ok()?;
        String::from_utf8(bytes).ok()
    } else {
        // Unknown prefix — possibly a raw key pasted into the file by hand, or
        // a format from a future version. Either way we cannot vouch for it.
        None
    }
}

#[cfg(windows)]
fn dpapi_protect(data: &[u8]) -> Option<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let len = u32::try_from(data.len()).ok()?;
    let input = CRYPT_INTEGER_BLOB { cbData: len, pbData: data.as_ptr().cast_mut() };
    let mut output = CRYPT_INTEGER_BLOB::default();

    // UI_FORBIDDEN: this runs headless inside a settings save; a surprise
    // credential prompt would hang the write with no window to answer it.
    let result = unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if result.is_err() || output.pbData.is_null() {
        return None;
    }

    // Copy out before freeing: DPAPI allocates the blob with LocalAlloc and it
    // leaks on every save unless we release it ourselves.
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(HLOCAL(output.pbData.cast()));
    }
    Some(bytes)
}

#[cfg(windows)]
fn dpapi_unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let len = u32::try_from(blob.len()).ok()?;
    let input = CRYPT_INTEGER_BLOB { cbData: len, pbData: blob.as_ptr().cast_mut() };
    let mut output = CRYPT_INTEGER_BLOB::default();

    // A blob from another user or machine fails here by design — DPAPI is
    // per-user — and that failure is exactly the "read as unset" contract.
    let result = unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if result.is_err() || output.pbData.is_null() {
        return None;
    }

    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(HLOCAL(output.pbData.cast()));
    }
    Some(bytes)
}

// Non-Windows builds (CI, dev containers) have no DPAPI; reporting it as
// unavailable routes every write through the marked plain: fallback, so the
// whole store — and its tests — behave identically everywhere.
#[cfg(not(windows))]
fn dpapi_protect(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(windows))]
fn dpapi_unprotect(_blob: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_output_is_always_prefixed() {
        // The prefix is the decode dispatch; an unprefixed value would read as
        // unset forever.
        let stored = protect("sk-test-123");
        assert!(
            stored.starts_with(ENC_PREFIX) || stored.starts_with(PLAIN_PREFIX),
            "got: {stored}"
        );
    }

    #[test]
    fn protect_unprotect_round_trips() {
        for secret in ["sk-ant-abc123", "a", "key with spaces", "clé-☂-ключ"] {
            assert_eq!(unprotect(&protect(secret)), Some(secret.to_string()), "secret: {secret}");
        }
    }

    #[test]
    fn stored_form_never_contains_the_plaintext() {
        // Both encodings are at least base64; the raw key must not be
        // greppable out of the settings file.
        let stored = protect("secret-material-123");
        assert!(!stored.contains("secret-material-123"));
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_protect_uses_dpapi_not_the_fallback() {
        // If this fails, keys are silently landing on disk merely encoded, on
        // the one platform where real encryption is available.
        assert!(protect("sk-test").starts_with(ENC_PREFIX));
    }

    #[test]
    fn plain_prefix_decodes_by_stored_prefix_not_keystore_state() {
        // A file written on a DPAPI-less machine (or before an OS repair) must
        // stay readable even though protect() would choose enc: today.
        let stored = format!("{PLAIN_PREFIX}{}", B64.encode("copied-key"));
        assert_eq!(unprotect(&stored), Some("copied-key".to_string()));
    }

    #[test]
    fn garbage_and_unknown_prefixes_read_as_unset() {
        // The classic trap: a raw key pasted straight into the JSON. Handing
        // it back as-is would "work" — until the prefix logic changes — so it
        // is uniformly rejected instead.
        for bad in ["sk-raw-key-pasted-by-hand", "", "vault:abcd", "ENC:abcd", "enc"] {
            assert_eq!(unprotect(bad), None, "bad: {bad}");
        }
    }

    #[test]
    fn invalid_base64_reads_as_unset() {
        assert_eq!(unprotect("enc:!!!not-base64!!!"), None);
        assert_eq!(unprotect("plain:!!!not-base64!!!"), None);
    }

    #[test]
    fn undecryptable_enc_blob_reads_as_unset() {
        // Valid base64 that is not a DPAPI blob — the settings-file-copied-
        // from-another-machine case. Must fail closed, not surface ciphertext.
        let stored = format!("{ENC_PREFIX}{}", B64.encode("not a dpapi blob"));
        assert_eq!(unprotect(&stored), None);
    }

    #[test]
    fn plain_value_with_invalid_utf8_reads_as_unset() {
        let stored = format!("{PLAIN_PREFIX}{}", B64.encode([0xff_u8, 0xfe, 0x00, 0x01]));
        assert_eq!(unprotect(&stored), None);
    }
}
