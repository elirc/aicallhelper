//! Origin pre-warm (§6.4).
//!
//! The answer request fires the instant the user presses Stop, and
//! stop-to-first-word is the number this whole app lives or dies by. A cold
//! request spends its opening milliseconds on DNS + TCP + TLS handshakes
//! before a single byte of the question even leaves the machine. Warming at
//! Record time performs that handshake early through `shared_client()`, so the
//! answer request fired right after Stop finds a live pooled TLS connection
//! and spends its entire budget on the model instead of on transport setup.
//!
//! Everything here is shaped by "best effort, zero cost on failure":
//!
//! * Fire-and-forget — `prewarm` returns immediately; nothing on the Record
//!   path may wait on the network.
//! * The response body is read to completion. Dropping a response mid-body
//!   makes reqwest close the connection instead of returning it to the pool,
//!   which defeats the warm while looking like it worked.
//! * Throttled per origin: Record can be pressed rapid-fire (re-asks,
//!   fat-fingers) and each press warms; without a throttle that is a burst of
//!   pointless requests at the provider.
//! * A failed warm must never panic, propagate, or log loudly — the answer
//!   path works fine without it, just slower, and noise here would train the
//!   user to ignore errors that matter.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::llm::http::shared_client;

/// One warm per origin per this window. Long enough to absorb a rapid-fire
/// Record burst, short enough that a genuinely new recording session still
/// re-warms a connection the pool may have dropped.
const WARM_THROTTLE: Duration = Duration::from_secs(2);

/// A warm that hasn't finished in 3s is talking to an origin that is down or
/// unreachable; holding the task and socket longer buys nothing.
const WARM_TIMEOUT: Duration = Duration::from_secs(3);

/// Last warm time per origin. Process-global because `prewarm` is free-function
/// fire-and-forget with no owner to hang state off.
fn last_warm() -> &'static Mutex<HashMap<String, Instant>> {
    static MAP: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The throttle decision, split out with an explicit `now` so tests can drive
/// the clock instead of sleeping. Returns true (and records `now`) when a warm
/// for `origin` should fire.
pub(crate) fn should_warm(origin: &str, now: Instant) -> bool {
    let mut map = match last_warm().lock() {
        Ok(guard) => guard,
        // A panic in some other holder must not disable warming for the rest
        // of the process — and this path itself must never panic.
        Err(poisoned) => poisoned.into_inner(),
    };
    match map.get(origin) {
        // `duration_since` saturates to zero if the clock ever reads earlier,
        // so this comparison cannot panic.
        Some(&last) if now.duration_since(last) < WARM_THROTTLE => false,
        _ => {
            map.insert(origin.to_string(), now);
            true
        }
    }
}

/// Warm `origin` (e.g. `https://api.anthropic.com`): open (or refresh) a
/// pooled TLS connection by fetching `<origin>/v1/models` in the background.
///
/// Unauthenticated on purpose: a 401 (or 404) completes the TCP + TLS
/// handshake just as well as a 200, which is all the warm is for — and the
/// user's API key has no business travelling on a fire-and-forget request
/// whose response nobody reads.
pub fn prewarm(origin: &str) {
    if !should_warm(origin, Instant::now()) {
        return;
    }

    // `tokio::spawn` panics outside a runtime; a warm must never panic, so a
    // missing runtime just means no warm.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };

    let url = format!("{}/v1/models", origin.trim_end_matches('/'));
    handle.spawn(async move {
        // Per-request timeout — the shared client deliberately has no global
        // one (see http.rs), so the cap must ride on this request.
        let response = shared_client().get(&url).timeout(WARM_TIMEOUT).send().await;
        if let Ok(response) = response {
            // Drain to completion so the connection goes back to the pool
            // (the body is tiny: a model list or a 401 JSON error). The
            // result itself is meaningless — the connection was the point.
            let _ = response.bytes().await;
        }
        // Errors are swallowed silently: a failed warm costs nothing but the
        // handshake the answer request would have paid anyway.
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // The throttle map is process-global, so every test uses its own origin
    // strings — reuse across tests would couple them through shared state.

    #[test]
    fn throttle_blocks_within_two_seconds_then_reopens() {
        let base = Instant::now();
        let origin = "https://throttle-window.test";

        assert!(should_warm(origin, base), "first warm for an origin always fires");
        assert!(!should_warm(origin, base + Duration::from_millis(1)));
        assert!(
            !should_warm(origin, base + Duration::from_millis(1999)),
            "still inside the window"
        );
        assert!(
            should_warm(origin, base + Duration::from_secs(2)),
            "window elapsed, warm again"
        );
    }

    #[test]
    fn denied_warms_do_not_extend_the_window() {
        // Only a warm that actually fires records a timestamp. If denials
        // refreshed it, rapid-fire Record presses would starve warming
        // forever — the opposite of what the throttle is for.
        let base = Instant::now();
        let origin = "https://no-extend.test";

        assert!(should_warm(origin, base));
        assert!(!should_warm(origin, base + Duration::from_millis(1900)));
        // 2s after the *fired* warm, not after the denied one.
        assert!(should_warm(origin, base + Duration::from_secs(2)));
    }

    #[test]
    fn origins_are_throttled_independently() {
        // Anthropic and Groq are different origins; warming one must not
        // suppress warming the other when the user switches provider.
        let base = Instant::now();

        assert!(should_warm("https://origin-a.test", base));
        assert!(should_warm("https://origin-b.test", base));
        assert!(!should_warm("https://origin-a.test", base + Duration::from_secs(1)));
        assert!(!should_warm("https://origin-b.test", base + Duration::from_secs(1)));
    }

    #[test]
    fn a_fired_warm_resets_its_own_window() {
        let base = Instant::now();
        let origin = "https://window-resets.test";

        assert!(should_warm(origin, base));
        // Fires at +2s and records that as the new reference point…
        assert!(should_warm(origin, base + Duration::from_secs(2)));
        // …so +3s is only 1s after the last fired warm.
        assert!(!should_warm(origin, base + Duration::from_secs(3)));
        assert!(should_warm(origin, base + Duration::from_secs(4)));
    }

    #[test]
    fn prewarm_never_panics_without_a_runtime() {
        // Plain #[test] — no tokio runtime exists here. prewarm must degrade
        // to a silent no-op, not take the caller down.
        prewarm("https://no-runtime.test");
    }
}
