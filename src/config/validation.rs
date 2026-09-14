//! Structural checks applied after a config file parses.
//!
//! Parsing catches type errors; these catch configurations that are
//! well-formed TOML but cannot possibly work, and say why in terms the user
//! can act on.

use crate::config::model::BackendKind;
use crate::config::model::ConfigFile;
use crate::config::model::ModelProfile;
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Profile id the problem belongs to, or `None` for document-level issues.
    pub profile: Option<String>,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.profile {
            Some(id) => write!(f, "model `{id}`: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

/// Collected validation failures, reported all at once so the user can fix the
/// whole file in one pass rather than one error per run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, err) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "  - {err}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

pub fn validate(config: &ConfigFile) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if config.models.is_empty() {
        errors.push(ValidationError {
            profile: None,
            message: "no `[[models]]` entries defined".to_string(),
        });
    }

    if config.settings.codex_bin.trim().is_empty() {
        errors.push(ValidationError {
            profile: None,
            message: "settings.codex_bin must not be empty".to_string(),
        });
    }

    if config.settings.health_timeout_secs == 0 {
        errors.push(ValidationError {
            profile: None,
            message: "settings.health_timeout_secs must be greater than 0".to_string(),
        });
    }

    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (index, profile) in config.models.iter().enumerate() {
        if let Some(first) = seen.insert(profile.id.as_str(), index) {
            errors.push(ValidationError {
                profile: Some(profile.id.clone()),
                message: format!(
                    "duplicate id (also defined at models[{first}]); ids must be unique"
                ),
            });
        }
        validate_profile(profile, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errors))
    }
}

fn validate_profile(profile: &ModelProfile, errors: &mut Vec<ValidationError>) {
    let mut push = |message: String| {
        errors.push(ValidationError {
            profile: Some(profile.id.clone()),
            message,
        })
    };

    if profile.id.trim().is_empty() {
        push("id must not be empty".to_string());
    } else if !profile
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        push(
            "id may only contain ASCII letters, digits, `-`, `_` and `.` because it is used as a \
             filename"
                .to_string(),
        );
    }

    if profile.name.trim().is_empty() {
        push("name must not be empty".to_string());
    }

    let parsed_url = match reqwest::Url::parse(&profile.endpoint.url) {
        Ok(url) => {
            if !matches!(url.scheme(), "http" | "https") {
                push(format!(
                    "endpoint.url scheme must be http or https, found `{}`",
                    url.scheme()
                ));
            }
            if url.host_str().is_none() {
                push(format!(
                    "endpoint.url `{}` has no host component",
                    profile.endpoint.url
                ));
            }
            Some(url)
        }
        Err(err) => {
            push(format!(
                "endpoint.url `{}` is not a valid URL: {err}",
                profile.endpoint.url
            ));
            None
        }
    };

    if profile
        .endpoint
        .url
        .trim_end_matches('/')
        .ends_with("/chat/completions")
    {
        push(
            "endpoint.url should be the API base (ending in `/v1`), not the chat completions path"
                .to_string(),
        );
    }

    if let Some(env_key) = &profile.endpoint.env_key {
        if env_key.trim().is_empty() {
            push("endpoint.env_key must not be empty when present".to_string());
        }
    }

    if let Some(ctx) = profile.context_window {
        if ctx == 0 {
            push("context_window must be greater than 0".to_string());
        }
    }
    if let Some(max_out) = profile.max_output_tokens {
        if max_out == 0 {
            push("max_output_tokens must be greater than 0".to_string());
        }
        if let Some(ctx) = profile.context_window {
            if max_out >= ctx {
                push(format!(
                    "max_output_tokens ({max_out}) must be smaller than context_window ({ctx})"
                ));
            }
        }
    }

    match profile.backend {
        BackendKind::Local => {
            if profile.ssh.is_some() {
                push("`[models.ssh]` is only valid for backend = \"ssh\"".to_string());
            }
            if let Some(runtime) = &profile.runtime {
                if runtime.command.trim().is_empty() {
                    push("runtime.command must not be empty".to_string());
                }
                if runtime.ready_timeout_secs == Some(0) {
                    push("runtime.ready_timeout_secs must be greater than 0".to_string());
                }
            }
        }
        BackendKind::Ssh => {
            if profile.runtime.is_some() {
                push(
                    "`[models.runtime]` is only valid for backend = \"local\"; llmx does not start \
                     remote processes"
                        .to_string(),
                );
            }
            match &profile.ssh {
                None => push("backend = \"ssh\" requires a `[models.ssh]` table".to_string()),
                Some(ssh) => {
                    if ssh.host.trim().is_empty() {
                        push("ssh.host must not be empty".to_string());
                    }
                    if ssh.local_port == 0 {
                        push("ssh.local_port must be in 1..=65535".to_string());
                    }
                    if ssh.remote_port == 0 {
                        push("ssh.remote_port must be in 1..=65535".to_string());
                    }
                    if ssh.remote_host.trim().is_empty() {
                        push("ssh.remote_host must not be empty".to_string());
                    }
                    if ssh.ready_timeout_secs == Some(0) {
                        push("ssh.ready_timeout_secs must be greater than 0".to_string());
                    }

                    // The forward binds `local_port`, so the endpoint the
                    // agent talks to has to be that same port on loopback.
                    // Getting this wrong produces a confusing "endpoint never
                    // became healthy" instead of an obvious mistake.
                    if let Some(url) = &parsed_url {
                        if let Some(port) = url.port_or_known_default() {
                            if port != ssh.local_port {
                                push(format!(
                                    "endpoint.url port ({port}) does not match ssh.local_port \
                                     ({}); the tunnel binds ssh.local_port, so they must agree",
                                    ssh.local_port
                                ));
                            }
                        }
                        if !is_loopback_host(url.host_str().unwrap_or_default()) {
                            push(format!(
                                "endpoint.url host must be loopback for an SSH profile (the \
                                 forward binds locally), found `{}`",
                                url.host_str().unwrap_or_default()
                            ));
                        }
                    }
                }
            }
        }
        BackendKind::Remote => {
            if profile.runtime.is_some() {
                push("`[models.runtime]` is only valid for backend = \"local\"".to_string());
            }
            if profile.ssh.is_some() {
                push(
                    "`[models.ssh]` is only valid for backend = \"ssh\"; a direct network endpoint \
                     needs no tunnel"
                        .to_string(),
                );
            }
        }
    }

    if let Some(resources) = &profile.resources {
        for (field, value) in [
            ("estimated_ram_gb", resources.estimated_ram_gb),
            ("min_ram_gb", resources.min_ram_gb),
            ("recommended_ram_gb", resources.recommended_ram_gb),
        ] {
            if let Some(v) = value {
                if !v.is_finite() || v <= 0.0 {
                    push(format!("resources.{field} must be a positive number"));
                }
            }
        }
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}
