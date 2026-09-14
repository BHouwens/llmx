//! The minimal slice of the OpenAI-compatible API llmx needs.
//!
//! This is *not* an inference client. llmx only asks a server "are you up, and
//! what are you serving?" — everything else is codex-local's job.

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use std::time::Duration;

/// One entry of `GET /v1/models`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ServedModel {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ServedModel>,
}

/// Thin wrapper over a `reqwest::Client` pointed at one base URL.
#[derive(Clone)]
pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
}

impl OpenAiClient {
    pub fn new(base_url: &str, request_timeout: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            // Never route loopback traffic through a corporate proxy; that is
            // a common cause of "tunnel is up but health check fails".
            .no_proxy()
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    /// Fetch the served model list.
    ///
    /// `api_key` is passed as a bearer token when present. Most local servers
    /// ignore it; some (vLLM with `--api-key`) reject requests without it.
    pub async fn list_models(&self, api_key: Option<&str>) -> Result<Vec<ServedModel>> {
        let url = self.models_url();
        let mut request = self.http.get(&url);
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(300).collect();
            anyhow::bail!("GET {url} returned HTTP {status}: {snippet}");
        }

        let parsed: ModelsResponse = response
            .json()
            .await
            .with_context(|| format!("GET {url} returned a body that is not a model list"))?;

        Ok(parsed.data)
    }
}

/// Outcome of matching a configured model id against what a server actually
/// serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelResolution {
    /// A single unambiguous choice. Either the configured id was found, or the
    /// server serves exactly one model.
    Resolved(String),
    /// The server serves several models and llmx must not pick for the user.
    Ambiguous { available: Vec<String> },
    /// The configured id is not among the served models.
    ConfiguredMissing {
        configured: String,
        available: Vec<String>,
    },
    /// The server reported an empty model list.
    NoneServed,
}

/// Decide which model id to hand to codex-local.
///
/// The rule is deliberately conservative: llmx auto-selects only when there is
/// no choice to make. Silently picking one of several models would produce a
/// session that looks fine and uses the wrong weights.
pub fn resolve_model(configured: Option<&str>, served: &[ServedModel]) -> ModelResolution {
    let available: Vec<String> = served.iter().map(|m| m.id.clone()).collect();

    match (configured, available.len()) {
        (_, 0) => {
            // An empty list is not proof the server is broken — some runtimes
            // return `{"data": []}` while still answering completions — so
            // fall back to the configured id when there is one.
            match configured {
                Some(id) => ModelResolution::Resolved(id.to_string()),
                None => ModelResolution::NoneServed,
            }
        }
        (Some(id), _) if available.iter().any(|a| a == id) => {
            ModelResolution::Resolved(id.to_string())
        }
        (Some(id), _) => ModelResolution::ConfiguredMissing {
            configured: id.to_string(),
            available,
        },
        (None, 1) => ModelResolution::Resolved(available[0].clone()),
        (None, _) => ModelResolution::Ambiguous { available },
    }
}
