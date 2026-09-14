//! Session logging.
//!
//! One log file per llmx run, plus an in-memory ring buffer the TUI tails.
//! Deliberately not built on `tracing`: the launcher has a handful of event
//! sites and needs the events in two places (file and screen), which is less
//! code to do directly than to wire a subscriber for.

use anyhow::Context;
use anyhow::Result;
use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Number of recent lines retained for the TUI log pane.
const RING_CAPACITY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    pub text: String,
    /// Wall-clock seconds since the Unix epoch, for the file prefix.
    pub at: u64,
}

struct Inner {
    file: Option<File>,
    ring: VecDeque<LogLine>,
}

/// Handle to one llmx session's log. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct SessionLog {
    inner: Arc<Mutex<Inner>>,
    path: Option<PathBuf>,
}

impl SessionLog {
    /// Create a log file named after the session start time and profile id.
    ///
    /// Failing to open the file is not fatal: the launcher still works, it
    /// just keeps the ring buffer only. Losing logs must never stop a launch.
    pub fn create(dir: &Path, profile_id: &str) -> Self {
        let stamp = now_secs();
        let file_name = format!("{stamp}-{profile_id}.log");
        let path = dir.join(file_name);
        let file = std::fs::create_dir_all(dir)
            .and_then(|()| OpenOptions::new().create(true).append(true).open(&path))
            .ok();
        let resolved_path = file.as_ref().map(|_| path);

        Self {
            inner: Arc::new(Mutex::new(Inner {
                file,
                ring: VecDeque::with_capacity(RING_CAPACITY),
            })),
            path: resolved_path,
        }
    }

    /// An in-memory-only log, for tests and for commands that should not
    /// leave files behind.
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                file: None,
                ring: VecDeque::with_capacity(RING_CAPACITY),
            })),
            path: None,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn log(&self, level: Level, text: impl Into<String>) {
        let line = LogLine {
            level,
            text: text.into(),
            at: now_secs(),
        };

        // A poisoned lock means another thread panicked mid-write. Recover the
        // guard rather than propagating: logging must not take down a launch.
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if let Some(file) = inner.file.as_mut() {
            let _ = writeln!(file, "{} {:<5} {}", line.at, line.level, line.text);
            let _ = file.flush();
        }

        if inner.ring.len() == RING_CAPACITY {
            inner.ring.pop_front();
        }
        inner.ring.push_back(line);
    }

    pub fn info(&self, text: impl Into<String>) {
        self.log(Level::Info, text);
    }

    pub fn warn(&self, text: impl Into<String>) {
        self.log(Level::Warn, text);
    }

    pub fn error(&self, text: impl Into<String>) {
        self.log(Level::Error, text);
    }

    pub fn debug(&self, text: impl Into<String>) {
        self.log(Level::Debug, text);
    }

    /// Most recent `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<LogLine> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let skip = inner.ring.len().saturating_sub(n);
        inner.ring.iter().skip(skip).cloned().collect()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Log files in `dir`, newest first.
pub fn list_logs(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read log directory {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .map(|entry| {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            (modified, entry.path())
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, path)| path).collect())
}
