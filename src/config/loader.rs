//! Locating and reading `models.toml`.

use crate::config::model::ConfigFile;
use crate::config::validation::validate;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

/// Environment variable that overrides the config file location.
pub const CONFIG_ENV_VAR: &str = "LLMX_CONFIG";

/// Where llmx looks for `models.toml` when `LLMX_CONFIG` is unset.
///
/// This is `~/.config/llmx/models.toml` on every platform, deliberately: the
/// XDG-style path is what the documentation shows, and diverging on macOS
/// would make the docs wrong for the primary platform.
pub fn default_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".config").join("llmx").join("models.toml"))
}

/// Config path for this process, honouring `LLMX_CONFIG`.
pub fn config_path() -> Result<PathBuf> {
    match std::env::var_os(CONFIG_ENV_VAR) {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => default_config_path(),
    }
}

/// Parse and validate the config file at `path`.
pub fn load_from_path(path: &Path) -> Result<ConfigFile> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read llmx config at {}\n\
             Create it, or point {CONFIG_ENV_VAR} at an existing file.",
            path.display()
        )
    })?;
    parse(&text).with_context(|| format!("invalid llmx config at {}", path.display()))
}

/// Parse and validate config text. Separated from [`load_from_path`] so tests
/// need no filesystem.
pub fn parse(text: &str) -> Result<ConfigFile> {
    // `toml::de::Error`'s Display carries the line, column and a caret span.
    // Interpolating it into the message keeps that detail in the first line
    // the user sees, rather than hiding it one level down the error chain.
    let config: ConfigFile =
        toml::from_str(text).map_err(|err| anyhow!("failed to parse TOML: {err}"))?;
    validate(&config).map_err(|errors| anyhow!("configuration is not usable:\n{errors}"))?;
    Ok(config)
}

/// Parse and validate the config file for this process.
pub fn load() -> Result<(PathBuf, ConfigFile)> {
    let path = config_path()?;
    let config = load_from_path(&path)?;
    Ok((path, config))
}
