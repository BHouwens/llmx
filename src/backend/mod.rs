//! Turning a profile into a healthy endpoint.
//!
//! Every backend answers the same question — "give me a base URL that
//! responds" — and differs only in what it must do first: start a server,
//! open a tunnel, or nothing at all. codex-local never learns which.

pub mod local;
pub mod remote;
pub mod ssh;

#[cfg(test)]
mod tests;

use crate::config::ModelProfile;
use crate::endpoint::health;
use crate::endpoint::WaitError;
use crate::endpoint::WaitProgress;
use crate::logging::SessionLog;
use crate::process::ProcessManager;
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::watch;

/// The resolved thing codex-local is pointed at. Backend-agnostic by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEndpoint {
    /// OpenAI-compatible base URL, e.g. `http://127.0.0.1:8100/v1`.
    pub base_url: String,
    /// Model id the server accepts, after discovery/validation.
    pub model_id: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

/// Lifecycle state of a backend, as shown in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendStatus {
    /// Nothing is listening and llmx has not tried to change that.
    Stopped,
    /// llmx has started a process that is not serving yet.
    Starting,
    /// A transport (SSH forward) is up; the endpoint has not answered yet.
    Connecting,
    /// The endpoint answers `GET /models`.
    Ready,
    /// Something went wrong; the string is safe to show the user.
    Error(String),
}

impl BackendStatus {
    pub fn label(&self) -> &str {
        match self {
            BackendStatus::Stopped => "stopped",
            BackendStatus::Starting => "starting",
            BackendStatus::Connecting => "connecting",
            BackendStatus::Ready => "ready",
            BackendStatus::Error(_) => "error",
        }
    }
}

/// Who started the process behind the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// llmx spawned it and must stop it on exit.
    Owned,
    /// It was already listening. llmx must not touch it.
    External,
    /// Nothing has been resolved yet.
    Unknown,
}

#[async_trait]
pub trait Backend: Send {
    /// One-line description of the transport, for the detail pane.
    fn describe(&self) -> String;

    /// Current state, determined without changing anything.
    async fn status(&self) -> BackendStatus;

    /// Make the endpoint exist. A no-op when something is already listening.
    async fn start(&mut self) -> Result<()>;

    /// Block until the endpoint answers, then report the discovered models.
    async fn wait_ready(
        &mut self,
        cancel: &mut watch::Receiver<bool>,
        on_progress: &mut (dyn FnMut(WaitProgress) + Send),
    ) -> Result<Vec<crate::endpoint::ServedModel>, WaitError>;

    /// Stop anything llmx started. Must be safe to call more than once, and
    /// must never signal a process llmx did not spawn.
    async fn shutdown(&mut self) -> Result<()>;

    /// Whether llmx has a process to clean up.
    fn owns_process(&self) -> bool;

    /// Base URL the agent should be pointed at.
    fn base_url(&self) -> &str;

    fn ownership(&self) -> Ownership;
}

/// Build the backend a profile calls for.
pub fn for_profile(
    profile: &ModelProfile,
    settings_timeout: Duration,
    log: SessionLog,
) -> Box<dyn Backend> {
    match profile.backend {
        crate::config::BackendKind::Local => {
            Box::new(local::LocalBackend::new(profile, settings_timeout, log))
        }
        crate::config::BackendKind::Ssh => {
            Box::new(ssh::SshBackend::new(profile, settings_timeout, log))
        }
        crate::config::BackendKind::Remote => {
            Box::new(remote::RemoteBackend::new(profile, settings_timeout))
        }
    }
}

/// Read the API key named by a profile, if any.
///
/// Only the variable *name* lives in config; the value is read here and never
/// written to a log or a generated file.
pub fn api_key_for(profile: &ModelProfile) -> Option<String> {
    profile
        .endpoint
        .env_key
        .as_ref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

/// Poll the endpoint, giving up early if a process llmx started has died.
///
/// The retry loop itself lives in [`crate::endpoint::health`]; this adds the
/// one thing that module deliberately knows nothing about — supervision of
/// child processes. A dead child means retrying is pointless, so the wait
/// ends immediately with whatever the process printed before it exited,
/// rather than burning the full timeout on a connection that cannot succeed.
pub(crate) async fn wait_with_supervision(
    base_url: &str,
    api_key: Option<&str>,
    timeout: Duration,
    processes: &mut ProcessManager,
    cancel: &mut watch::Receiver<bool>,
    on_progress: &mut (dyn FnMut(WaitProgress) + Send),
) -> Result<Vec<crate::endpoint::ServedModel>, WaitError> {
    let mut abort = || {
        processes.check_liveness().into_iter().next().map(|dead| {
            let code = dead
                .code
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "terminated by signal".to_string());
            (dead.label, format!("{code}\n{}", dead.output))
        })
    };

    health::wait_until_ready(
        base_url,
        api_key,
        timeout,
        cancel,
        Some(&mut abort),
        on_progress,
    )
    .await
}
