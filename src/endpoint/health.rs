//! Waiting for an endpoint to answer.

use crate::endpoint::openai::OpenAiClient;
use crate::endpoint::openai::ServedModel;
use anyhow::Result;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::watch;

/// Per-attempt HTTP timeout. Short, because a hung attempt should not eat the
/// whole budget — the retry loop will come back around.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often a supervised child is checked for death while a probe is in
/// flight.
const ABORT_POLL_INTERVAL: Duration = Duration::from_millis(200);

const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(3);

/// Single-shot reachability check.
pub async fn probe(base_url: &str, api_key: Option<&str>) -> Result<Vec<ServedModel>> {
    let client = OpenAiClient::new(base_url, ATTEMPT_TIMEOUT)?;
    client.list_models(api_key).await
}

/// Is the endpoint answering right now? Never returns an error; a failure to
/// build the client counts as "not reachable".
pub async fn is_reachable(base_url: &str, api_key: Option<&str>) -> bool {
    probe(base_url, api_key).await.is_ok()
}

/// Progress emitted while waiting, so callers can render something truthful
/// instead of a spinner with no information behind it.
#[derive(Debug, Clone, Default)]
pub struct WaitProgress {
    pub attempts: u32,
    pub elapsed: Duration,
    /// Error text from the most recent failed attempt.
    pub last_error: Option<String>,
}

/// Why a wait stopped without the endpoint becoming healthy.
#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    #[error("timed out after {elapsed:?} waiting for {url} ({attempts} attempts)\nlast error: {last_error}")]
    Timeout {
        url: String,
        elapsed: Duration,
        attempts: u32,
        last_error: String,
    },

    #[error("cancelled while waiting for {url}")]
    Cancelled { url: String },

    /// The process llmx started (model server or ssh) exited before the
    /// endpoint came up. Retrying is pointless; report what it said.
    #[error("{what} exited before the endpoint became ready: {detail}")]
    BackendExited { what: String, detail: String },
}

/// Checked between attempts. Returning `Some((what, detail))` abandons the
/// wait: something the caller supervises has died and no amount of retrying
/// will make the endpoint appear.
pub type AbortCheck<'a> = &'a mut (dyn FnMut() -> Option<(String, String)> + Send);

/// Poll `GET <base_url>/models` until it answers, with exponential backoff.
///
/// Returns as soon as the endpoint responds. `on_progress` is called after
/// every failed attempt; `abort` is consulted between attempts so a dead
/// child process short-circuits the wait instead of burning the full timeout.
pub async fn wait_until_ready(
    base_url: &str,
    api_key: Option<&str>,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
    mut abort: Option<AbortCheck<'_>>,
    on_progress: &mut (dyn FnMut(WaitProgress) + Send),
) -> Result<Vec<ServedModel>, WaitError> {
    let client = match OpenAiClient::new(base_url, ATTEMPT_TIMEOUT) {
        Ok(client) => client,
        Err(err) => {
            return Err(WaitError::Timeout {
                url: base_url.to_string(),
                elapsed: Duration::ZERO,
                attempts: 0,
                last_error: err.to_string(),
            });
        }
    };

    let started = Instant::now();
    let mut backoff = INITIAL_BACKOFF;
    let mut attempts = 0u32;
    // Replaced by the first failed attempt; only survives if the loop exits
    // before one completes, which the deadline check below cannot do.
    let mut last_error = String::from("no attempt completed");

    loop {
        if *cancel.borrow() {
            return Err(WaitError::Cancelled {
                url: base_url.to_string(),
            });
        }
        if let Some(check) = abort.as_deref_mut() {
            if let Some((what, detail)) = check() {
                return Err(WaitError::BackendExited { what, detail });
            }
        }

        attempts += 1;

        // Race the probe against the abort check. A probe against a port that
        // accepts but never answers blocks for the full attempt timeout, so
        // polling only between attempts would let a dead tunnel go unnoticed
        // for seconds.
        let attempt = client.list_models(api_key);
        tokio::pin!(attempt);
        let outcome = loop {
            tokio::select! {
                result = &mut attempt => break result,
                _ = tokio::time::sleep(ABORT_POLL_INTERVAL) => {
                    if let Some(check) = abort.as_deref_mut() {
                        if let Some((what, detail)) = check() {
                            return Err(WaitError::BackendExited { what, detail });
                        }
                    }
                }
            }
        };

        match outcome {
            Ok(models) => return Ok(models),
            Err(err) => {
                last_error.clear();
                last_error.push_str(&format_error_chain(&err));
                on_progress(WaitProgress {
                    attempts,
                    elapsed: started.elapsed(),
                    last_error: Some(last_error.clone()),
                });
            }
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Err(WaitError::Timeout {
                url: base_url.to_string(),
                elapsed,
                attempts,
                last_error,
            });
        }

        // Never sleep past the deadline; a 3s backoff should not turn a 180s
        // budget into 183s.
        let remaining = timeout.saturating_sub(elapsed);
        let sleep_for = backoff.min(remaining);

        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    return Err(WaitError::Cancelled { url: base_url.to_string() });
                }
            }
        }

        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Flatten an `anyhow` error chain into one line.
///
/// The useful detail is almost always in the innermost source (`Connection
/// refused`), which `Display` on the outer error hides.
fn format_error_chain(err: &anyhow::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !parts.iter().any(|p| p == &text) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(": ")
}
