//! Declarative description of a model profile.
//!
//! These types mirror the on-disk TOML schema one-to-one. Nothing in here
//! performs I/O or validation; see [`crate::config::loader`] and
//! [`crate::config::validation`].

use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Top-level document of `~/.config/llmx/models.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub models: Vec<ModelProfile>,
}

/// Launcher-wide knobs. Every field has a usable default so the `[settings]`
/// table may be omitted entirely.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Executable used to launch the coding agent. Resolved against `PATH`.
    #[serde(default = "default_codex_bin")]
    pub codex_bin: String,

    /// How long to wait for `GET <endpoint>/models` to answer before giving up.
    #[serde(default = "default_health_timeout_secs")]
    pub health_timeout_secs: u64,

    /// How the generated agent configuration reaches codex-local.
    #[serde(default)]
    pub isolation: Isolation,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            codex_bin: default_codex_bin(),
            health_timeout_secs: default_health_timeout_secs(),
            isolation: Isolation::default(),
        }
    }
}

fn default_codex_bin() -> String {
    "codex-local".to_string()
}

fn default_health_timeout_secs() -> u64 {
    180
}

/// Strategy for handing generated settings to codex-local.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// Pass every generated key as a `-c key=value` override. The user's
    /// `~/.codex-local/config.toml` still loads, so MCP servers, history and
    /// sessions are preserved. This is the default.
    #[default]
    Overrides,

    /// Point `CODEX_HOME` at a directory owned by llmx. Fully isolated: the
    /// user's own config.toml, MCP servers and session history do not load.
    CodexHome,
}

/// Transport used to reach the model server.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// A model server process on this machine, optionally started by llmx.
    Local,
    /// A model server on another machine, reached through an SSH local forward.
    Ssh,
    /// A model server reachable directly over the network (LAN, VPN, tunnel
    /// someone else manages). llmx never starts or stops anything.
    Remote,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Local => "local",
            BackendKind::Ssh => "ssh",
            BackendKind::Remote => "remote",
        }
    }
}

/// Wire protocol the served model speaks. Mirrors codex-local's `WireApi`.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    #[default]
    Chat,
    Responses,
}

impl WireApi {
    pub fn as_str(self) -> &'static str {
        match self {
            WireApi::Chat => "chat",
            WireApi::Responses => "responses",
        }
    }
}

/// One selectable entry in the launcher.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    /// Stable machine-readable key. Used by `llmx run <id>` and as the
    /// generated-config filename, so it must be filename-safe.
    pub id: String,

    /// Human-readable name shown in the picker.
    pub name: String,

    /// Model identifier the server expects. When absent, llmx discovers it
    /// from `GET <endpoint>/models`.
    #[serde(default)]
    pub model_id: Option<String>,

    pub backend: BackendKind,

    /// Short tag rendered in the picker's second column, e.g. `MLX`, `SPARK`.
    /// Defaults to the uppercased backend kind.
    #[serde(default)]
    pub label: Option<String>,

    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,

    /// Token count at which codex-local compacts history. Left unset, codex
    /// applies its own default.
    #[serde(default)]
    pub auto_compact_token_limit: Option<i64>,

    #[serde(default)]
    pub wire_api: WireApi,

    pub endpoint: EndpointConfig,

    /// Required for `backend = "local"` if llmx should be able to start the
    /// server itself; optional if the server is always started out-of-band.
    #[serde(default)]
    pub runtime: Option<RuntimeConfig>,

    /// Required for `backend = "ssh"`.
    #[serde(default)]
    pub ssh: Option<SshConfig>,

    #[serde(default)]
    pub resources: Option<ResourceHints>,

    /// Extra keys merged verbatim into the generated codex-local config.
    /// Use for things llmx has no opinion about (`approval_policy`,
    /// `model_reasoning_effort`, ...). Values are arbitrary TOML.
    #[serde(default)]
    pub codex: BTreeMap<String, toml::Value>,
}

impl ModelProfile {
    /// Short tag for the picker's backend column.
    pub fn label(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.backend.as_str().to_uppercase())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    /// Full OpenAI-compatible base URL including the version segment, e.g.
    /// `http://127.0.0.1:8000/v1`.
    pub url: String,

    /// Name of an environment variable holding the API key. llmx never stores
    /// key material itself; it only forwards the variable *name* to
    /// codex-local, which reads it at request time.
    #[serde(default)]
    pub env_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Per-profile override of `settings.health_timeout_secs`. Large local
    /// models can take minutes to load weights.
    #[serde(default)]
    pub ready_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SshConfig {
    /// An OpenSSH host alias from `~/.ssh/config`, or `user@hostname`.
    pub host: String,
    /// Port bound on this machine by the forward.
    pub local_port: u16,
    /// Address the model server listens on *as seen from the remote host*.
    #[serde(default = "default_remote_host")]
    pub remote_host: String,
    pub remote_port: u16,
    /// Extra arguments spliced in before the host, e.g.
    /// `["-o", "ServerAliveInterval=30"]`.
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub ready_timeout_secs: Option<u64>,
}

fn default_remote_host() -> String {
    "127.0.0.1".to_string()
}

impl SshConfig {
    /// Argument vector for the forward, excluding the `ssh` program name.
    ///
    /// `-N` means "do not run a remote command"; `ExitOnForwardFailure=yes`
    /// makes ssh exit non-zero instead of silently sitting there when the
    /// local port is already taken.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-N".to_string(),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            "-L".to_string(),
            format!(
                "{}:{}:{}",
                self.local_port, self.remote_host, self.remote_port
            ),
        ];
        args.extend(self.extra_args.iter().cloned());
        args.push(self.host.clone());
        args
    }
}

/// Advisory memory hints. Never used to block a launch.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResourceHints {
    pub estimated_ram_gb: Option<f64>,
    pub min_ram_gb: Option<f64>,
    pub recommended_ram_gb: Option<f64>,
}
