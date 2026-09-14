//! Orchestration: the sequence that turns a chosen profile into a running
//! agent, and the non-interactive commands around it.

use crate::backend::Backend;
use crate::backend::ModelEndpoint;
use crate::codex;
use crate::config::BackendKind;
use crate::config::ConfigFile;
use crate::config::ModelProfile;
use crate::endpoint;
use crate::logging::SessionLog;
use crate::paths;
use crate::system;
use crate::tui;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;

/// Exit code convention: 130 is the shell's "terminated by SIGINT".
pub const EXIT_INTERRUPTED: i32 = 130;

pub struct App {
    pub config_path: PathBuf,
    pub config: ConfigFile,
    pub cwd: PathBuf,
}

impl App {
    pub fn load() -> Result<Self> {
        let (config_path, config) = crate::config::load()?;
        let cwd = std::env::current_dir().context("failed to determine current directory")?;
        Ok(Self {
            config_path,
            config,
            cwd,
        })
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.settings.health_timeout_secs)
    }

    fn find(&self, id: &str) -> Result<&ModelProfile> {
        self.config
            .models
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| {
                let available: Vec<&str> =
                    self.config.models.iter().map(|p| p.id.as_str()).collect();
                anyhow!(
                    "no model profile with id `{id}`.\nConfigured ids: {}",
                    if available.is_empty() {
                        "(none)".to_string()
                    } else {
                        available.join(", ")
                    }
                )
            })
    }

    // ---------------------------------------------------------------- TUI --

    /// The default path: pick a model, bring it up, hand over the terminal.
    pub async fn run_tui(&self) -> Result<i32> {
        if self.config.models.is_empty() {
            anyhow::bail!(
                "no models configured in {}.\nAdd a `[[models]]` entry; see `llmx doctor`.",
                self.config_path.display()
            );
        }

        let log = self.new_log("session");
        let mut picker = tui::state::PickerState::new(
            self.config.models.clone(),
            self.cwd.clone(),
            self.config_path.clone(),
            system::total_ram_gb(),
        );

        let (mut input, input_handle) = tui::events::spawn_reader();
        let mut guard = tui::TerminalGuard::enter()?;

        let selected = match tui::run_picker(
            &mut guard,
            &mut picker,
            &mut input,
            &log,
            self.timeout(),
        )
        .await
        {
            Ok(tui::PickerOutcome::Launch(index)) => index,
            Ok(tui::PickerOutcome::Quit) => {
                guard.leave()?;
                drop(input_handle);
                return Ok(0);
            }
            Err(err) => {
                guard.leave()?;
                drop(input_handle);
                return Err(err);
            }
        };

        let profile = self.config.models[selected].clone();
        log.info(format!("selected profile `{}`", profile.id));

        let backend = crate::backend::for_profile(&profile, self.timeout(), log.clone());
        let outcome = tui::run_startup(&mut guard, &profile, backend, &mut input, &log).await;

        let (mut backend, model_id) = match outcome {
            Ok(tui::StartupOutcome::Ready { backend, model_id }) => (backend, model_id),
            Ok(other) => {
                let cancelled = matches!(other, tui::StartupOutcome::Cancelled { .. });
                let error = match &other {
                    tui::StartupOutcome::Failed { error, .. } => Some(format!("{error:#}")),
                    _ => None,
                };
                let mut backend = other.into_backend();
                // Give the screen a moment to be read before tearing down.
                tokio::time::sleep(Duration::from_millis(400)).await;
                guard.leave()?;
                drop(input_handle);
                self.cleanup(&mut backend, &log).await;
                if let Some(error) = error {
                    eprintln!("llmx: {error}");
                    self.print_log_hint(&log);
                    return Ok(1);
                }
                return Ok(if cancelled { EXIT_INTERRUPTED } else { 0 });
            }
            Err(err) => {
                guard.leave()?;
                drop(input_handle);
                return Err(err);
            }
        };

        // Terminal goes back to the shell's control before the agent starts.
        guard.leave()?;
        drop(input_handle);

        let status = self
            .launch_agent(&profile, backend.as_ref(), &model_id, &log)
            .await;
        self.cleanup(&mut backend, &log).await;

        match status {
            Ok(code) => Ok(code),
            Err(err) => {
                eprintln!("llmx: {err:#}");
                self.print_log_hint(&log);
                Ok(1)
            }
        }
    }

    // ---------------------------------------------------- llmx run <id> --

    /// Non-interactive launch of a named profile.
    pub async fn run_profile(&self, id: &str) -> Result<i32> {
        let profile = self.find(id)?.clone();
        let log = self.new_log(&profile.id);

        let mut backend = crate::backend::for_profile(&profile, self.timeout(), log.clone());

        eprintln!("llmx: {} — {}", profile.name, backend.describe());

        // Ctrl-C only races the startup phase. Once the agent has the
        // terminal, SIGINT belongs to it.
        let startup = tokio::select! {
            result = self.bring_up(&profile, backend.as_mut(), &log) => result,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("llmx: interrupted");
                self.cleanup(&mut backend, &log).await;
                return Ok(EXIT_INTERRUPTED);
            }
        };

        let model_id = match startup {
            Ok(model_id) => model_id,
            Err(err) => {
                self.cleanup(&mut backend, &log).await;
                eprintln!("llmx: {err:#}");
                self.print_log_hint(&log);
                return Ok(1);
            }
        };

        let status = self
            .launch_agent(&profile, backend.as_ref(), &model_id, &log)
            .await;
        self.cleanup(&mut backend, &log).await;

        match status {
            Ok(code) => Ok(code),
            Err(err) => {
                eprintln!("llmx: {err:#}");
                self.print_log_hint(&log);
                Ok(1)
            }
        }
    }

    /// Start the backend and wait for a usable model id, printing progress to
    /// stderr rather than to a TUI.
    async fn bring_up(
        &self,
        profile: &ModelProfile,
        backend: &mut dyn Backend,
        log: &SessionLog,
    ) -> Result<String> {
        backend.start().await?;
        if backend.owns_process() {
            eprintln!("llmx: backend started by llmx");
        } else {
            eprintln!("llmx: endpoint already running; leaving it untouched");
        }

        let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let mut last_reported = 0u64;
        let mut on_progress = move |progress: endpoint::WaitProgress| {
            let secs = progress.elapsed.as_secs();
            // One line every 5s, so a long model load does not scroll the
            // terminal away.
            if secs >= last_reported + 5 {
                last_reported = secs;
                eprintln!(
                    "llmx: waiting for endpoint… {secs}s, attempt {}",
                    progress.attempts
                );
            }
        };

        let models = backend
            .wait_ready(&mut cancel_rx, &mut on_progress)
            .await
            .map_err(anyhow::Error::new)?;

        match endpoint::resolve_model(profile.model_id.as_deref(), &models) {
            endpoint::ModelResolution::Resolved(id) => {
                log.info(format!("resolved model id `{id}`"));
                eprintln!("llmx: model `{id}`");
                Ok(id)
            }
            endpoint::ModelResolution::Ambiguous { available } => Err(anyhow!(
                "{} serves several models and the profile sets no `model_id`: {}.\n\
                 Add `model_id = \"...\"` to the `{}` profile, or run `llmx` and choose \
                 interactively.",
                backend.base_url(),
                available.join(", "),
                profile.id
            )),
            endpoint::ModelResolution::ConfiguredMissing {
                configured,
                available,
            } => Err(anyhow!(
                "the `{}` profile asks for model `{configured}`, but {} serves: {}.",
                profile.id,
                backend.base_url(),
                available.join(", ")
            )),
            endpoint::ModelResolution::NoneServed => Err(anyhow!(
                "{} serves no models and the `{}` profile sets no `model_id`.",
                backend.base_url(),
                profile.id
            )),
        }
    }

    // ------------------------------------------------------------ launch --

    /// Generate the agent configuration and run codex-local in this directory.
    async fn launch_agent(
        &self,
        profile: &ModelProfile,
        backend: &dyn Backend,
        model_id: &str,
        log: &SessionLog,
    ) -> Result<i32> {
        let endpoint = ModelEndpoint {
            base_url: backend.base_url().to_string(),
            model_id: model_id.to_string(),
            context_window: profile.context_window,
            max_output_tokens: profile.max_output_tokens,
        };

        let document = codex::build_document(profile, &endpoint);
        let generated_dir = paths::generated_dir()?;
        let generated_path = codex::write_generated(&generated_dir, &profile.id, &document)
            .with_context(|| {
                format!(
                    "failed to write the generated agent config under {}",
                    generated_dir.display()
                )
            })?;
        log.info(format!("generated config at {}", generated_path.display()));

        let delivery = codex::prepare_delivery(
            self.config.settings.isolation,
            &generated_dir,
            &profile.id,
            &document,
        )?;

        let program = self.config.settings.codex_bin.clone();
        codex::resolve_program(&program)?;

        let plan = codex::LaunchPlan {
            program,
            cwd: self.cwd.clone(),
            delivery,
        };

        let status = codex::launcher::run(&plan, log).await?;
        Ok(status.code().unwrap_or(EXIT_INTERRUPTED))
    }

    /// Stop anything llmx started. Never touches processes it found running.
    async fn cleanup(&self, backend: &mut Box<dyn Backend>, log: &SessionLog) {
        if !backend.owns_process() {
            let reason = match backend.ownership() {
                crate::backend::Ownership::External => {
                    "the endpoint was already running and is left as it was"
                }
                _ => "llmx started no processes",
            };
            log.info(format!("nothing to clean up: {reason}"));
            return;
        }
        if let Err(err) = backend.shutdown().await {
            log.error(format!("cleanup failed: {err:#}"));
            eprintln!("llmx: cleanup failed: {err:#}");
        }
    }

    fn new_log(&self, tag: &str) -> SessionLog {
        match paths::log_dir() {
            Ok(dir) => SessionLog::create(&dir, tag),
            Err(_) => SessionLog::in_memory(),
        }
    }

    fn print_log_hint(&self, log: &SessionLog) {
        if let Some(path) = log.path() {
            eprintln!("llmx: full log at {}", path.display());
        }
    }

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
