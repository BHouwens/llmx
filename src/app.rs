//! Orchestration: the sequence that turns a chosen profile into a running
//! agent. Noninteractive commands live in [`commands`].

mod commands;

use crate::backend::Backend;
use crate::backend::ModelEndpoint;
use crate::codex;
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
}
