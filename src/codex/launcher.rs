//! Handing the terminal to codex-local.
//!
//! Unlike backend processes, codex-local is *not* put in its own process
//! group. It must stay in the launcher's foreground process group so it owns
//! the terminal: reading the keyboard from a background process group raises
//! SIGTTIN and the TUI would hang on its first keypress.

use crate::config::Isolation;
use crate::logging::SessionLog;
use anyhow::Context;
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use toml::value::Table;

/// How the generated configuration reaches codex-local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigDelivery {
    /// `-c key=value` for every generated key. The user's own config.toml
    /// still loads underneath, so MCP servers and history survive.
    Overrides(Vec<String>),
    /// `CODEX_HOME` pointed at a directory llmx owns. Fully isolated.
    CodexHome(PathBuf),
}

/// Everything needed to start the agent.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub program: String,
    pub cwd: PathBuf,
    pub delivery: ConfigDelivery,
    /// Resume an existing conversation; the inner `None` opens the session picker.
    pub resume: Option<Option<String>>,
}

impl LaunchPlan {
    /// The argument vector, excluding the program name.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec!["--cd".to_string(), self.cwd.display().to_string()];
        if let ConfigDelivery::Overrides(overrides) = &self.delivery {
            for item in overrides {
                args.push("-c".to_string());
                args.push(item.clone());
            }
        }
        if let Some(session_id) = &self.resume {
            args.push("resume".to_string());
            if let Some(session_id) = session_id {
                args.push(session_id.clone());
            }
        }
        args
    }

    /// The command line as a user could retype it, for logs and `--dry-run`.
    pub fn command_line(&self) -> String {
        let mut parts = Vec::new();
        if let ConfigDelivery::CodexHome(home) = &self.delivery {
            parts.push(format!(
                "CODEX_HOME={}",
                shell_quote(&home.display().to_string())
            ));
        }
        parts.push(self.program.clone());
        parts.extend(self.args().iter().map(|a| shell_quote(a)));
        parts.join(" ")
    }
}

/// Decide how to deliver the generated document and materialise it on disk.
///
/// Both modes write `<generated_dir>/<profile id>.toml`; the file is the
/// artefact to look at when a launch goes wrong. In `CodexHome` mode it is
/// additionally copied to `<generated_dir>/home-<profile id>/config.toml`,
/// because codex-local resolves `CODEX_HOME` with `canonicalize()` and so the
/// directory must already exist (`codex-rs/core/src/config.rs`,
/// `find_codex_home`).
pub fn prepare_delivery(
    isolation: Isolation,
    generated_dir: &Path,
    profile_id: &str,
    doc: &Table,
) -> Result<ConfigDelivery> {
    match isolation {
        Isolation::Overrides => Ok(ConfigDelivery::Overrides(
            crate::codex::config::to_cli_overrides(doc)?,
        )),
        Isolation::CodexHome => {
            let home = generated_dir.join(format!("home-{profile_id}"));
            std::fs::create_dir_all(&home).with_context(|| {
                format!("failed to create isolated CODEX_HOME at {}", home.display())
            })?;
            let config_path = home.join("config.toml");
            std::fs::write(&config_path, crate::codex::config::render(doc)?)
                .with_context(|| format!("failed to write {}", config_path.display()))?;
            Ok(ConfigDelivery::CodexHome(home))
        }
    }
}

/// Resolve the agent executable, failing with an actionable message.
pub fn resolve_program(program: &str) -> Result<()> {
    if which(program).is_some() {
        return Ok(());
    }
    anyhow::bail!(
        "`{program}` was not found on PATH.\n\
         Install codex-local, or set `settings.codex_bin` in your llmx config to its full path."
    )
}

/// Minimal `which`, to avoid a dependency for one lookup.
pub fn which(program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.is_absolute() || program.contains('/') {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let full = dir.join(program);
        is_executable(&full).then_some(full)
    })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Run codex-local in the foreground and wait for it to exit.
///
/// The caller must have restored the terminal (left the alternate screen,
/// disabled raw mode) before calling this.
pub async fn run(plan: &LaunchPlan, log: &SessionLog) -> Result<std::process::ExitStatus> {
    log.info(format!("launching: {}", plan.command_line()));

    let mut command = tokio::process::Command::new(&plan.program);
    command
        .args(plan.args())
        .current_dir(&plan.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let ConfigDelivery::CodexHome(home) = &plan.delivery {
        command.env("CODEX_HOME", home);
    }

    // Deliberately no `process_group`: see the module comment.
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start `{}`", plan.program))?;

    let status = child
        .wait()
        .await
        .with_context(|| format!("failed while waiting for `{}`", plan.program))?;

    log.info(format!(
        "{} exited with {}",
        plan.program,
        status
            .code()
            .map(|c| format!("code {c}"))
            .unwrap_or_else(|| "a signal".to_string())
    ));

    Ok(status)
}

/// Quote a shell argument only when it needs it, so logged command lines stay
/// readable and remain copy-pasteable.
fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+".contains(c));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}
