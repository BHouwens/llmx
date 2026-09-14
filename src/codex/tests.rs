use super::config::build_document;
use super::config::provider_key;
use super::config::render;
use super::config::to_cli_overrides;
use super::config::write_generated;
use super::launcher::resolve_program;
use crate::backend::ModelEndpoint;
use crate::codex::launcher::prepare_delivery;
use crate::codex::launcher::ConfigDelivery;
use crate::codex::launcher::LaunchPlan;
use crate::config::loader::parse;
use crate::config::Isolation;
use crate::config::ModelProfile;

const SPARK: &str = r#"
[[models]]
id = "deepseek-v4-spark"
name = "DeepSeek V4 Flash — DGX Spark"
backend = "ssh"
context_window = 262144
max_output_tokens = 32768
auto_compact_token_limit = 196608

[models.endpoint]
url = "http://127.0.0.1:8100/v1"

[models.ssh]
host = "spark"
local_port = 8100
remote_port = 8000
"#;

fn spark_profile() -> ModelProfile {
    parse(SPARK).expect("fixture parses").models.remove(0)
}

fn spark_endpoint() -> ModelEndpoint {
    ModelEndpoint {
        base_url: "http://127.0.0.1:8100/v1".to_string(),
        model_id: "deepseek-v4-flash".to_string(),
        context_window: Some(262_144),
        max_output_tokens: Some(32_768),
    }
}

#[test]
fn generated_config_matches_the_profile() {
    let document = build_document(&spark_profile(), &spark_endpoint());
    let rendered = render(&document).expect("render");

    // Re-parse rather than string-match, so formatting changes do not fail
    // the test but a wrong value does.
    let value: toml::Value = toml::from_str(&rendered).expect("generated config is valid TOML");

    assert_eq!(value["model"].as_str(), Some("deepseek-v4-flash"));
    assert_eq!(
        value["model_provider"].as_str(),
        Some("llmx-deepseek-v4-spark")
    );
    assert_eq!(value["model_context_window"].as_integer(), Some(262_144));
    assert_eq!(value["model_max_output_tokens"].as_integer(), Some(32_768));
    assert_eq!(
        value["model_auto_compact_token_limit"].as_integer(),
        Some(196_608)
    );

    let provider = &value["model_providers"]["llmx-deepseek-v4-spark"];
    assert_eq!(
        provider["base_url"].as_str(),
        Some("http://127.0.0.1:8100/v1")
    );
    assert_eq!(provider["wire_api"].as_str(), Some("chat"));
    assert_eq!(
        provider["requires_openai_auth"].as_bool(),
        Some(false),
        "a local or self-hosted provider must not trigger codex-local's login screen"
    );
    assert!(
        provider.get("env_key").is_none(),
        "no env_key should be emitted when the profile sets none"
    );
}

/// Every key emitted must be one codex-local actually reads. This list was
/// taken from `codex-rs/core/src/config.rs` (`ConfigToml`) and
/// `codex-rs/core/src/model_provider_info.rs` (`ModelProviderInfo`).
#[test]
fn generated_config_uses_only_keys_codex_local_understands() {
    const TOP_LEVEL: &[&str] = &[
        "model",
        "model_provider",
        "model_context_window",
        "model_max_output_tokens",
        "model_auto_compact_token_limit",
        "model_providers",
    ];
    const PROVIDER: &[&str] = &[
        "name",
        "base_url",
        "env_key",
        "env_key_instructions",
        "wire_api",
        "query_params",
        "http_headers",
        "env_http_headers",
        "request_max_retries",
        "stream_max_retries",
        "stream_idle_timeout_ms",
        "requires_openai_auth",
    ];

    let document = build_document(&spark_profile(), &spark_endpoint());
    for key in document.keys() {
        assert!(
            TOP_LEVEL.contains(&key.as_str()),
            "unknown top-level key `{key}`"
        );
    }

    let providers = document["model_providers"].as_table().expect("providers");
    for provider in providers.values() {
        for key in provider.as_table().expect("provider table").keys() {
            assert!(
                PROVIDER.contains(&key.as_str()),
                "unknown provider key `{key}`"
            );
        }
    }
}

#[test]
fn wire_api_responses_is_passed_through() {
    let text = SPARK.replace(
        "context_window = 262144",
        "context_window = 262144\nwire_api = \"responses\"",
    );
    let profile = parse(&text).expect("parses").models.remove(0);
    let document = build_document(&profile, &spark_endpoint());
    assert_eq!(
        document["model_providers"]["llmx-deepseek-v4-spark"]["wire_api"].as_str(),
        Some("responses")
    );
}

#[test]
fn only_the_name_of_an_api_key_variable_is_written() {
    let text = SPARK.replace(
        r#"url = "http://127.0.0.1:8100/v1""#,
        "url = \"http://127.0.0.1:8100/v1\"\nenv_key = \"SPARK_API_KEY\"",
    );
    let profile = parse(&text).expect("parses").models.remove(0);
    let rendered = render(&build_document(&profile, &spark_endpoint())).expect("render");

    assert!(
        rendered.contains("SPARK_API_KEY"),
        "the variable name is needed"
    );
    assert!(
        !rendered.contains("sk-"),
        "no key material may reach a generated file"
    );
}

#[test]
fn profile_passthrough_can_override_generated_values() {
    let text = format!("{SPARK}\n[models.codex]\napproval_policy = \"on-request\"\n");
    let profile = parse(&text).expect("parses").models.remove(0);
    let document = build_document(&profile, &spark_endpoint());
    assert_eq!(document["approval_policy"].as_str(), Some("on-request"));
}

#[test]
fn a_profile_without_limits_omits_them_rather_than_guessing() {
    let text = r#"
[[models]]
id = "minimal"
name = "Minimal"
backend = "remote"
[models.endpoint]
url = "http://192.168.1.50:8000/v1"
"#;
    let profile = parse(text).expect("parses").models.remove(0);
    let endpoint = ModelEndpoint {
        base_url: "http://192.168.1.50:8000/v1".to_string(),
        model_id: "m".to_string(),
        context_window: None,
        max_output_tokens: None,
    };
    let document = build_document(&profile, &endpoint);

    assert!(document.get("model_context_window").is_none());
    assert!(document.get("model_max_output_tokens").is_none());
    assert!(document.get("model_auto_compact_token_limit").is_none());
}

// --------------------------------------------------------- CLI overrides --

#[test]
fn overrides_use_dotted_paths_and_typed_toml_values() {
    let document = build_document(&spark_profile(), &spark_endpoint());
    let overrides = to_cli_overrides(&document).expect("flatten");

    assert!(
        overrides.contains(&r#"model="deepseek-v4-flash""#.to_string()),
        "strings must stay quoted so codex-local parses them as strings: {overrides:?}"
    );
    assert!(overrides.contains(&"model_context_window=262144".to_string()));
    assert!(overrides.contains(&r#"model_provider="llmx-deepseek-v4-spark""#.to_string()));
    assert!(overrides.contains(
        &r#"model_providers.llmx-deepseek-v4-spark.base_url="http://127.0.0.1:8100/v1""#
            .to_string()
    ));
    assert!(overrides.contains(
        &"model_providers.llmx-deepseek-v4-spark.requires_openai_auth=false".to_string()
    ));

    assert!(
        overrides.iter().all(|o| o.contains('=')),
        "every override must be key=value: {overrides:?}"
    );
    assert!(
        overrides.iter().all(|o| !o.contains('\n')),
        "an override spanning lines would not survive argv"
    );
}

/// A model id like `70b` must not be reinterpreted as an integer by
/// codex-local's override parser, which parses the right-hand side as TOML.
#[test]
fn a_numeric_looking_model_id_stays_a_string() {
    let endpoint = ModelEndpoint {
        model_id: "70".to_string(),
        ..spark_endpoint()
    };
    let overrides =
        to_cli_overrides(&build_document(&spark_profile(), &endpoint)).expect("flatten");
    assert!(
        overrides.contains(&r#"model="70""#.to_string()),
        "got: {overrides:?}"
    );
}

#[test]
fn a_key_containing_a_dot_is_refused_with_a_way_out() {
    let mut document = build_document(&spark_profile(), &spark_endpoint());
    let mut nested = toml::value::Table::new();
    nested.insert("x.y".to_string(), toml::Value::Integer(1));
    document.insert("weird".to_string(), toml::Value::Table(nested));

    let err = to_cli_overrides(&document)
        .expect_err("must refuse")
        .to_string();
    assert!(err.contains("x.y"), "should name the key: {err}");
    assert!(
        err.contains("codex_home"),
        "should offer the alternative: {err}"
    );
}

// ------------------------------------------------------------- delivery --

#[test]
fn override_delivery_writes_nothing_into_the_users_codex_home() {
    let temp = tempfile::tempdir().expect("tempdir");
    let document = build_document(&spark_profile(), &spark_endpoint());

    let path = write_generated(temp.path(), "deepseek-v4-spark", &document).expect("write");
    assert_eq!(path, temp.path().join("deepseek-v4-spark.toml"));
    assert!(path.exists());

    let delivery = prepare_delivery(
        Isolation::Overrides,
        temp.path(),
        "deepseek-v4-spark",
        &document,
    )
    .expect("prepare");
    assert!(matches!(delivery, ConfigDelivery::Overrides(_)));

    // The generated tree must be self-contained: only llmx's own file.
    let entries: Vec<String> = std::fs::read_dir(temp.path())
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["deepseek-v4-spark.toml".to_string()]);
}

#[test]
fn codex_home_delivery_creates_the_directory_it_points_at() {
    let temp = tempfile::tempdir().expect("tempdir");
    let document = build_document(&spark_profile(), &spark_endpoint());

    let delivery = prepare_delivery(
        Isolation::CodexHome,
        temp.path(),
        "deepseek-v4-spark",
        &document,
    )
    .expect("prepare");

    let ConfigDelivery::CodexHome(home) = delivery else {
        panic!("expected CodexHome delivery");
    };
    // codex-local resolves CODEX_HOME with canonicalize(), so the directory
    // has to exist before the process starts.
    assert!(home.is_dir(), "CODEX_HOME must exist before launch");
    assert!(home.join("config.toml").is_file());
    assert!(
        home.starts_with(temp.path()),
        "the isolated home must live under llmx's own data dir, not the user's"
    );
}

// ------------------------------------------------------------- launching --

#[test]
fn the_launch_plan_targets_codex_local_in_the_current_directory() {
    let document = build_document(&spark_profile(), &spark_endpoint());
    let plan = LaunchPlan {
        program: "codex-local".to_string(),
        cwd: std::path::PathBuf::from("/Users/someone/code/project"),
        delivery: ConfigDelivery::Overrides(to_cli_overrides(&document).expect("flatten")),
    };

    let args = plan.args();
    assert_eq!(args[0], "--cd");
    assert_eq!(args[1], "/Users/someone/code/project");

    let overrides: Vec<&String> = args
        .iter()
        .skip(2)
        .step_by(2)
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    assert!(
        overrides.iter().all(|flag| *flag == "-c"),
        "each override must be introduced by its own -c: {args:?}"
    );

    let line = plan.command_line();
    assert!(line.starts_with("codex-local --cd"), "got: {line}");
    assert!(
        !line.contains("CODEX_HOME"),
        "override mode must not set CODEX_HOME: {line}"
    );
}

#[test]
fn codex_home_mode_sets_the_env_var_in_the_logged_command_line() {
    let plan = LaunchPlan {
        program: "codex-local".to_string(),
        cwd: std::path::PathBuf::from("/tmp/p"),
        delivery: ConfigDelivery::CodexHome(std::path::PathBuf::from("/tmp/llmx/home-x")),
    };
    let line = plan.command_line();
    assert!(
        line.starts_with("CODEX_HOME=/tmp/llmx/home-x codex-local"),
        "got: {line}"
    );
    assert!(
        !line.contains(" -c "),
        "codex_home mode passes no overrides: {line}"
    );
}

#[test]
fn a_missing_agent_executable_says_what_to_do() {
    let err = resolve_program("llmx-definitely-not-installed")
        .expect_err("must fail")
        .to_string();
    assert!(err.contains("not found on PATH"), "got: {err}");
    assert!(
        err.contains("settings.codex_bin"),
        "should name the fix: {err}"
    );
}

#[test]
fn provider_keys_are_namespaced_by_profile_id() {
    assert_eq!(provider_key("spark"), "llmx-spark");
    assert_ne!(
        provider_key("a"),
        provider_key("b"),
        "two profiles must never share a provider entry"
    );
}
