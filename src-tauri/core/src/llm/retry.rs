//! The shared retry policy: exactly one retry, and only when it is provably
//! safe (§6.4). Both providers route through `with_retry_once` with their own
//! "was this a connection-level failure?" predicate.
//!
//! The rules, each of which corresponds to a way a naive retry makes things
//! worse instead of better:
//!
//! * **Never retry an HTTP error status.** The server heard us and said no —
//!   a 401 will be a 401 again, a 429 asked us to back off, and an instant
//!   repeat just burns the first-token budget on a request we already know
//!   fails. Only a failure *before* the server answered (connect refused, TLS
//!   reset, connection dropped pre-response) is worth one more try, because
//!   that is exactly the case a stale pooled connection produces.
//! * **Never retry after a delta has reached the UI.** The UI *appends*
//!   deltas. A second attempt would stream a second full answer onto the end
//!   of the first's partial text, and the user reads one garbled reply that
//!   restarts mid-thought. A truncated answer is recoverable by the user; a
//!   concatenated one is gibberish.
//! * **Never retry after an abort.** The user superseded this work; a retry
//!   would resurrect an answer nobody wants and race the one they do.
//! * **The retried request must be byte-identical.** This helper only hands
//!   the closure an attempt index — callers build the body ONCE, outside the
//!   closure, and reuse it. A rebuilt body can differ (timestamps, map
//!   ordering) and on Anthropic a differing prefix silently misses the prompt
//!   cache, turning the retry into the slowest request of the day.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio_util::sync::CancellationToken;

use crate::error::{AppError, AppResult};

/// Shared record of whether the current call has pushed any delta to the UI.
///
/// The streaming code flips it on the first delta (typically from a sink
/// wrapper, on whatever task the response body is polled on); the retry
/// decision reads it after the attempt's future has completed. It is the one
/// piece of state that distinguishes "the connection died before anything
/// happened" (retryable) from "the connection died mid-answer" (not — see the
/// module comment on concatenation).
#[derive(Debug, Default)]
pub struct Attempt {
    delta_emitted: AtomicBool,
}

impl Attempt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call on the first delta (calling on every delta is harmless).
    pub fn mark_delta_emitted(&self) {
        self.delta_emitted.store(true, Ordering::SeqCst);
    }

    pub fn delta_emitted(&self) -> bool {
        self.delta_emitted.load(Ordering::SeqCst)
    }
}

/// Run `run(0)`; on failure, run `run(1)` — at most once, and only when every
/// safety rule in the module comment allows it.
///
/// * `cancel` is checked before any attempt and after any failure, and wins
///   over every other outcome as `AppError::aborted()` — never as an
///   HTTP-flavoured error the UI would show for work the user cancelled.
/// * `attempt` must be the same instance the caller's sink marks on first
///   delta, or the concatenation guard is blind.
/// * `is_retryable` is the provider's judgment of "connection-level failure";
///   this helper never second-guesses it in the retryable direction, only
///   vetoes (abort, delta already emitted).
/// * `run` receives only the attempt index. Anything that must be identical
///   across attempts — above all the request body — must be built before
///   calling this and captured by reference.
pub async fn with_retry_once<T, F, Fut>(
    cancel: &CancellationToken,
    attempt: &Attempt,
    is_retryable: impl Fn(&AppError) -> bool,
    run: F,
) -> AppResult<T>
where
    F: Fn(u32) -> Fut,
    Fut: Future<Output = AppResult<T>>,
{
    // Cancellation first, before any network work: a Stop pressed between
    // scheduling and running this call must not cost a request.
    if cancel.is_cancelled() {
        return Err(AppError::aborted());
    }

    let first_err = match run(0).await {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };

    // Cancellation may be *why* the attempt failed (the provider aborts its
    // request on cancel). Either signal — the token or an aborted error —
    // means the user moved on, and no error may outrank that.
    if cancel.is_cancelled() {
        return Err(AppError::aborted());
    }
    if first_err.is_aborted() {
        return Err(first_err);
    }

    // The concatenation guard: once any delta reached the appending UI, a
    // second answer glued onto the first is worse than the error.
    if attempt.delta_emitted() {
        return Err(first_err);
    }

    if !is_retryable(&first_err) {
        return Err(first_err);
    }

    match run(1).await {
        Ok(v) => Ok(v),
        Err(e) => {
            if cancel.is_cancelled() {
                return Err(AppError::aborted());
            }
            // The second error describes current reality; the first described
            // a connection we already gave up on.
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::sync::{Arc, Mutex};

    /// The shape of error a provider maps a pre-response connection failure
    /// to. The *code* is irrelevant to the helper — retryability is entirely
    /// the predicate's call — so tests key the predicate off the message.
    fn conn_err(msg: &str) -> AppError {
        AppError::new(ErrorCode::LlmHttp, msg)
    }

    fn retry_on_conn(e: &AppError) -> bool {
        e.message.contains("conn")
    }

    /// Records every attempt index the run closure sees.
    fn call_log() -> Arc<Mutex<Vec<u32>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[tokio::test]
    async fn connection_failure_before_any_delta_is_retried_exactly_once() {
        let cancel = CancellationToken::new();
        let attempt = Attempt::new();
        let calls = call_log();
        let calls2 = calls.clone();

        let result = with_retry_once(&cancel, &attempt, retry_on_conn, move |i| {
            let calls = calls2.clone();
            async move {
                calls.lock().unwrap().push(i);
                if i == 0 {
                    Err(conn_err("conn reset before response"))
                } else {
                    Ok("answer".to_string())
                }
            }
        })
        .await;

        // The second attempt's success is what the caller gets, and the
        // closure saw exactly the indices 0 then 1 — no third attempt exists.
        assert_eq!(result.unwrap(), "answer");
        assert_eq!(*calls.lock().unwrap(), vec![0, 1]);
    }

    #[tokio::test]
    async fn http_status_error_is_not_retried() {
        let cancel = CancellationToken::new();
        let attempt = Attempt::new();
        let calls = call_log();
        let calls2 = calls.clone();

        let result: AppResult<String> =
            with_retry_once(&cancel, &attempt, retry_on_conn, move |i| {
                let calls = calls2.clone();
                async move {
                    calls.lock().unwrap().push(i);
                    // The server answered; repeating the question changes
                    // nothing and burns first-token budget.
                    Err(AppError::new(ErrorCode::LlmHttp, "HTTP 500"))
                }
            })
            .await;

        assert_eq!(result.unwrap_err().message, "HTTP 500");
        assert_eq!(*calls.lock().unwrap(), vec![0]);
    }

    #[tokio::test]
    async fn connection_failure_after_a_delta_is_not_retried() {
        // THE rule that prevents two answers being concatenated: the failure
        // is connection-level and the predicate would allow a retry, but a
        // delta already reached the appending UI, so the retry is vetoed.
        let cancel = CancellationToken::new();
        let attempt = Arc::new(Attempt::new());
        let calls = call_log();
        let calls2 = calls.clone();
        let attempt2 = attempt.clone();

        let result: AppResult<String> =
            with_retry_once(&cancel, &attempt, retry_on_conn, move |i| {
                let calls = calls2.clone();
                let attempt = attempt2.clone();
                async move {
                    calls.lock().unwrap().push(i);
                    // Simulate: some answer text streamed out, then the
                    // connection dropped.
                    attempt.mark_delta_emitted();
                    Err(conn_err("conn dropped mid-stream"))
                }
            })
            .await;

        assert_eq!(result.unwrap_err().message, "conn dropped mid-stream");
        assert_eq!(*calls.lock().unwrap(), vec![0], "a retry here would concatenate answers");
    }

    #[tokio::test]
    async fn aborted_error_is_not_retried() {
        let cancel = CancellationToken::new();
        let attempt = Attempt::new();
        let calls = call_log();
        let calls2 = calls.clone();

        // Predicate says everything is retryable; abort must still veto.
        let result: AppResult<String> =
            with_retry_once(&cancel, &attempt, |_| true, move |i| {
                let calls = calls2.clone();
                async move {
                    calls.lock().unwrap().push(i);
                    Err(AppError::aborted())
                }
            })
            .await;

        assert!(result.unwrap_err().is_aborted());
        assert_eq!(*calls.lock().unwrap(), vec![0]);
    }

    #[tokio::test]
    async fn cancelled_token_short_circuits_before_any_attempt() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let attempt = Attempt::new();
        let calls = call_log();
        let calls2 = calls.clone();

        let result: AppResult<String> =
            with_retry_once(&cancel, &attempt, |_| true, move |i| {
                let calls = calls2.clone();
                async move {
                    calls.lock().unwrap().push(i);
                    Ok("never".to_string())
                }
            })
            .await;

        // No request may be spent on work the user already cancelled.
        assert!(result.unwrap_err().is_aborted());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancellation_during_the_first_attempt_yields_aborted_not_the_http_error() {
        // The token is cancelled while attempt 0 is in flight. Whatever
        // transport error the abort provoked, the caller must see aborted() —
        // the UI never renders that — rather than an HTTP-flavoured error for
        // work the user stopped caring about.
        let cancel = CancellationToken::new();
        let attempt = Attempt::new();
        let calls = call_log();
        let calls2 = calls.clone();
        let cancel2 = cancel.clone();

        let result: AppResult<String> =
            with_retry_once(&cancel, &attempt, retry_on_conn, move |i| {
                let calls = calls2.clone();
                let cancel = cancel2.clone();
                async move {
                    calls.lock().unwrap().push(i);
                    cancel.cancel();
                    Err(conn_err("conn torn down by abort"))
                }
            })
            .await;

        assert!(result.unwrap_err().is_aborted());
        assert_eq!(*calls.lock().unwrap(), vec![0]);
    }

    #[tokio::test]
    async fn failure_on_the_second_attempt_propagates_the_second_error() {
        let cancel = CancellationToken::new();
        let attempt = Attempt::new();
        let calls = call_log();
        let calls2 = calls.clone();

        let result: AppResult<String> =
            with_retry_once(&cancel, &attempt, retry_on_conn, move |i| {
                let calls = calls2.clone();
                async move {
                    calls.lock().unwrap().push(i);
                    if i == 0 {
                        Err(conn_err("conn error: first"))
                    } else {
                        Err(conn_err("conn error: second"))
                    }
                }
            })
            .await;

        // The second error describes current reality; surfacing the first
        // would send the user debugging a connection we already replaced.
        assert_eq!(result.unwrap_err().message, "conn error: second");
        assert_eq!(*calls.lock().unwrap(), vec![0, 1]);
    }

    #[tokio::test]
    async fn both_attempts_receive_the_identical_body() {
        // The byte-identical rule in practice: the body is built once, the
        // closure only borrows it, and both attempts observe the same
        // allocation with the same bytes. (Anthropic's prompt cache keys on
        // the exact prefix, so even a semantically-equal rebuild can regress.)
        let cancel = CancellationToken::new();
        let attempt = Attempt::new();
        let body: Arc<String> = Arc::new(r#"{"model":"m","messages":[]}"#.to_string());
        let seen: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let body2 = body.clone();
        let seen2 = seen.clone();

        let result = with_retry_once(&cancel, &attempt, retry_on_conn, move |i| {
            let body = body2.clone();
            let seen = seen2.clone();
            async move {
                seen.lock().unwrap().push((Arc::as_ptr(&body) as usize, (*body).clone()));
                if i == 0 {
                    Err(conn_err("conn refused"))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        // Same allocation and same bytes — nothing was rebuilt between tries.
        assert_eq!(seen[0].0, seen[1].0);
        assert_eq!(seen[0].1, seen[1].1);
        assert_eq!(seen[0].1, *body.as_ref());
    }
}
