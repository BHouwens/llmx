//! A child process llmx started and is therefore responsible for.
//!
//! The central invariant of this module: an `OwnedChild` only ever exists for
//! a process llmx spawned itself. Anything already running when llmx started
//! is never represented here, and so can never be signalled.

use crate::logging::SessionLog;
use anyhow::Context;
use anyhow::Result;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;

/// How long a process gets to exit after SIGTERM before it is SIGKILLed.
const GRACEFUL_SHUTDOWN: Duration = Duration::from_secs(5);

/// Lines of child output retained for error reporting.
const OUTPUT_TAIL: usize = 40;

/// How a child process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitState {
    Running,
    Exited { code: Option<i32> },
}

/// Shared tail of a child's stdout/stderr, so an error message can quote what
/// the process actually said rather than "it failed".
#[derive(Clone, Default)]
pub struct OutputTail {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl OutputTail {
    fn push(&self, line: String) {
        let mut lines = match self.lines.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if lines.len() == OUTPUT_TAIL {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        let lines = match self.lines.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        lines.iter().cloned().collect()
    }

    /// The tail rendered for an error message, or a placeholder when the
    /// process said nothing at all.
    pub fn as_detail(&self) -> String {
        let lines = self.snapshot();
        if lines.is_empty() {
            "(no output)".to_string()
        } else {
            lines.join("\n")
        }
    }
}

/// A process llmx spawned, its process group, and its captured output.
pub struct OwnedChild {
    /// Human name used in log lines and errors, e.g. `ssh tunnel (spark)`.
    label: String,
    child: Child,
    pid: u32,
    output: OutputTail,
    log: SessionLog,
}

impl OwnedChild {
    /// Spawn `command` in its own process group with output captured.
    ///
    /// The new process group matters for cleanup: model server launchers are
    /// often shell wrappers that fork the real server, and killing only the
    /// wrapper leaves the server holding the port. Signalling the group gets
    /// the descendants too.
    ///
    /// It also means the child does *not* receive the terminal's Ctrl-C.
    /// That is intentional here — backend processes are managed by llmx, not
    /// by the user's keyboard. codex-local itself is launched differently,
    /// see [`crate::codex::launcher`].
    pub fn spawn(label: impl Into<String>, mut command: Command, log: &SessionLog) -> Result<Self> {
        let label = label.into();

        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);

        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn {label}"))?;

        let pid = child.id().context("spawned child reported no pid")?;

        let output = OutputTail::default();

        if let Some(stdout) = child.stdout.take() {
            spawn_reader(stdout, label.clone(), "stdout", output.clone(), log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(stderr, label.clone(), "stderr", output.clone(), log.clone());
        }

        log.info(format!("started {label} (pid {pid})"));

        Ok(Self {
            label,
            child,
            pid,
            output,
            log: log.clone(),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn output(&self) -> &OutputTail {
        &self.output
    }

    /// Non-blocking check of whether the process is still alive.
    pub fn poll(&mut self) -> Result<ExitState> {
        match self.child.try_wait() {
            Ok(None) => Ok(ExitState::Running),
            Ok(Some(status)) => Ok(ExitState::Exited {
                code: status.code(),
            }),
            Err(err) => Err(anyhow::Error::new(err)
                .context(format!("failed to check status of {}", self.label))),
        }
    }

    /// Terminate the process group: SIGTERM, then SIGKILL if it has not gone
    /// within [`GRACEFUL_SHUTDOWN`].
    pub async fn shutdown(&mut self) -> Result<()> {
        if matches!(self.poll()?, ExitState::Exited { .. }) {
            return Ok(());
        }

        self.log
            .info(format!("stopping {} (pid {})", self.label, self.pid));

        #[cfg(unix)]
        signal_group(self.pid, libc::SIGTERM);

        let deadline = tokio::time::Instant::now() + GRACEFUL_SHUTDOWN;
        loop {
            if matches!(self.poll()?, ExitState::Exited { .. }) {
                self.log.info(format!("{} stopped", self.label));
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        self.log.warn(format!(
            "{} did not exit within {}s of SIGTERM; sending SIGKILL",
            self.label,
            GRACEFUL_SHUTDOWN.as_secs()
        ));

        #[cfg(unix)]
        signal_group(self.pid, libc::SIGKILL);

        // Reap so the process does not linger as a zombie.
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        Ok(())
    }
}

/// Send `signal` to the process group led by `pid`.
///
/// `kill(-pid, sig)` addresses the group. The child was spawned with
/// `process_group(0)`, which makes its pid the group id, so this reaches the
/// child and everything it forked — and nothing else.
#[cfg(unix)]
fn signal_group(pid: u32, signal: i32) {
    // SAFETY: `kill` with a negative pid targets a process group. The pid is
    // one llmx spawned and has not reaped, so it is either alive or a zombie;
    // in both cases the call is defined and cannot reach an unrelated process.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

fn spawn_reader<R>(
    reader: R,
    label: String,
    stream: &'static str,
    tail: OutputTail,
    log: SessionLog,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log.debug(format!("[{label}/{stream}] {line}"));
            tail.push(line);
        }
    });
}
