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
//!
//! The client also owns the two transport-level guards that keep a warmed
//! connection worth having: TCP keepalive, so a NAT or VPN gateway does not
//! silently forget the idle socket during a long recording, and a connect
//! timeout, so a black-holed handshake fails fast enough for the retry
//! policy (§6.4) to matter. Neither bounds a whole request — the body streams.

use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;

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

/// Bounds ONLY the TCP + TLS connect phase. Against an origin that is
/// black-holing SYNs (captive portal, a VPN mid-flap, a firewall that drops
/// instead of rejecting) an unbounded connect sits in the OS timeout — about
/// 21 s on Windows — before the retry policy (§6.4) even gets a look, which
/// is twice the entire first-token budget (§3). 3 s is generous for a
/// handshake to a CDN-fronted API and leaves the retry room to succeed
/// inside the 10 s cap. Once connected the body may stream for as long as
/// the session machine allows.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Idle time before the first TCP keepalive probe on every pooled socket.
/// The warmed connection sits idle for the whole recording — a human-length
/// pause — and consumer NAT and VPN gateways drop idle mappings on timers
/// that can be shorter than that. reqwest then holds a socket the far end has
/// forgotten, the answer request dies on its first write, and the one retry
/// is spent on a cold handshake. Probing at 20 s keeps the mapping alive: the
/// warm buys the ~300 ms; this keeps it bought.
const TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(20);

/// Gap between keepalive probes once the idle threshold has passed. Set
/// explicitly rather than left to the OS: socket2 hands an unset interval to
/// `SIO_KEEPALIVE_VALS` as 0 on Windows, which is not "use the default".
const TCP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);

/// Every setting the shared client is built from, kept apart from
/// `shared_client()` so a test can read the configuration back:
/// `reqwest::Client` hides its config once built, `ClientBuilder`'s Debug
/// output does not.
fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        // rustls rather than a system TLS stack: it ships in the binary,
        // so answers don't depend on the state of the user's Windows
        // schannel configuration.
        .use_rustls_tls()
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_keepalive(Some(TCP_KEEPALIVE_IDLE))
        .tcp_keepalive_interval(Some(TCP_KEEPALIVE_INTERVAL))
    // Deliberately NO `.timeout(..)`: a client-wide timeout is a
    // ceiling on the *whole* request, body included, and a long
    // streamed answer would be cut off mid-sentence by it. The
    // per-stage timeouts (first token, overall answer) live in the
    // session state machine where they can be tuned per stage.
}

/// The process-wide client. Lazily built on first use so construction cost is
/// paid off the startup path, and `&'static` so no caller is ever tempted to
/// build their own (see the module comment for why that would quietly break
/// pre-warming).
pub fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        builder()
            .build()
            // Only reachable if the TLS backend fails to initialise, which
            // with compiled-in rustls means the binary itself is broken.
            // Panicking once at first use beats every later request failing
            // with a confusing per-call error.
            .expect("failed to build shared HTTP client")
    })
}

/// Most of an error body a provider may make us hold (R2). The UI shows a
/// 300-character snippet; 16 KiB is ample to find it in while bounding a
/// whole HTML error page, or a server that streams garbage forever.
pub const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;

/// Read a non-2xx response body, stopping at `MAX_ERROR_BODY_BYTES`.
///
/// Enforced while reading: `response.text()` would allocate the whole body
/// first and truncate after, which is exactly the unbounded read the cap
/// exists to prevent — and against a server that never ends the body it
/// would never return at all. A transport error mid-body keeps what arrived:
/// a partial snippet still names the failure better than nothing.
pub async fn read_error_body(response: reqwest::Response) -> String {
    let mut stream = Box::pin(response.bytes_stream());
    let mut body: Vec<u8> = Vec::new();
    while let Some(Ok(chunk)) = stream.next().await {
        let room = MAX_ERROR_BODY_BYTES - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= MAX_ERROR_BODY_BYTES {
            break;
        }
    }
    // A multi-byte character cut at the cap decodes as one replacement
    // character, which the caller's snippet then trims away anyway.
    String::from_utf8_lossy(&body).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn error_bodies_are_read_only_up_to_the_cap_even_if_the_server_never_ends() {
        // The server promises 10 MB, sends 64 KiB, then holds the socket open
        // forever. Reading to the end (`response.text()`) would hang here and
        // would allocate whatever arrived; the capped reader must return with
        // at most MAX_ERROR_BODY_BYTES.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let head = "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 10000000\r\n\r\n";
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&vec![b'e'; 64 * 1024]).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(600)).await;
        });
        let response = shared_client()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap();
        let body = tokio::time::timeout(Duration::from_secs(20), read_error_body(response))
            .await
            .expect("the capped read must not wait for the rest of the body");
        assert_eq!(body.len(), MAX_ERROR_BODY_BYTES);
        assert!(body.bytes().all(|b| b == b'e'));
    }

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

    #[test]
    fn client_bounds_the_connect_but_never_the_whole_request() {
        // The builder's Debug output is the only window into the config once
        // a Client exists. `connect_timeout` must be there (a black-holed SYN
        // otherwise waits on the OS, ~21 s on Windows, past the first-token
        // cap) and a whole-request `timeout` must NOT be: it would cut a long
        // streamed answer off mid-sentence.
        let debug = format!("{:?}", builder());
        assert!(debug.contains("connect_timeout: 3s"), "got {debug}");
        assert!(!debug.contains(" timeout: "), "a whole-request timeout crept in: {debug}");
    }

    #[test]
    fn keepalive_probes_before_a_gateway_can_forget_the_warm_socket() {
        // Keepalive is not observable through the built client, so the values
        // are pinned here so a change is deliberate: the first probe must
        // land well inside the pool idle window (or the pool reaps the socket
        // before keepalive ever mattered), and the interval must be explicit
        // and shorter than the idle threshold — an unset one reaches Windows
        // as 0.
        assert_eq!(TCP_KEEPALIVE_IDLE, Duration::from_secs(20));
        assert_eq!(TCP_KEEPALIVE_INTERVAL, Duration::from_secs(1));
        assert!(TCP_KEEPALIVE_INTERVAL < TCP_KEEPALIVE_IDLE);
        assert!(TCP_KEEPALIVE_IDLE < POOL_IDLE_TIMEOUT);
        assert_eq!(POOL_IDLE_TIMEOUT, Duration::from_secs(120), "unchanged on purpose (§6.4)");
    }
}
