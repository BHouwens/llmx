use super::loader::load_from_path;
use super::loader::parse;
use super::model::BackendKind;
use super::model::Isolation;
use super::model::Settings;

const VALID: &str = r#"
[settings]
codex_bin = "codex-local"
health_timeout_secs = 120

[[models]]
id = "qwen38-27b"
name = "Qwen3.8 27B"
model_id = "Qwen3.8-27B"
backend = "local"
context_window = 131072
max_output_tokens = 32768

[models.endpoint]
url = "http://127.0.0.1:8000/v1"

[models.runtime]
command = "mlx_lm.server"
args = ["--model", "mlx-community/Qwen3.8-27B-6bit", "--port", "8000"]

[[models]]
id = "deepseek-v4-spark"
name = "DeepSeek V4 Flash"
backend = "ssh"
context_window = 262144
max_output_tokens = 32768

[models.endpoint]
url = "http://127.0.0.1:8100/v1"

[models.ssh]
host = "spark"
local_port = 8100
remote_host = "127.0.0.1"
remote_port = 8000
"#;

#[test]
fn valid_config_parses() {
    let config = parse(VALID).expect("valid config should parse");

    assert_eq!(config.settings.codex_bin, "codex-local");
    assert_eq!(config.settings.health_timeout_secs, 120);
    assert_eq!(config.settings.isolation, Isolation::Overrides);
    assert_eq!(config.models.len(), 2);

    let local = &config.models[0];
    assert_eq!(local.backend, BackendKind::Local);
    assert_eq!(local.model_id.as_deref(), Some("Qwen3.8-27B"));
    assert_eq!(local.context_window, Some(131_072));
    assert_eq!(
        local.runtime.as_ref().map(|r| r.command.as_str()),
        Some("mlx_lm.server")
    );

    let remote = &config.models[1];
    assert_eq!(remote.backend, BackendKind::Ssh);
    assert_eq!(remote.model_id, None, "model id is discovered when omitted");
    assert_eq!(remote.ssh.as_ref().unwrap().local_port, 8100);
}

#[test]
fn settings_table_is_optional() {
    let text = r#"
[[models]]
id = "m"
name = "M"
backend = "remote"

[models.endpoint]
url = "http://192.168.1.50:8000/v1"
"#;
    let config = parse(text).expect("config without [settings] should parse");
    assert_eq!(config.settings.codex_bin, "codex-local");
    assert_eq!(config.settings.health_timeout_secs, 180);
}

/// Regression guard: llmx must never default to the upstream `codex` binary,
/// which is a separate install with a separate home directory.
#[test]
fn default_agent_is_codex_local_not_codex() {
    assert_eq!(Settings::default().codex_bin, "codex-local");
}

#[test]
fn duplicate_ids_are_rejected() {
    let text = r#"
[[models]]
id = "dup"
name = "One"
backend = "remote"
[models.endpoint]
url = "http://127.0.0.1:9000/v1"

[[models]]
id = "dup"
name = "Two"
backend = "remote"
[models.endpoint]
url = "http://127.0.0.1:9001/v1"
"#;
    let err = parse(text)
        .expect_err("duplicate ids must fail")
        .to_string();
    assert!(err.contains("duplicate id"), "unhelpful error: {err}");
    assert!(err.contains("dup"), "error should name the id: {err}");
}

#[test]
fn zero_port_is_rejected() {
    let text = r#"
[[models]]
id = "bad-port"
name = "Bad"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:8100/v1"
[models.ssh]
host = "spark"
local_port = 0
remote_port = 8000
"#;
    let err = parse(text).expect_err("port 0 must fail").to_string();
    assert!(err.contains("local_port"), "unhelpful error: {err}");
}

#[test]
fn out_of_range_port_is_rejected_at_parse_time() {
    let text = r#"
[[models]]
id = "bad-port"
name = "Bad"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:8100/v1"
[models.ssh]
host = "spark"
local_port = 70000
remote_port = 8000
"#;
    let err = parse(text).expect_err("port > 65535 must fail").to_string();
    assert!(
        err.contains("local_port") || err.contains("out of range") || err.contains("invalid"),
        "unhelpful error: {err}"
    );
}

/// The forward binds `ssh.local_port`; pointing the endpoint somewhere else
/// produces a mystifying timeout, so it is caught here instead.
#[test]
fn ssh_endpoint_port_must_match_the_forward() {
    let text = r#"
[[models]]
id = "mismatch"
name = "Mismatch"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:9999/v1"
[models.ssh]
host = "spark"
local_port = 8100
remote_port = 8000
"#;
    let err = parse(text)
        .expect_err("mismatched ports must fail")
        .to_string();
    assert!(err.contains("does not match ssh.local_port"), "got: {err}");
}

#[test]
fn ssh_profile_requires_ssh_table() {
    let text = r#"
[[models]]
id = "no-ssh"
name = "No SSH"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:8100/v1"
"#;
    let err = parse(text).expect_err("must fail").to_string();
    assert!(
        err.contains("requires a `[models.ssh]` table"),
        "got: {err}"
    );
}

#[test]
fn runtime_is_rejected_for_ssh_profiles() {
    let text = r#"
[[models]]
id = "weird"
name = "Weird"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:8100/v1"
[models.ssh]
host = "spark"
local_port = 8100
remote_port = 8000
[models.runtime]
command = "vllm"
"#;
    let err = parse(text).expect_err("must fail").to_string();
    assert!(
        err.contains("only valid for backend = \"local\""),
        "got: {err}"
    );
}

#[test]
fn unknown_backend_names_the_valid_options() {
    let text = r#"
[[models]]
id = "x"
name = "X"
backend = "telepathy"
[models.endpoint]
url = "http://127.0.0.1:8000/v1"
"#;
    let err = parse(text).expect_err("must fail").to_string();
    assert!(
        err.contains("local"),
        "error should list valid values: {err}"
    );
}

#[test]
fn unknown_keys_are_rejected_rather_than_silently_ignored() {
    let text = r#"
[[models]]
id = "typo"
name = "Typo"
backend = "remote"
contxt_window = 1000
[models.endpoint]
url = "http://127.0.0.1:8000/v1"
"#;
    let err = parse(text).expect_err("typo must fail").to_string();
    assert!(
        err.contains("contxt_window"),
        "error should name the key: {err}"
    );
}

#[test]
fn chat_completions_url_is_rejected() {
    let text = r#"
[[models]]
id = "wrong-url"
name = "Wrong"
backend = "remote"
[models.endpoint]
url = "http://127.0.0.1:8000/v1/chat/completions"
"#;
    let err = parse(text).expect_err("must fail").to_string();
    assert!(err.contains("should be the API base"), "got: {err}");
}

#[test]
fn max_output_must_be_smaller_than_context() {
    let text = r#"
[[models]]
id = "silly"
name = "Silly"
backend = "remote"
context_window = 8192
max_output_tokens = 8192
[models.endpoint]
url = "http://127.0.0.1:8000/v1"
"#;
    let err = parse(text).expect_err("must fail").to_string();
    assert!(
        err.contains("must be smaller than context_window"),
        "got: {err}"
    );
}

#[test]
fn id_must_be_filename_safe() {
    let text = r#"
[[models]]
id = "../escape"
name = "Escape"
backend = "remote"
[models.endpoint]
url = "http://127.0.0.1:8000/v1"
"#;
    let err = parse(text).expect_err("must fail").to_string();
    assert!(err.contains("used as a filename"), "got: {err}");
}

#[test]
fn every_validation_problem_is_reported_at_once() {
    let text = r#"
[[models]]
id = "a"
name = ""
backend = "remote"
context_window = 0
[models.endpoint]
url = "not a url"
"#;
    let err = parse(text).expect_err("must fail").to_string();
    for expected in [
        "name must not be empty",
        "context_window",
        "not a valid URL",
    ] {
        assert!(err.contains(expected), "missing `{expected}` in:\n{err}");
    }
}

#[test]
fn empty_model_list_is_rejected() {
    let err = parse("").expect_err("must fail").to_string();
    assert!(err.contains("no `[[models]]` entries"), "got: {err}");
}

// ------------------------------------------------------- ssh command line --

#[test]
fn ssh_args_build_the_expected_local_forward() {
    let config = parse(VALID).expect("parse");
    let ssh = config.models[1].ssh.as_ref().expect("ssh table");

    assert_eq!(
        ssh.args(),
        vec![
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-L",
            "8100:127.0.0.1:8000",
            "spark",
        ]
    );
}

#[test]
fn ssh_args_map_a_non_loopback_remote_host() {
    let text = r#"
[[models]]
id = "h200"
name = "H200"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:8200/v1"
[models.ssh]
host = "h200-box"
local_port = 8200
remote_host = "10.0.0.5"
remote_port = 8000
extra_args = ["-o", "ServerAliveInterval=30"]
"#;
    let config = parse(text).expect("parse");
    let ssh = config.models[0].ssh.as_ref().expect("ssh table");

    assert_eq!(
        ssh.args(),
        vec![
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-L",
            "8200:10.0.0.5:8000",
            "-o",
            "ServerAliveInterval=30",
            "h200-box",
        ],
        "extra args belong before the host, and the forward maps local:remote_host:remote_port"
    );
}

#[test]
fn remote_host_defaults_to_loopback_on_the_remote_side() {
    let text = r#"
[[models]]
id = "s"
name = "S"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:8100/v1"
[models.ssh]
host = "spark"
local_port = 8100
remote_port = 8000
"#;
    let config = parse(text).expect("parse");
    let ssh = config.models[0].ssh.as_ref().expect("ssh");
    assert_eq!(
        ssh.remote_host, "127.0.0.1",
        "defaulting to loopback keeps the remote API port off the network"
    );
}

#[test]
fn load_from_path_reports_the_file_it_could_not_read() {
    let missing = std::path::Path::new("/nonexistent/llmx/models.toml");
    let err = format!("{:#}", load_from_path(missing).expect_err("must fail"));
    assert!(err.contains("/nonexistent/llmx/models.toml"), "got: {err}");
    assert!(
        err.contains("LLMX_CONFIG"),
        "should mention the override: {err}"
    );
}

/// The example file is the first thing a new user copies. If it stops
/// parsing, the documented starting point is broken.
#[test]
fn the_shipped_example_config_is_valid() {
    let text = include_str!("../../examples/models.toml");
    let config = parse(text).expect("examples/models.toml must parse and validate");

    assert!(
        config.models.len() >= 5,
        "examples should cover every backend"
    );
    assert_eq!(
        config.settings.codex_bin, "codex-local",
        "the example must target codex-local, not upstream codex"
    );

    for kind in [BackendKind::Local, BackendKind::Ssh, BackendKind::Remote] {
        assert!(
            config.models.iter().any(|p| p.backend == kind),
            "no example for backend {kind:?}"
        );
    }
}
