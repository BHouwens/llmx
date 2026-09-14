//! Tracks every process llmx owns for the duration of a run.
//!
//! Ownership is the whole point. A backend either started a process (llmx
//! owns it, and must stop it) or found one already listening (someone else
//! owns it, and llmx must leave it completely alone). The manager only ever
//! holds the first kind.

use crate::logging::SessionLog;
use crate::process::child::ExitState;
use crate::process::child::OwnedChild;

/// A process that died on its own, noticed by [`ProcessManager::check_liveness`].
#[derive(Debug, Clone)]
pub struct DeadChild {
    pub label: String,
    pub code: Option<i32>,
    pub output: String,
}

#[derive(Default)]
pub struct ProcessManager {
    children: Vec<OwnedChild>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn adopt(&mut self, child: OwnedChild) {
        self.children.push(child);
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// One line per owned process, naming it and its pid.
    pub fn owned_summary(&self) -> Vec<String> {
        self.children
            .iter()
            .map(|child| format!("{} (pid {})", child.label(), child.pid()))
            .collect()
    }

    /// Report any owned process that has exited since the last check.
    pub fn check_liveness(&mut self) -> Vec<DeadChild> {
        let mut dead = Vec::new();
        self.children.retain_mut(|child| match child.poll() {
            Ok(ExitState::Running) => true,
            Ok(ExitState::Exited { code }) => {
                dead.push(DeadChild {
                    label: child.label().to_string(),
                    code,
                    output: child.output().as_detail(),
                });
                false
            }
            Err(_) => {
                // Status could not be read; assume gone rather than keep
                // signalling an unknown pid.
                false
            }
        });
        dead
    }

    /// Stop every owned process, newest first.
    ///
    /// Reverse order matters: a tunnel started after a server should come down
    /// before it, so the server never sees a half-open forward.
    pub async fn shutdown_all(&mut self, log: &SessionLog) {
        if !self.children.is_empty() {
            log.info(format!(
                "cleaning up {} process(es) llmx started: {}",
                self.children.len(),
                self.owned_summary().join(", ")
            ));
        }
        while let Some(mut child) = self.children.pop() {
            if let Err(err) = child.shutdown().await {
                log.error(format!("failed to stop {}: {err:#}", child.label()));
            }
        }
    }
}
