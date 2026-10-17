//! Implementations of the noninteractive CLI commands.
//!
//! These commands inspect profiles or the machine; launch lifecycle handling
//! remains in the parent `app` module.

use super::App;
use crate::backend::ModelEndpoint;
use crate::codex;
use crate::config::BackendKind;
use crate::logging::SessionLog;
use crate::{paths, system};
use anyhow::{Context, Result};

impl App {
    // ---------------------------------------------------------- commands --

    pub fn list(&self) {
        println!("config: {}", self.config_path.display());
        println!();
        let id_width = self
            .config
            .models
            .iter()
            .map(|p| p.id.len())
            .max()
            .unwrap_or(2)
            .max(2);
        let name_width = self
            .config
            .models
            .iter()
            .map(|p| p.name.len())
            .max()
            .unwrap_or(4)
            .max(4);

        println!(
            "{:<id_width$}  {:<name_width$}  {:<8}  ENDPOINT",
            "ID", "NAME", "BACKEND"
        );
        for profile in &self.config.models {
            println!(
                "{:<id_width$}  {:<name_width$}  {:<8}  {}",
                profile.id,
                profile.name,
                profile.backend.as_str(),
                profile.endpoint.url
            );
        }
    }

    /// Probe every endpoint without starting anything.
    pub async fn status(&self) -> Result<()> {
        let log = SessionLog::in_memory();
        let mut handles = Vec::new();
        for profile in &self.config.models {
            let profile = profile.clone();
            let timeout = self.timeout();
            let log = log.clone();
            handles.push(tokio::spawn(async move {
                let backend = crate::backend::for_profile(&profile, timeout, log);
                let status = backend.status().await;
                (profile, status)
            }));
        }

        for handle in handles {
            let (profile, status) = handle.await?;
            println!(
                "{:<24}  {:<10}  {}",
                profile.id,
                status.label(),
                profile.endpoint.url
            );
        }
        Ok(())
    }

    /// Offline checks: everything that can be verified without a network call.
    pub async fn doctor(&self) -> Result<i32> {
        let mut problems = 0usize;
        let mut note = |ok: bool, message: String| {
            if !ok {
                problems += 1;
            }
            println!("{} {message}", if ok { "ok  " } else { "FAIL" });
        };

        note(
            true,
            format!("config parses: {}", self.config_path.display()),
        );
        note(
            true,
            format!("{} model profile(s) defined", self.config.models.len()),
        );

        let program = &self.config.settings.codex_bin;
        match codex::which(program) {
            Some(path) => note(
                true,
                format!("agent `{program}` found at {}", path.display()),
            ),
            None => note(
                false,
                format!("agent `{program}` not found on PATH (set settings.codex_bin)"),
            ),
        }
        println!(
            "     note: llmx targets codex-local (~/.codex-local); it never reads or writes \
             ~/.codex"
        );

        let needs_ssh = self
            .config
            .models
            .iter()
            .any(|p| p.backend == BackendKind::Ssh);
        if needs_ssh {
            match codex::which("ssh") {
                Some(path) => note(true, format!("ssh found at {}", path.display())),
                None => note(false, "ssh not found on PATH".to_string()),
            }
        }

        for profile in &self.config.models {
            if let Some(runtime) = &profile.runtime {
                match codex::which(&runtime.command) {
                    Some(path) => note(
                        true,
                        format!(
                            "`{}` runtime `{}` → {}",
                            profile.id,
                            runtime.command,
                            path.display()
                        ),
                    ),
                    None => note(
                        false,
                        format!(
                            "`{}` runtime command `{}` not found on PATH",
                            profile.id, runtime.command
                        ),
                    ),
                }
            }

            if let Some(ssh) = &profile.ssh {
                // `ssh -G` resolves the effective config for a host without
                // connecting, so this check stays offline.
                let resolved = tokio::process::Command::new("ssh")
                    .arg("-G")
                    .arg(&ssh.host)
                    .output()
                    .await;
                match resolved {
                    Ok(output) if output.status.success() => {
                        let hostname = String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .find_map(|line| line.strip_prefix("hostname ").map(str::to_string))
                            .unwrap_or_else(|| ssh.host.clone());

                        // `ssh -G` answers for any name, echoing the input
                        // back when no Host block matched. An unchanged
                        // hostname therefore means "not an alias" — fine if
                        // it is a real hostname, worth flagging if it was
                        // meant to be one.
                        let bare = ssh.host.rsplit('@').next().unwrap_or(&ssh.host);
                        if hostname == bare && !bare.contains('.') {
                            note(
                                true,
                                format!(
                                    "`{}` ssh host `{}` is not a Host alias in ~/.ssh/config; it \
                                     will be used as a literal hostname",
                                    profile.id, ssh.host
                                ),
                            );
                        } else {
                            note(
                                true,
                                format!("`{}` ssh alias `{}` → {hostname}", profile.id, ssh.host),
                            );
                        }
                    }
                    _ => note(
                        false,
                        format!(
                            "`{}` ssh host `{}` could not be resolved by ssh; check ~/.ssh/config",
                            profile.id, ssh.host
                        ),
                    ),
                }
            }
        }

        match paths::generated_dir().and_then(|dir| {
            std::fs::create_dir_all(&dir)?;
            Ok(dir)
        }) {
            Ok(dir) => note(
                true,
                format!("generated config dir writable: {}", dir.display()),
            ),
            Err(err) => note(false, format!("generated config dir unusable: {err:#}")),
        }

        match paths::log_dir().and_then(|dir| {
            std::fs::create_dir_all(&dir)?;
            Ok(dir)
        }) {
            Ok(dir) => note(true, format!("log dir writable: {}", dir.display())),
            Err(err) => note(false, format!("log dir unusable: {err:#}")),
        }

        if let Some(total) = system::total_ram_gb() {
            note(true, format!("system RAM: {total:.0} GB"));
            for profile in &self.config.models {
                match system::assess(profile.resources.as_ref(), Some(total)) {
                    system::MemoryVerdict::Insufficient(detail) => {
                        println!("warn `{}`: {detail}", profile.id);
                    }
                    system::MemoryVerdict::Tight(detail) => {
                        println!("warn `{}`: {detail}", profile.id);
                    }
                    _ => {}
                }
            }
        }

        println!();
        if problems == 0 {
            println!("no problems found");
            Ok(0)
        } else {
            println!("{problems} problem(s) found");
            Ok(1)
        }
    }

    /// Print the most recent session log, or list them.
    pub fn logs(&self, list_only: bool, lines: usize) -> Result<()> {
        let dir = paths::log_dir()?;
        let files = crate::logging::list_logs(&dir)?;

        if files.is_empty() {
            println!("no logs in {}", dir.display());
            return Ok(());
        }

        if list_only {
            for file in files {
                println!("{}", file.display());
            }
            return Ok(());
        }

        let newest = &files[0];
        println!("== {} ==", newest.display());
        let contents = std::fs::read_to_string(newest)
            .with_context(|| format!("failed to read {}", newest.display()))?;
        let all: Vec<&str> = contents.lines().collect();
        let start = all.len().saturating_sub(lines);
        for line in &all[start..] {
            println!("{line}");
        }
        Ok(())
    }

    /// Print what would be run, without starting anything.
    pub fn explain(&self, id: &str) -> Result<()> {
        let profile = self.find(id)?;
        let endpoint = ModelEndpoint {
            base_url: profile.endpoint.url.clone(),
            model_id: profile
                .model_id
                .clone()
                .unwrap_or_else(|| "<discovered at launch>".to_string()),
            context_window: profile.context_window,
            max_output_tokens: profile.max_output_tokens,
        };

        if let Some(ssh) = &profile.ssh {
            println!("tunnel:");
            println!(
                "  {}",
                crate::backend::ssh::SshBackend::command_line(ssh).join(" ")
            );
            println!();
        }
        if let Some(runtime) = &profile.runtime {
            println!("runtime:");
            println!("  {} {}", runtime.command, runtime.args.join(" "));
            println!();
        }

        let document = codex::build_document(profile, &endpoint);
        println!("generated codex-local config:");
        for line in codex::render(&document)?.lines() {
            println!("  {line}");
        }
        println!();

        let delivery = match self.config.settings.isolation {
            crate::config::Isolation::Overrides => {
                codex::ConfigDelivery::Overrides(codex::to_cli_overrides(&document)?)
            }
            crate::config::Isolation::CodexHome => {
                codex::ConfigDelivery::CodexHome(paths::generated_dir()?.join(format!("home-{id}")))
            }
        };
        let plan = codex::LaunchPlan {
            program: self.config.settings.codex_bin.clone(),
            cwd: self.cwd.clone(),
            delivery,
        };
        println!("launch:");
        println!("  {}", plan.command_line());
        Ok(())
    }
}
