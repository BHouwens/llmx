//! A model server on another machine, reached through an SSH local forward.
//!
//! The repository and the agent stay here. Only inference requests cross the
//! tunnel, and the remote API port is never exposed beyond the remote host's
//! own loopback interface.

use crate::backend::api_key_for;
use crate::backend::wait_with_supervision;
use crate::backend::Backend;
use crate::backend::BackendStatus;
use crate::backend::Ownership;
use crate::config::ModelProfile;
use crate::config::SshConfig;
use crate::endpoint::health;
use crate::endpoint::ServedModel;
use crate::endpoint::WaitError;
use crate::endpoint::WaitProgress;
use crate::logging::SessionLog;
use crate::process::OwnedChild;
use crate::process::ProcessManager;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::watch;

pub struct SshBackend {
    profile_name: String,
    base_url: String,
    api_key: Option<String>,
    ssh: SshConfig,
    timeout: Duration,
    processes: ProcessManager,
    ownership: Ownership,
    log: SessionLog,
}

impl SshBackend {
    /// Panics only if called for a profile that failed validation; validation
    /// guarantees `[models.ssh]` is present for `backend = "ssh"`.
    pub fn new(profile: &ModelProfile, settings_timeout: Duration, log: SessionLog) -> Self {
        let ssh = profile
            .ssh
            .clone()
            .expect("validated ssh profile always has [models.ssh]");
        let timeout = ssh
            .ready_timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(settings_timeout);

        Self {
            profile_name: profile.name.clone(),
            base_url: profile.endpoint.url.clone(),
            api_key: api_key_for(profile),
            ssh,
            timeout,
            processes: ProcessManager::new(),
            ownership: Ownership::Unknown,
            log,
        }
    }

    /// Full command line, for logs and for `llmx doctor`.
    pub fn command_line(ssh: &SshConfig) -> Vec<String> {
        let mut parts = vec!["ssh".to_string()];
        parts.extend(ssh.args());
        parts
    }
}

/// Explain an ssh failure in terms of what to do about it.
///
/// llmx runs ssh with stdin closed so it cannot corrupt the terminal, which
/// also means ssh can never prompt for a passphrase or a host-key
/// confirmation. Those two cases look like bare refusals unless said plainly.
pub(crate) fn ssh_hint(detail: &str) -> Option<&'static str> {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("permission denied") || lower.contains("publickey") {
        return Some(
            "llmx runs ssh with no terminal, so it cannot ask for a key passphrase. \
             Run `ssh-add <your key>` first, or use a key without a passphrase.",
        );
    }
    if lower.contains("host key verification failed") || lower.contains("authenticity of host") {
        return Some(
            "the host is not in ~/.ssh/known_hosts and llmx cannot answer the prompt. \
             Connect once by hand (`ssh <host>`) to accept the key, then retry.",
        );
    }
    if lower.contains("address already in use") || lower.contains("cannot listen to port") {
        return Some(
            "another process holds the local port. Change ssh.local_port (and the port in \
             endpoint.url to match), or stop whatever is using it.",
        );
    }
    if lower.contains("could not resolve hostname") {
        return Some(
            "the host is neither a Host alias in ~/.ssh/config nor a resolvable hostname. \
             Check it with `llmx doctor`.",
        );
    }
    None
}

#[async_trait]
impl Backend for SshBackend {
    fn describe(&self) -> String {
        format!(
            "ssh -L {}:{}:{} {}",
            self.ssh.local_port, self.ssh.remote_host, self.ssh.remote_port, self.ssh.host
        )
    }

    async fn status(&self) -> BackendStatus {
        if health::is_reachable(&self.base_url, self.api_key.as_deref()).await {
            BackendStatus::Ready
        } else if self.owns_process() {
            BackendStatus::Connecting
        } else {
            BackendStatus::Stopped
        }
    }

    async fn start(&mut self) -> Result<()> {
        // Someone may already have this forward open — another llmx session,
        // a manual `ssh -L`, a tmux pane. Reuse it and leave it running.
        if health::is_reachable(&self.base_url, self.api_key.as_deref()).await {
            self.ownership = Ownership::External;
            self.log.info(format!(
                "{} is already reachable at {}; reusing the existing tunnel and leaving it running",
                self.profile_name, self.base_url
            ));
            return Ok(());
        }

        let args = self.ssh.args();
        self.log
            .info(format!("opening tunnel: ssh {}", args.join(" ")));

        let mut command = Command::new("ssh");
        command.args(&args);

        let label = format!("ssh tunnel ({})", self.ssh.host);
        let child = OwnedChild::spawn(label, command, &self.log).with_context(|| {
            format!(
                "failed to run `ssh`. Check that OpenSSH is installed and that `{}` resolves in \
                 ~/.ssh/config.",
                self.ssh.host
            )
        })?;
        self.processes.adopt(child);
        self.ownership = Ownership::Owned;
        Ok(())
    }

    async fn wait_ready(
        &mut self,
        cancel: &mut watch::Receiver<bool>,
        on_progress: &mut (dyn FnMut(WaitProgress) + Send),
    ) -> Result<Vec<ServedModel>, WaitError> {
        let result = wait_with_supervision(
            &self.base_url,
            self.api_key.as_deref(),
            self.timeout,
            &mut self.processes,
            cancel,
            on_progress,
        )
        .await;

        // ssh's own messages are accurate but assume you know why it could
        // not ask you anything. Name the cause it cannot name itself.
        match result {
            Err(WaitError::BackendExited { what, detail }) => Err(WaitError::BackendExited {
                what,
                detail: match ssh_hint(&detail) {
                    Some(hint) => format!("{detail}\n\nhint: {hint}"),
                    None => detail,
                },
            }),
            other => other,
        }
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.processes.shutdown_all(&self.log).await;
        Ok(())
    }

    fn owns_process(&self) -> bool {
        !self.processes.is_empty()
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn ownership(&self) -> Ownership {
        self.ownership
    }
}
