//! A model server on this machine.
//!
//! llmx starts the server only when nothing is already listening on the
//! configured endpoint. If something is, that process belongs to whoever
//! started it and llmx will not stop it on exit.

use crate::backend::api_key_for;
use crate::backend::wait_with_supervision;
use crate::backend::Backend;
use crate::backend::BackendStatus;
use crate::backend::Ownership;
use crate::config::ModelProfile;
use crate::config::RuntimeConfig;
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

pub struct LocalBackend {
    profile_name: String,
    base_url: String,
    api_key: Option<String>,
    runtime: Option<RuntimeConfig>,
    timeout: Duration,
    processes: ProcessManager,
    ownership: Ownership,
    log: SessionLog,
}

impl LocalBackend {
    pub fn new(profile: &ModelProfile, settings_timeout: Duration, log: SessionLog) -> Self {
        let timeout = profile
            .runtime
            .as_ref()
            .and_then(|r| r.ready_timeout_secs)
            .map(Duration::from_secs)
            .unwrap_or(settings_timeout);

        Self {
            profile_name: profile.name.clone(),
            base_url: profile.endpoint.url.clone(),
            api_key: api_key_for(profile),
            runtime: profile.runtime.clone(),
            timeout,
            processes: ProcessManager::new(),
            ownership: Ownership::Unknown,
            log,
        }
    }

    fn build_command(runtime: &RuntimeConfig) -> Command {
        let mut command = Command::new(&runtime.command);
        command.args(&runtime.args);
        for (key, value) in &runtime.env {
            command.env(key, value);
        }
        if let Some(cwd) = &runtime.cwd {
            command.current_dir(cwd);
        }
        command
    }
}

#[async_trait]
impl Backend for LocalBackend {
    fn describe(&self) -> String {
        match &self.runtime {
            Some(runtime) => {
                let args = runtime.args.join(" ");
                if args.is_empty() {
                    format!("local process: {}", runtime.command)
                } else {
                    format!("local process: {} {args}", runtime.command)
                }
            }
            None => "local endpoint (started outside llmx)".to_string(),
        }
    }

    async fn status(&self) -> BackendStatus {
        if health::is_reachable(&self.base_url, self.api_key.as_deref()).await {
            BackendStatus::Ready
        } else if self.owns_process() {
            BackendStatus::Starting
        } else if self.runtime.is_none() {
            // Down, and llmx has no command to bring it up. Saying "stopped"
            // would imply Enter will start it.
            BackendStatus::Error("no runtime command; start the server yourself".to_string())
        } else {
            BackendStatus::Stopped
        }
    }

    async fn start(&mut self) -> Result<()> {
        if health::is_reachable(&self.base_url, self.api_key.as_deref()).await {
            self.ownership = Ownership::External;
            self.log.info(format!(
                "{} is already serving at {}; llmx will not start or stop it",
                self.profile_name, self.base_url
            ));
            return Ok(());
        }

        let runtime = self.runtime.clone().with_context(|| {
            format!(
                "nothing is listening at {} and `{}` has no `[models.runtime]` block, so llmx has \
                 no command to start.\n\
                 Either start the server yourself, or add a runtime command to the profile.",
                self.base_url, self.profile_name
            )
        })?;

        let label = format!("model server ({})", runtime.command);
        let child = OwnedChild::spawn(label, Self::build_command(&runtime), &self.log)
            .with_context(|| {
                format!(
                    "failed to start `{}`. Check that it is installed and on PATH.",
                    runtime.command
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
        wait_with_supervision(
            &self.base_url,
            self.api_key.as_deref(),
            self.timeout,
            &mut self.processes,
            cancel,
            on_progress,
        )
        .await
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
