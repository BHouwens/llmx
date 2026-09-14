//! What the screens render from.

use crate::backend::BackendStatus;
use crate::config::ModelProfile;
use crate::system::MemoryVerdict;
use std::path::PathBuf;

/// One row of the picker.
pub struct ProfileRow {
    pub profile: ModelProfile,
    pub status: BackendStatus,
    pub memory: MemoryVerdict,
}

/// State behind the model picker screen.
pub struct PickerState {
    pub rows: Vec<ProfileRow>,
    pub selected: usize,
    pub cwd: PathBuf,
    pub total_ram_gb: Option<f64>,
    pub config_path: PathBuf,
    pub show_logs: bool,
    /// Set while background probes are in flight.
    pub refreshing: bool,
}

impl PickerState {
    pub fn new(
        profiles: Vec<ModelProfile>,
        cwd: PathBuf,
        config_path: PathBuf,
        total_ram_gb: Option<f64>,
    ) -> Self {
        let rows = profiles
            .into_iter()
            .map(|profile| {
                let memory = crate::system::assess(profile.resources.as_ref(), total_ram_gb);
                ProfileRow {
                    profile,
                    status: BackendStatus::Stopped,
                    memory,
                }
            })
            .collect();

        Self {
            rows,
            selected: 0,
            cwd,
            total_ram_gb,
            config_path,
            show_logs: false,
            refreshing: true,
        }
    }

    pub fn selected_row(&self) -> Option<&ProfileRow> {
        self.rows.get(self.selected)
    }

    pub fn move_up(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.rows.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn move_down(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.rows.len();
    }
}

/// One line of the startup screen's progress list.
#[derive(Debug, Clone)]
pub struct Step {
    pub title: String,
    pub state: StepState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Active(String),
    Done(String),
    Failed(String),
}

/// State behind the startup screen shown between Enter and codex-local.
pub struct StartupState {
    pub profile_name: String,
    pub backend_description: String,
    pub steps: Vec<Step>,
    /// Populated when the endpoint serves several models and llmx must ask.
    pub model_choices: Option<Vec<String>>,
    pub elapsed_secs: u64,
}

impl StartupState {
    pub fn new(profile_name: String, backend_description: String, titles: &[&str]) -> Self {
        Self {
            profile_name,
            backend_description,
            steps: titles
                .iter()
                .map(|t| Step {
                    title: (*t).to_string(),
                    state: StepState::Pending,
                })
                .collect(),
            model_choices: None,
            elapsed_secs: 0,
        }
    }

    pub fn start(&mut self, index: usize, detail: impl Into<String>) {
        if let Some(step) = self.steps.get_mut(index) {
            step.state = StepState::Active(detail.into());
        }
    }

    pub fn update(&mut self, index: usize, detail: impl Into<String>) {
        if let Some(step) = self.steps.get_mut(index) {
            step.state = StepState::Active(detail.into());
        }
    }

    pub fn finish(&mut self, index: usize, detail: impl Into<String>) {
        if let Some(step) = self.steps.get_mut(index) {
            step.state = StepState::Done(detail.into());
        }
    }

    pub fn fail(&mut self, index: usize, detail: impl Into<String>) {
        if let Some(step) = self.steps.get_mut(index) {
            step.state = StepState::Failed(detail.into());
        }
    }
}

/// Render `131072` as `128k`, because a context window is easier to compare
/// at a glance in those units.
pub fn format_tokens(tokens: Option<u64>) -> String {
    match tokens {
        None => "—".to_string(),
        Some(t) if t >= 1000 => format!("{}k", t / 1024),
        Some(t) => t.to_string(),
    }
}

/// Shorten a path by replacing the home directory prefix with `~`.
pub fn tilde(path: &std::path::Path) -> String {
    let display = path.display().to_string();
    match dirs::home_dir() {
        Some(home) => {
            let home = home.display().to_string();
            match display.strip_prefix(&home) {
                Some(rest) => format!("~{rest}"),
                None => display,
            }
        }
        None => display,
    }
}
