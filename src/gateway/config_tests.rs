use super::{
    config::{GatewayConfig, GatewayConfigError, SemanticRoutingMode},
    test_support::TestEnvironment,
};

fn valid_config() -> &'static str {
    r#"
config_version = 2

[server]
host = "127.0.0.1"
port = 3000
api_key_env = "OCTOROUTE_API_KEY"
max_request_bytes = 8388608

[upstreams.local]
kind = "llama_cpp"
name = "strix"
base_url = "http://127.0.0.1:8080"
model = "puzzle-75b"
context_window = 65536
context_safety_tokens = 1024
max_in_flight = 1
capabilities = ["chat", "stream"]
health_path = "/health"
slots_path = "/slots?fail_on_no_slot=1"
input_tokens_path = "/v1/chat/completions/input_tokens"

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
auto_model = "openrouter/auto"
cost_quality_tradeoff = 9
app_title = "Octoroute"

[routing]
default = "prefer_local"
fallback_before_commit = true

[observability]
log_level = "info"
"#
}

#[test]
fn valid_v2_config_resolves_secrets_without_exposing_them() {
    let config = GatewayConfig::from_toml(valid_config(), &TestEnvironment::gateway())
        .expect("valid v2 configuration");

    assert_eq!(config.version(), 2);
    assert_eq!(config.server().port(), 3000);
    assert_eq!(config.server().max_header_bytes(), 32768);
    assert_eq!(config.server().max_in_flight(), 32);
    assert_eq!(config.server().requests_per_minute(), 120);
    assert_eq!(config.local().model(), "puzzle-75b");
    assert_eq!(config.local().default_max_output_tokens(), 4096);
    assert_eq!(config.local().health_cache_ttl_ms(), 1000);
    assert_eq!(config.local().probe_timeout_ms(), 2000);
    assert_eq!(config.local().first_byte_timeout_ms(), None);
    assert_eq!(config.openrouter().auto_model(), "openrouter/auto");
    assert_eq!(config.openrouter().max_in_flight(), 8);
    assert_eq!(config.openrouter().health_cache_ttl_ms(), 10000);
    assert_eq!(config.openrouter().probe_timeout_ms(), 3000);
    assert_eq!(
        config.routing().semantic_mode(),
        SemanticRoutingMode::Shadow
    );
    assert_eq!(config.routing().decision_timeout_ms(), 30_000);
    assert_eq!(config.routing().local_success_threshold(), 0.50);
    assert_eq!(config.routing().boundary_threshold_step(), 0.10);

    let debug = format!("{config:?}");
    assert!(!debug.contains("inbound-secret"));
    assert!(!debug.contains("openrouter-secret"));
}

#[test]
fn semantic_routing_modes_are_additive_and_closed() {
    for (configured, expected) in [
        ("disabled", SemanticRoutingMode::Disabled),
        ("shadow", SemanticRoutingMode::Shadow),
        ("enforced", SemanticRoutingMode::Enforced),
    ] {
        let input = valid_config().replace(
            "fallback_before_commit = true",
            &format!("fallback_before_commit = true\nsemantic_mode = \"{configured}\""),
        );
        let config = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
            .expect("supported semantic routing mode");
        assert_eq!(config.routing().semantic_mode(), expected);
    }

    let input = valid_config().replace(
        "fallback_before_commit = true",
        "fallback_before_commit = true\nsemantic_mode = \"experimental\"",
    );
    assert!(matches!(
        GatewayConfig::from_toml(&input, &TestEnvironment::gateway()),
        Err(GatewayConfigError::Parse { .. })
    ));
}

#[test]
fn default_cloud_destination_is_current_openrouter_auto() {
    let config = super::test_support::gateway_config("http://127.0.0.1:8080", "", "", "");

    assert_eq!(config.openrouter().auto_model(), "openrouter/auto");
}

#[test]
fn semantic_routing_timeout_must_be_positive() {
    let input = valid_config().replace(
        "fallback_before_commit = true",
        "fallback_before_commit = true\ndecision_timeout_ms = 0",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("zero semantic routing timeout must fail startup");

    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "routing.decision_timeout_ms"
    ));
}

#[test]
fn semantic_routing_probability_policy_is_configurable() {
    let input = valid_config().replace(
        "fallback_before_commit = true",
        "fallback_before_commit = true\nlocal_success_threshold = 0.65\nboundary_threshold_step = 0.15",
    );
    let config = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect("valid semantic probability policy");

    assert_eq!(config.routing().local_success_threshold(), 0.65);
    assert_eq!(config.routing().boundary_threshold_step(), 0.15);
}

#[test]
fn semantic_routing_probability_policy_must_remain_bounded() {
    for (field, value) in [
        ("local_success_threshold", "-0.01"),
        ("local_success_threshold", "1.01"),
        ("local_success_threshold", "nan"),
        ("boundary_threshold_step", "-0.01"),
        ("boundary_threshold_step", "0.26"),
        ("boundary_threshold_step", "inf"),
    ] {
        let input = valid_config().replace(
            "fallback_before_commit = true",
            &format!("fallback_before_commit = true\n{field} = {value}"),
        );
        let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
            .expect_err("invalid semantic probability policy must fail startup");

        assert!(matches!(
            error,
            GatewayConfigError::Invalid {
                field: ref actual,
                ..
            } if actual == &format!("routing.{field}")
        ));
    }

    let input = valid_config().replace(
        "fallback_before_commit = true",
        "fallback_before_commit = true\nlocal_success_threshold = 0.60\nboundary_threshold_step = 0.25",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("the strictest boundary threshold must not exceed one");
    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "routing.boundary_threshold_step"
    ));
}

#[test]
fn session_latch_is_opt_in_bounded_and_enforced_only() {
    let default = GatewayConfig::from_toml(valid_config(), &TestEnvironment::gateway())
        .expect("default configuration");
    assert!(default.routing().session_latch().is_none());

    let configured = valid_config().replace(
        "fallback_before_commit = true",
        "fallback_before_commit = true\nsemantic_mode = \"enforced\"\nsession_latch_enabled = true\nsession_latch_ttl_ms = 60000\nsession_latch_max_entries = 64\nsession_latch_evidence_threshold = 3",
    );
    let config = GatewayConfig::from_toml(&configured, &TestEnvironment::gateway())
        .expect("bounded enforced session latch");
    let latch = config.routing().session_latch().expect("enabled latch");
    assert_eq!(latch.ttl_ms(), 60_000);
    assert_eq!(latch.max_entries(), 64);
    assert_eq!(latch.evidence_threshold(), 3);

    for (field, value) in [
        ("session_latch_ttl_ms", "999"),
        ("session_latch_ttl_ms", "86400001"),
        ("session_latch_max_entries", "0"),
        ("session_latch_max_entries", "10001"),
        ("session_latch_evidence_threshold", "1"),
        ("session_latch_evidence_threshold", "11"),
    ] {
        let input = valid_config().replace(
            "fallback_before_commit = true",
            &format!(
                "fallback_before_commit = true\nsemantic_mode = \"enforced\"\nsession_latch_enabled = true\n{field} = {value}"
            ),
        );
        let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
            .expect_err("invalid latch bound must fail startup");
        assert!(matches!(
            error,
            GatewayConfigError::Invalid { field: ref actual, .. }
                if actual == &format!("routing.{field}")
        ));
    }

    let shadow = valid_config().replace(
        "fallback_before_commit = true",
        "fallback_before_commit = true\nsession_latch_enabled = true",
    );
    let error = GatewayConfig::from_toml(&shadow, &TestEnvironment::gateway())
        .expect_err("an active latch must require enforced semantic mode");
    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "routing.session_latch_enabled"
    ));
}

#[test]
fn gateway_and_cloud_limits_must_be_positive() {
    for (needle, replacement, expected_field) in [
        (
            "max_request_bytes = 8388608",
            "max_request_bytes = 8388608\nmax_header_bytes = 0",
            "server.max_header_bytes",
        ),
        (
            "max_request_bytes = 8388608",
            "max_request_bytes = 8388608\nmax_in_flight = 0",
            "server.max_in_flight",
        ),
        (
            "max_request_bytes = 8388608",
            "max_request_bytes = 8388608\nrequests_per_minute = 0",
            "server.requests_per_minute",
        ),
        (
            "app_title = \"Octoroute\"",
            "app_title = \"Octoroute\"\nmax_in_flight = 0",
            "upstreams.openrouter.max_in_flight",
        ),
    ] {
        let input = valid_config().replace(needle, replacement);
        let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
            .expect_err("zero safety limit must fail startup");

        assert!(matches!(
            error,
            GatewayConfigError::Invalid {
                field: ref actual,
                ..
            } if actual == expected_field
        ));
    }
}

#[test]
fn configured_outbound_header_values_reject_control_characters() {
    for (needle, replacement, expected_field) in [
        (
            "name = \"strix\"",
            r#"name = "strix\nInjected""#,
            "upstreams.local.name",
        ),
        (
            "app_title = \"Octoroute\"",
            r#"app_title = "Octoroute\nInjected""#,
            "upstreams.openrouter.app_title",
        ),
    ] {
        let input = valid_config().replace(needle, replacement);
        let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
            .expect_err("control characters must not reach HTTP headers");
        assert!(matches!(
            error,
            GatewayConfigError::Invalid {
                field: ref actual,
                ..
            } if actual == expected_field
        ));
    }
}

#[test]
fn local_first_byte_timeout_is_optional_but_must_be_positive() {
    let configured = valid_config().replace(
        "max_in_flight = 1",
        "max_in_flight = 1\nfirst_byte_timeout_ms = 45000",
    );
    let config = GatewayConfig::from_toml(&configured, &TestEnvironment::gateway())
        .expect("positive first-byte timeout");
    assert_eq!(config.local().first_byte_timeout_ms(), Some(45_000));

    let invalid = valid_config().replace(
        "max_in_flight = 1",
        "max_in_flight = 1\nfirst_byte_timeout_ms = 0",
    );
    let error = GatewayConfig::from_toml(&invalid, &TestEnvironment::gateway())
        .expect_err("zero first-byte timeout must fail startup");
    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "upstreams.local.first_byte_timeout_ms"
    ));
}

#[test]
fn local_probe_timing_must_be_positive() {
    for field in ["health_cache_ttl_ms", "probe_timeout_ms"] {
        let input = valid_config().replace(
            "max_in_flight = 1",
            &format!("max_in_flight = 1\n{field} = 0"),
        );
        let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
            .expect_err("zero probe timing must fail startup");

        assert!(matches!(
            error,
            GatewayConfigError::Invalid {
                field: ref actual,
                ..
            } if actual == &format!("upstreams.local.{field}")
        ));
    }
}

#[test]
fn cloud_probe_timing_must_be_positive() {
    for field in ["health_cache_ttl_ms", "probe_timeout_ms"] {
        let input = valid_config().replace(
            "app_title = \"Octoroute\"",
            &format!("app_title = \"Octoroute\"\n{field} = 0"),
        );
        let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
            .expect_err("zero cloud probe timing must fail startup");
        assert!(matches!(
            error,
            GatewayConfigError::Invalid {
                field: ref actual,
                ..
            } if actual == &format!("upstreams.openrouter.{field}")
        ));
    }
}

#[test]
fn local_output_reservation_must_leave_usable_context() {
    let input = valid_config().replace(
        "context_safety_tokens = 1024",
        "context_safety_tokens = 1024\ndefault_max_output_tokens = 65000",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("output reservation must leave room for input");

    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "upstreams.local.default_max_output_tokens"
    ));
}

#[test]
fn v1_config_returns_actionable_migration_error() {
    let error = GatewayConfig::from_toml(
        r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "legacy"
base_url = "http://localhost:8080/v1"
"#,
        &TestEnvironment::gateway(),
    )
    .expect_err("v1 configuration must be rejected");

    assert!(matches!(error, GatewayConfigError::MigrationRequired));
    assert!(error.to_string().contains("config_version = 2"));
}

#[test]
fn unsupported_config_version_is_rejected() {
    let input = valid_config().replacen("config_version = 2", "config_version = 3", 1);
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("future config version must be rejected");

    assert!(matches!(
        error,
        GatewayConfigError::UnsupportedVersion { version: 3 }
    ));
}

#[test]
fn missing_openrouter_secret_is_rejected_by_environment_name() {
    let environment = TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-secret");
    let error = GatewayConfig::from_toml(valid_config(), &environment)
        .expect_err("missing OpenRouter key must fail startup");

    assert!(matches!(
        error,
        GatewayConfigError::MissingEnvironmentVariable {
            ref name,
            ref field
        } if name == "OPENROUTER_API_KEY" && field == "upstreams.openrouter.api_key_env"
    ));
    assert!(!error.to_string().contains("inbound-secret"));
}

#[test]
fn secret_values_must_be_valid_bearer_credentials_without_echoing_them() {
    for (name, field) in [
        ("OCTOROUTE_API_KEY", "server.api_key_env"),
        ("OPENROUTER_API_KEY", "upstreams.openrouter.api_key_env"),
    ] {
        let environment = TestEnvironment::gateway().with(name, "secret\ninjected");
        let error = GatewayConfig::from_toml(valid_config(), &environment)
            .expect_err("control characters cannot form bearer credentials");
        let displayed = error.to_string();

        assert!(matches!(
            error,
            GatewayConfigError::Invalid {
                field: ref actual,
                ..
            } if actual == field
        ));
        assert!(!displayed.contains("secret"));
        assert!(!displayed.contains("injected"));
    }
}

#[test]
fn raw_secret_fields_are_rejected() {
    let input = valid_config().replace(
        "api_key_env = \"OPENROUTER_API_KEY\"",
        "api_key_env = \"OPENROUTER_API_KEY\"\napi_key = \"must-not-be-accepted\"",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("raw secrets must not be accepted in TOML");

    assert!(matches!(error, GatewayConfigError::Parse { .. }));
    assert!(!error.to_string().contains("must-not-be-accepted"));
}

#[test]
fn openrouter_requires_https() {
    let input = valid_config().replace(
        "https://openrouter.ai/api/v1",
        "http://openrouter.ai/api/v1",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("OpenRouter over HTTP must be rejected");

    assert!(matches!(
        error,
        GatewayConfigError::Invalid {
            ref field,
            ref message
        } if field == "upstreams.openrouter.base_url" && message.contains("HTTPS")
    ));
}

#[test]
fn unknown_local_capability_is_rejected() {
    let input = valid_config().replace(
        "[\"chat\", \"stream\"]",
        "[\"chat\", \"stream\", \"telepathy\"]",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("unknown capability must fail startup");

    assert!(matches!(error, GatewayConfigError::Parse { .. }));
    assert!(!error.to_string().contains("telepathy"));
}

#[test]
fn invalid_context_safety_margin_is_rejected() {
    let input = valid_config().replace(
        "context_safety_tokens = 1024",
        "context_safety_tokens = 65536",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("safety margin must leave usable context");

    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "upstreams.local.context_safety_tokens"
    ));
}

#[test]
fn openrouter_cost_quality_tradeoff_is_bounded() {
    let input = valid_config().replace("cost_quality_tradeoff = 9", "cost_quality_tradeoff = 11");
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("cost-quality tradeoff must be within OpenRouter bounds");

    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "upstreams.openrouter.cost_quality_tradeoff"
    ));
}

#[test]
fn upstream_paths_must_be_absolute_paths_not_urls() {
    let input = valid_config().replace(
        "health_path = \"/health\"",
        "health_path = \"https://attacker.invalid/health\"",
    );
    let error = GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect_err("absolute upstream URL must be rejected as a path");

    assert!(matches!(
        error,
        GatewayConfigError::Invalid { ref field, .. }
            if field == "upstreams.local.health_path"
    ));
}
