//! The one `reqwest::Client` every piece of LLM traffic goes through (§2, §6.4).
//!
//! Pre-warming works by opening a TLS connection early and leaving it idle in
//! the connection pool for the answer request to pick up. The pool lives
//! *inside* the `Client`, so the whole feature rests on a single invariant:
//! the warm GET and the answer POST it warms for must use the **same** client.
//! A per-request `Client::new()` gets a fresh, empty pool — the warmed
//! connection is stranded in a pool nobody will ever read, and pre-warm
//! silently does nothing while looking fully implemented. That failure mode is
//! invisible in tests and only shows up as ~300ms of extra stop-to-first-word
//! latency in production, which is why the singleton is enforced here rather
//! than left as a convention.

use std::sync::OnceLock;
use std::time::Duration;

/// How long an idle pooled connection survives. The warm happens when the user
/// presses Record; the answer request fires when they press Stop. That gap is
/// a human-length pause — a question being asked and thought about — so
/// anything short (reqwest's default is 90s, but some stacks default to 15–30s)
/// risks the warmed connection being reaped in exactly the window it exists
/// for. 120s covers a long question with headroom without holding sockets
/// open forever.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Per provider host the realistic load is one warmed connection plus one
/// in-flight answer. 4 leaves slack for a warm racing an answer without
/// letting stray sockets accumulate against api.anthropic.com / api.groq.com.
const POOL_MAX_IDLE_PER_HOST: usize = 4;

/// The process-wide client. Lazily built on first use so construction cost is
/// paid off the startup path, and `&'static` so no caller is ever tempted to
/// build their own (see the module comment for why that would quietly break
/// pre-warming).
pub fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // rustls rather than a system TLS stack: it ships in the binary,
            // so answers don't depend on the state of the user's Windows
            // schannel configuration.
            .use_rustls_tls()
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            // Deliberately NO `.timeout(..)`: a client-wide timeout is a
            // ceiling on the *whole* request, body included, and a long
            // streamed answer would be cut off mid-sentence by it. The
            // per-stage timeouts (first token, overall answer) live in the
            // session state machine where they can be tuned per stage.
            .build()
            // Only reachable if the TLS backend fails to initialise, which
            // with compiled-in rustls means the binary itself is broken.
            // Panicking once at first use beats every later request failing
            // with a confusing per-call error.
            .expect("failed to build shared HTTP client")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_client_is_the_same_instance_on_every_call() {
        // Pointer identity, not just equality: the pre-warm design depends on
        // the warm and the answer request sharing one connection pool, and the
        // pool is owned by the client instance.
        let a: *const reqwest::Client = shared_client();
        let b: *const reqwest::Client = shared_client();
        assert!(std::ptr::eq(a, b), "shared_client() must return one instance");
    }

    #[test]
    fn shared_client_is_the_same_instance_across_threads() {
        // The warm task and the answer request run on different tokio workers;
        // the singleton must hold across threads, not just across calls.
        let main_ptr = shared_client() as *const reqwest::Client as usize;
        let handles: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| shared_client() as *const reqwest::Client as usize)
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), main_ptr);
        }
    }
}
