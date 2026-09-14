//! A model server reachable directly over the network.
//!
//! A LAN box, a VPN host, a tunnel someone else manages. llmx starts nothing
//! and stops nothing; it only checks that the endpoint answers. This is what
//! makes SSH just one transport rather than a special case.

use crate::backend::api_key_for;
use crate::backend::wait_with_supervision;
use crate::backend::Backend;
use crate::backend::BackendStatus;
use crate::backend::Ownership;
use crate::config::ModelProfile;
use crate::endpoint::health;
use crate::endpoint::ServedModel;
use crate::endpoint::WaitError;
use crate::endpoint::WaitProgress;
use crate::process::ProcessManager;
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::watch;

pub struct RemoteBackend {
    base_url: String,
    api_key: Option<String>,
    timeout: Duration,
    /// Always empty. Kept so the shared wait loop has the same shape here as
    /// in the backends that do own processes.
    processes: ProcessManager,
}

impl RemoteBackend {
    pub fn new(profile: &ModelProfile, settings_timeout: Duration) -> Self {
        Self {
            base_url: profile.endpoint.url.clone(),
            api_key: api_key_for(profile),
            timeout: settings_timeout,
            processes: ProcessManager::new(),
        }
    }
}

#[async_trait]
impl Backend for RemoteBackend {
    fn describe(&self) -> String {
        format!("direct network endpoint: {}", self.base_url)
    }

    async fn status(&self) -> BackendStatus {
        if health::is_reachable(&self.base_url, self.api_key.as_deref()).await {
            BackendStatus::Ready
        } else {
            BackendStatus::Stopped
        }
    }

    async fn start(&mut self) -> Result<()> {
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
        Ok(())
    }

    fn owns_process(&self) -> bool {
        false
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn ownership(&self) -> Ownership {
        Ownership::External
    }
}
