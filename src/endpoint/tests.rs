use super::health::is_reachable;
use super::health::probe;
use super::health::wait_until_ready;
use super::health::WaitError;
use super::openai::resolve_model;
use super::openai::ModelResolution;
use super::openai::ServedModel;
use crate::testutil::closed_port_url;
use crate::testutil::MockServer;
use std::time::Duration;
use std::time::Instant;

fn served(ids: &[&str]) -> Vec<ServedModel> {
    ids.iter()
        .map(|id| ServedModel {
            id: (*id).to_string(),
        })
        .collect()
}

// ------------------------------------------------------------ discovery --

#[tokio::test]
async fn probe_parses_a_model_list() {
    let server = MockServer::json(r#"{"object":"list","data":[{"id":"Qwen3.8-27B"}]}"#).await;

    let models = probe(&server.base_url(), None).await.expect("probe");
    assert_eq!(models, served(&["Qwen3.8-27B"]));
}

#[tokio::test]
async fn probe_ignores_extra_fields_servers_add() {
    // Real runtimes return `object`, `created`, `owned_by`, `permission`, ...
    let body = r#"{"object":"list","data":[
        {"id":"a","object":"model","created":1,"owned_by":"me","permission":[]}
    ]}"#;
    let server = MockServer::json(body).await;
    let models = probe(&server.base_url(), None).await.expect("probe");
    assert_eq!(models, served(&["a"]));
}

#[tokio::test]
async fn probe_reports_an_http_error_with_the_body() {
    let server = MockServer::status(401, "missing api key").await;

    let err = format!(
        "{:#}",
        probe(&server.base_url(), None)
            .await
            .expect_err("should fail")
    );
    assert!(err.contains("401"), "status should be visible: {err}");
    assert!(
        err.contains("missing api key"),
        "body should be visible: {err}"
    );
}

#[tokio::test]
async fn probe_reports_a_non_json_body_clearly() {
    let server = MockServer::status(200, "<html>nginx</html>").await;

    let err = format!(
        "{:#}",
        probe(&server.base_url(), None)
            .await
            .expect_err("should fail")
    );
    assert!(err.contains("not a model list"), "got: {err}");
}

#[tokio::test]
async fn is_reachable_is_false_when_nothing_is_listening() {
    let url = closed_port_url().await;
    assert!(!is_reachable(&url, None).await);
}

// --------------------------------------------------------- model choice --

#[test]
fn a_single_served_model_is_discovered_without_configuration() {
    let resolution = resolve_model(None, &served(&["only-one"]));
    assert_eq!(
        resolution,
        ModelResolution::Resolved("only-one".to_string())
    );
}

#[test]
fn a_configured_id_is_used_when_the_server_serves_it() {
    let resolution = resolve_model(Some("b"), &served(&["a", "b", "c"]));
    assert_eq!(resolution, ModelResolution::Resolved("b".to_string()));
}

/// The important negative case: with several models and no configured id,
/// llmx must ask rather than pick. Choosing silently would run the wrong
/// weights in a session that looks correct.
#[test]
fn several_models_with_no_configured_id_is_ambiguous() {
    let resolution = resolve_model(None, &served(&["a", "b"]));
    assert_eq!(
        resolution,
        ModelResolution::Ambiguous {
            available: vec!["a".to_string(), "b".to_string()],
        }
    );
}

#[test]
fn a_configured_id_the_server_does_not_serve_is_an_error_not_a_fallback() {
    let resolution = resolve_model(Some("typo"), &served(&["a", "b"]));
    assert_eq!(
        resolution,
        ModelResolution::ConfiguredMissing {
            configured: "typo".to_string(),
            available: vec!["a".to_string(), "b".to_string()],
        }
    );
}

/// Some runtimes answer `{"data": []}` while still serving completions, so an
/// empty list falls back to the configured id instead of failing.
#[test]
fn an_empty_list_falls_back_to_the_configured_id() {
    let resolution = resolve_model(Some("configured"), &[]);
    assert_eq!(
        resolution,
        ModelResolution::Resolved("configured".to_string())
    );
}

#[test]
fn an_empty_list_with_no_configured_id_has_nothing_to_use() {
    assert_eq!(resolve_model(None, &[]), ModelResolution::NoneServed);
}

// -------------------------------------------------------------- waiting --

#[tokio::test]
async fn wait_returns_as_soon_as_the_endpoint_answers() {
    let server = MockServer::json(r#"{"data":[{"id":"m"}]}"#).await;
    let (_tx, mut cancel) = tokio::sync::watch::channel(false);

    let models = wait_until_ready(
        &server.base_url(),
        None,
        Duration::from_secs(5),
        &mut cancel,
        None,
        &mut |_| {},
    )
    .await
    .expect("should become ready");

    assert_eq!(models, served(&["m"]));
    assert_eq!(
        server.request_count(),
        1,
        "a healthy endpoint needs one call"
    );
}

#[tokio::test]
async fn wait_times_out_and_reports_why() {
    let url = closed_port_url().await;
    let (_tx, mut cancel) = tokio::sync::watch::channel(false);
    let mut attempts = 0;

    let started = Instant::now();
    let err = wait_until_ready(
        &url,
        None,
        Duration::from_millis(600),
        &mut cancel,
        None,
        &mut |_| attempts += 1,
    )
    .await
    .expect_err("nothing is listening");

    let elapsed = started.elapsed();
    assert!(
        matches!(err, WaitError::Timeout { .. }),
        "expected a timeout, got {err:?}"
    );
    assert!(attempts > 0, "progress should have been reported");
    assert!(
        elapsed < Duration::from_secs(3),
        "backoff must not overrun the deadline (took {elapsed:?})"
    );

    let message = err.to_string();
    assert!(
        message.contains("refused") || message.contains("error"),
        "the timeout should carry the underlying reason: {message}"
    );
}

#[tokio::test]
async fn wait_stops_when_cancelled() {
    let url = closed_port_url().await;
    let (tx, mut cancel) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let _ = tx.send(true);
    });

    let err = wait_until_ready(
        &url,
        None,
        Duration::from_secs(30),
        &mut cancel,
        None,
        &mut |_| {},
    )
    .await
    .expect_err("should be cancelled");

    assert!(
        matches!(err, WaitError::Cancelled { .. }),
        "expected cancellation, got {err:?}"
    );
}
