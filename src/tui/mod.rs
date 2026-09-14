//! Terminal lifecycle and the two interactive screens.

pub mod events;
pub mod state;
pub mod view;

#[cfg(test)]
mod tests;

use crate::backend::Backend;
use crate::backend::BackendStatus;
use crate::config::ModelProfile;
use crate::endpoint::ServedModel;
use crate::endpoint::WaitProgress;
use crate::logging::SessionLog;
use crate::tui::events::Action;
use crate::tui::state::PickerState;
use crate::tui::state::StartupState;
use anyhow::Context;
use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::io::Stdout;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::watch;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Owns raw mode and the alternate screen, restoring both on drop.
///
/// Restoration on drop matters more than usual here: llmx hands the terminal
/// to another full-screen program, and a panic that skipped teardown would
/// leave the user with an unusable shell.
pub struct TerminalGuard {
    terminal: Option<Term>,
}

impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen).context("failed to enter alternate screen")?;
        let terminal =
            Terminal::new(CrosstermBackend::new(out)).context("failed to create terminal")?;
        Ok(Self {
            terminal: Some(terminal),
        })
    }

    pub fn terminal(&mut self) -> &mut Term {
        self.terminal
            .as_mut()
            .expect("terminal is present until leave() or drop")
    }

    /// Restore the terminal explicitly, before handing it to another program.
    pub fn leave(&mut self) -> Result<()> {
        if let Some(mut terminal) = self.terminal.take() {
            disable_raw_mode().ok();
            execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
            terminal.show_cursor().ok();
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

/// How the picker screen ended.
pub enum PickerOutcome {
    Launch(usize),
    Quit,
}

/// Run the model picker until the user launches or quits.
pub async fn run_picker(
    guard: &mut TerminalGuard,
    state: &mut PickerState,
    input: &mut mpsc::Receiver<Action>,
    log: &SessionLog,
    settings_timeout: Duration,
) -> Result<PickerOutcome> {
    let (status_tx, mut status_rx) = mpsc::channel::<(usize, BackendStatus)>(32);
    log.info(format!("config: {}", state.config_path.display()));
    spawn_status_probes(state, status_tx.clone(), settings_timeout, log);

    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    let mut pending = state.rows.len();

    loop {
        let tail = log.tail(64);
        guard
            .terminal()
            .draw(|frame| view::draw_picker(frame, state, &tail))?;

        tokio::select! {
            Some(action) = input.recv() => match action {
                Action::Up => state.move_up(),
                Action::Down => state.move_down(),
                Action::Select => {
                    if !state.rows.is_empty() {
                        return Ok(PickerOutcome::Launch(state.selected));
                    }
                }
                Action::Refresh => {
                    state.refreshing = true;
                    pending = state.rows.len();
                    spawn_status_probes(state, status_tx.clone(), settings_timeout, log);
                }
                Action::ToggleLogs => state.show_logs = !state.show_logs,
                Action::Quit | Action::Interrupt => return Ok(PickerOutcome::Quit),
                _ => {}
            },
            Some((index, status)) = status_rx.recv() => {
                if let Some(row) = state.rows.get_mut(index) {
                    row.status = status;
                }
                pending = pending.saturating_sub(1);
                if pending == 0 {
                    state.refreshing = false;
                }
            }
            _ = ticker.tick() => {}
        }
    }
}

/// Probe every profile's endpoint concurrently.
///
/// These are read-only checks: they never start a server or open a tunnel, so
/// opening the picker has no side effects on the user's machine.
fn spawn_status_probes(
    state: &PickerState,
    tx: mpsc::Sender<(usize, BackendStatus)>,
    settings_timeout: Duration,
    log: &SessionLog,
) {
    for (index, row) in state.rows.iter().enumerate() {
        let profile = row.profile.clone();
        let tx = tx.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let backend = crate::backend::for_profile(&profile, settings_timeout, log);
            let status = backend.status().await;
            let _ = tx.send((index, status)).await;
        });
    }
}

/// What the startup screen produced.
pub enum StartupOutcome {
    Ready {
        backend: Box<dyn Backend>,
        model_id: String,
    },
    Cancelled {
        backend: Box<dyn Backend>,
    },
    Failed {
        backend: Box<dyn Backend>,
        error: anyhow::Error,
    },
}

impl StartupOutcome {
    /// The backend, whatever happened, so the caller can always clean up.
    pub fn into_backend(self) -> Box<dyn Backend> {
        match self {
            StartupOutcome::Ready { backend, .. }
            | StartupOutcome::Cancelled { backend }
            | StartupOutcome::Failed { backend, .. } => backend,
        }
    }
}

/// The backend task's result: the backend itself (always returned, so the
/// caller can clean up) plus what the health wait produced.
type StartupResult = (Box<dyn Backend>, Result<Vec<ServedModel>>);

enum TaskMessage {
    Step(usize, String),
    StepDone(usize, String),
    Progress(WaitProgress),
}

/// Start the backend, wait for health, and settle on a model id.
///
/// The backend runs in a task so the screen keeps redrawing — elapsed time,
/// attempt count and the most recent connection error stay live instead of
/// freezing behind a blocking call.
pub async fn run_startup(
    guard: &mut TerminalGuard,
    profile: &ModelProfile,
    mut backend: Box<dyn Backend>,
    input: &mut mpsc::Receiver<Action>,
    log: &SessionLog,
) -> Result<StartupOutcome> {
    const STEP_START: usize = 0;
    const STEP_HEALTH: usize = 1;
    const STEP_MODEL: usize = 2;

    let mut screen = StartupState::new(
        profile.name.clone(),
        backend.describe(),
        &["Start backend", "Wait for endpoint", "Resolve model"],
    );

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (msg_tx, mut msg_rx) = mpsc::channel::<TaskMessage>(64);
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();

    let task_log = log.clone();
    let mut cancel_for_task = cancel_rx.clone();
    tokio::spawn(async move {
        let _ = msg_tx
            .send(TaskMessage::Step(STEP_START, "…".to_string()))
            .await;

        if let Err(err) = backend.start().await {
            let _ = done_tx.send((backend, Err(err)));
            return;
        }

        let started_detail = if backend.owns_process() {
            "started by llmx".to_string()
        } else {
            "already running (left untouched)".to_string()
        };
        let _ = msg_tx
            .send(TaskMessage::StepDone(STEP_START, started_detail))
            .await;
        let _ = msg_tx
            .send(TaskMessage::Step(STEP_HEALTH, "connecting…".to_string()))
            .await;

        let progress_tx = msg_tx.clone();
        let mut on_progress = move |progress: WaitProgress| {
            let _ = progress_tx.try_send(TaskMessage::Progress(progress));
        };

        let result = backend
            .wait_ready(&mut cancel_for_task, &mut on_progress)
            .await;

        match result {
            Ok(models) => {
                task_log.info(format!(
                    "endpoint healthy; server reports {} model(s)",
                    models.len()
                ));
                let _ = msg_tx
                    .send(TaskMessage::StepDone(STEP_HEALTH, "healthy".to_string()))
                    .await;
                let _ = done_tx.send((backend, Ok(models)));
            }
            Err(err) => {
                let _ = done_tx.send((backend, Err(anyhow::Error::new(err))));
            }
        }
    });

    let started_at = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(150));
    let mut finished: Option<StartupResult> = None;
    let mut cancelled = false;

    // Phase A: drive the backend task while rendering.
    while finished.is_none() {
        screen.elapsed_secs = started_at.elapsed().as_secs();
        guard.terminal().draw(|f| view::draw_startup(f, &screen))?;

        tokio::select! {
            Some(action) = input.recv() => {
                if matches!(action, Action::Quit | Action::Interrupt) {
                    cancelled = true;
                    let _ = cancel_tx.send(true);
                }
            }
            Some(message) = msg_rx.recv() => match message {
                TaskMessage::Step(index, detail) => screen.start(index, detail),
                TaskMessage::StepDone(index, detail) => screen.finish(index, detail),
                TaskMessage::Progress(progress) => {
                    let detail = match &progress.last_error {
                        Some(error) => format!(
                            "attempt {} · {}s · {}",
                            progress.attempts,
                            progress.elapsed.as_secs(),
                            first_line(error)
                        ),
                        None => format!("attempt {}", progress.attempts),
                    };
                    screen.update(STEP_HEALTH, detail);
                }
            },
            result = &mut done_rx => {
                finished = result.ok();
            }
            _ = ticker.tick() => {}
        }
    }

    let (backend, health) = match finished {
        Some(value) => value,
        None => unreachable!("loop exits only once `finished` is set"),
    };

    let models = match health {
        Ok(models) => models,
        Err(error) => {
            screen.fail(STEP_HEALTH, first_line(&format!("{error:#}")));
            guard.terminal().draw(|f| view::draw_startup(f, &screen))?;
            log.error(format!("startup failed: {error:#}"));
            return Ok(if cancelled {
                StartupOutcome::Cancelled { backend }
            } else {
                StartupOutcome::Failed { backend, error }
            });
        }
    };

    // Phase B: settle on a model id, asking only when there is a real choice.
    screen.start(STEP_MODEL, "…".to_string());
    let resolution = crate::endpoint::resolve_model(profile.model_id.as_deref(), &models);

    let model_id = match resolution {
        crate::endpoint::ModelResolution::Resolved(id) => {
            screen.finish(STEP_MODEL, id.clone());
            id
        }
        crate::endpoint::ModelResolution::NoneServed => {
            let error = anyhow::anyhow!(
                "{} serves no models and the profile sets no `model_id`.\n\
                 Add `model_id = \"...\"` to the profile, or check what the server loaded.",
                backend.base_url()
            );
            screen.fail(STEP_MODEL, "no models served");
            guard.terminal().draw(|f| view::draw_startup(f, &screen))?;
            return Ok(StartupOutcome::Failed { backend, error });
        }
        crate::endpoint::ModelResolution::ConfiguredMissing {
            configured,
            available,
        } => {
            let error = anyhow::anyhow!(
                "the profile asks for model `{configured}`, but {} serves: {}.\n\
                 Fix `model_id` in the profile, or remove it to choose at launch.",
                backend.base_url(),
                available.join(", ")
            );
            screen.fail(STEP_MODEL, format!("`{configured}` is not served"));
            guard.terminal().draw(|f| view::draw_startup(f, &screen))?;
            return Ok(StartupOutcome::Failed { backend, error });
        }
        crate::endpoint::ModelResolution::Ambiguous { available } => {
            match prompt_for_model(guard, &mut screen, input, available).await? {
                Some(id) => {
                    screen.finish(STEP_MODEL, id.clone());
                    id
                }
                None => return Ok(StartupOutcome::Cancelled { backend }),
            }
        }
    };

    guard.terminal().draw(|f| view::draw_startup(f, &screen))?;
    Ok(StartupOutcome::Ready { backend, model_id })
}

/// Ask which model to use when the server offers several.
///
/// llmx never picks for the user here: an arbitrary choice would produce a
/// session that looks correct and silently runs the wrong weights.
async fn prompt_for_model(
    guard: &mut TerminalGuard,
    screen: &mut StartupState,
    input: &mut mpsc::Receiver<Action>,
    available: Vec<String>,
) -> Result<Option<String>> {
    screen.update(2, "several models served — choose one");
    screen.model_choices = Some(available.clone());

    let result = loop {
        guard.terminal().draw(|f| view::draw_startup(f, screen))?;
        match input.recv().await {
            Some(Action::Digit(n)) => {
                let index = (n as usize).saturating_sub(1);
                if let Some(choice) = available.get(index) {
                    break Some(choice.clone());
                }
            }
            Some(Action::Quit) | Some(Action::Interrupt) | None => break None,
            _ => {}
        }
    };

    screen.model_choices = None;
    Ok(result)
}

/// First line of a multi-line message, for single-line status rows.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).to_string()
}
