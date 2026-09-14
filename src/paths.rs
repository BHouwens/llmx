//! Filesystem locations owned by llmx.
//!
//! llmx writes to exactly three places, all under the user's home directory,
//! and never to codex-local's own directory.

use anyhow::anyhow;
use anyhow::Result;
use std::path::PathBuf;

fn home() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))
}

/// `~/.local/state/llmx` — logs and other churn-heavy state.
pub fn state_dir() -> Result<PathBuf> {
    if let Some(dir) = env_dir("LLMX_STATE_DIR") {
        return Ok(dir);
    }
    Ok(home()?.join(".local").join("state").join("llmx"))
}

/// `~/.local/state/llmx/logs` — one file per launcher session.
pub fn log_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("logs"))
}

/// `~/.local/share/llmx` — generated artefacts worth keeping around.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = env_dir("LLMX_DATA_DIR") {
        return Ok(dir);
    }
    Ok(home()?.join(".local").join("share").join("llmx"))
}

/// `~/.local/share/llmx/generated` — codex-local configs llmx produces. These
/// are inspectable on purpose: when a launch misbehaves, this file is the
/// exact configuration that was handed to the agent.
pub fn generated_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("generated"))
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
