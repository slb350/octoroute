use super::*;

const REPOSITORY_CONFIG: &str = include_str!("../../../config.toml");

fn example() -> FabricConfig {
    FabricConfig::from_toml(REPOSITORY_CONFIG).expect("the repository v3 example must remain valid")
}

/// Derive a config fixture by replacing an anchor in the repository example.
///
/// A plain `str::replace` whose anchor has moved silently becomes a no-op, and
/// the test then asserts against the unmodified example while still passing.
/// This fails instead, so reformatting `config.toml` cannot hollow out the
/// validation suite.
fn config_with(anchor: &str, replacement: &str) -> String {
    assert!(
        REPOSITORY_CONFIG.contains(anchor),
        "fixture anchor is no longer present in config.toml: {anchor:?}"
    );
    REPOSITORY_CONFIG.replace(anchor, replacement)
}

/// As [`config_with`], replacing only the first occurrence.
fn config_with_first(anchor: &str, replacement: &str) -> String {
    assert!(
        REPOSITORY_CONFIG.contains(anchor),
        "fixture anchor is no longer present in config.toml: {anchor:?}"
    );
    REPOSITORY_CONFIG.replacen(anchor, replacement, 1)
}

#[test]
fn repository_example_parses() {
    let config = example();
    assert_eq!(config.default_model, "auto-route");
    assert_eq!(config.local_pools["workers"].members.len(), 3);
    assert_eq!(
        config.providers["kimi"].protocol,
        Some(ProviderProtocol::Anthropic)
    );
    assert_eq!(config.providers["codex"].kind, ProviderKind::CodexCli);
    assert_eq!(
        config.providers["zai"]
            .endpoint
            .as_ref()
            .expect("z.ai endpoint")
            .path(),
        "/api/coding/paas/v4/"
    );
}

#[test]
fn local_only_narrows_auto_route_before_cloud_disclosure() {
    let config = example();
    let plan = config
        .route_plan("auto", true)
        .expect("the auto route has local targets");
    assert!(
        plan.steps
            .iter()
            .all(|step| matches!(step, RouteTarget::LocalPool(_)))
    );
    assert_eq!(
        plan.steps,
        vec![
            RouteTarget::LocalPool("workers".to_string()),
            RouteTarget::LocalPool("supervisor-local".to_string()),
        ]
    );
}

/// A route declaring `local_only` is filtered even without the header, so the
/// promise does not rest on config validation having refused provider steps.
#[test]
fn local_only_route_declaration_filters_providers_without_the_header() {
    let mut config = example();
    let route = config.routes.get_mut("auto-route").expect("auto route");
    route.privacy = RoutePrivacy::LocalOnly;
    route
        .steps
        .push(RouteTarget::Provider("openrouter".to_string()));

    let plan = config
        .route_plan("auto-route", false)
        .expect("local targets remain");
    assert!(
        plan.steps
            .iter()
            .all(|step| matches!(step, RouteTarget::LocalPool(_))),
        "a local-only route must not plan a provider step"
    );
}

/// `PrivacyDirective` is the boundary parser: anything but the one accepted
/// value is an error, never a silently cloud-eligible request.
#[test]
fn privacy_header_parsing_fails_closed() {
    use axum::http::{HeaderMap, HeaderValue};

    assert_eq!(
        PrivacyDirective::from_headers(&HeaderMap::new()).expect("absent header"),
        PrivacyDirective::None
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    assert_eq!(
        PrivacyDirective::from_headers(&headers).expect("accepted value"),
        PrivacyDirective::LocalOnly
    );

    for rejected in [
        "local_only",
        "Local-Only",
        "",
        "cloud-only",
        "local-only, local-only",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-octoroute-privacy",
            HeaderValue::from_str(rejected).expect("header value"),
        );
        PrivacyDirective::from_headers(&headers)
            .expect_err(&format!("{rejected:?} must not be accepted"));
    }

    // A repeated header is ambiguous and must not resolve to the weaker value.
    let mut headers = HeaderMap::new();
    headers.append(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    headers.append(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    PrivacyDirective::from_headers(&headers).expect_err("a repeated header must be rejected");
}

#[test]
fn cloud_only_route_rejects_local_only_header() {
    let config = example();
    assert_eq!(
        config
            .route_plan("cloud-sota", true)
            .expect_err("privacy is contradictory"),
        FabricRouteError::ContradictoryPrivacy
    );
}

#[test]
fn low_reasoning_is_a_supported_policy_value() {
    let input = config_with_first(
        "default_reasoning_effort = \"medium\"",
        "default_reasoning_effort = \"low\"",
    );
    let config = FabricConfig::from_toml(&input).expect("low must parse");
    assert_eq!(
        config.local_pools["workers"].default_reasoning_effort,
        ReasoningEffort::Low
    );
}

#[test]
fn malformed_v3_toml_does_not_echo_values() {
    let input = "config_version = 3\nsecret = \"do-not-echo\"\n[server\n";
    let error = FabricConfig::from_toml(input).expect_err("malformed TOML");
    assert!(!error.to_string().contains("do-not-echo"));
}

#[test]
fn route_rejects_returning_local_after_cloud() {
    let input = config_with(
        "steps = [\"pool:workers\", \"pool:supervisor-local\", \"provider:kimi\", \"provider:zai\", \"provider:openrouter\"]",
        "steps = [\"provider:kimi\", \"pool:workers\"]",
    );
    let error = FabricConfig::from_toml(&input).expect_err("cloud-to-local is invalid");
    assert!(
        error
            .to_string()
            .contains("local pools must precede providers")
    );
}

#[test]
fn http_provider_requires_exactly_one_credential_source() {
    let input = config_with(
        "api_key_env = \"KIMI_API_KEY\"",
        "api_key_env = \"KIMI_API_KEY\"\napi_key_command = [\"secret-tool\", \"lookup\", \"kimi\"]",
    );
    let error = FabricConfig::from_toml(&input).expect_err("two credential sources are invalid");
    assert!(error.to_string().contains("exactly one"));
}

#[test]
fn anthropic_provider_requires_a_bounded_default_max_tokens() {
    let input = config_with("max_tokens = 200000\n", "");
    let error = FabricConfig::from_toml(&input).expect_err("Anthropic max_tokens is required");
    assert!(error.to_string().contains("max_tokens"));
}

#[test]
fn codex_provider_accepts_only_an_executable_override() {
    let config = example();
    assert_eq!(
        config.providers["codex"].executable.as_deref(),
        Some("codex")
    );

    let input = config_with(
        "executable = \"codex\"",
        "executable = \"codex\"\napi_key_env = \"OPENAI_API_KEY\"",
    );
    let error = FabricConfig::from_toml(&input).expect_err("Codex credentials are forbidden");
    assert!(error.to_string().contains("codex_cli"));
}

#[test]
fn provider_readiness_windows_are_bounded() {
    let input = config_with(
        "timeout_ms = 1800000\nmax_tokens = 200000",
        "timeout_ms = 1800000\nreadiness_ttl_ms = 3600001\nmax_tokens = 200000",
    );
    let error = FabricConfig::from_toml(&input).expect_err("probe TTL exceeds its bound");
    assert!(error.to_string().contains("readiness_ttl_ms"));
}

#[test]
fn auto_is_reserved_for_the_default_route_alias() {
    let input = config_with(
        "model = \"auto-route\"\nprivacy = \"cloud_allowed\"",
        "model = \"auto\"\nprivacy = \"cloud_allowed\"",
    );
    let error = FabricConfig::from_toml(&input).expect_err("auto route must be rejected");
    assert!(error.to_string().contains("reserved"));
}

#[test]
fn route_targets_cannot_repeat() {
    let input = config_with(
        "steps = [\"provider:codex\", \"provider:openrouter\", \"provider:openai\"]",
        "steps = [\"provider:codex\", \"provider:codex\"]",
    );
    let error = FabricConfig::from_toml(&input).expect_err("duplicate target must be rejected");
    assert!(error.to_string().contains("duplicate target"));
}

#[test]
fn removed_provider_priority_is_rejected_instead_of_ignored() {
    let input = config_with(
        "name = \"kimi\"\nenabled = true",
        "name = \"kimi\"\nenabled = true\npriority = 10",
    );
    let error = FabricConfig::from_toml(&input).expect_err("provider priority is not a field");
    assert!(matches!(error, FabricConfigError::Parse { .. }));
}

#[test]
fn credential_command_argv_is_statically_bounded() {
    let oversized = "x".repeat(4097);
    let input = config_with(
        "api_key_env = \"KIMI_API_KEY\"",
        &format!("api_key_command = [\"{oversized}\"]"),
    );
    let error = FabricConfig::from_toml(&input).expect_err("oversized argv must be rejected");
    assert!(error.to_string().contains("4096"));
}

#[test]
fn upstream_deadlines_and_concurrency_are_statically_bounded() {
    let timeout = config_with_first("timeout_ms = 1800000", "timeout_ms = 3600001");
    let error = FabricConfig::from_toml(&timeout).expect_err("local timeout exceeds one hour");
    assert!(error.to_string().contains("timeout_ms"));

    let concurrency = config_with_first("max_in_flight = 2", "max_in_flight = 10001");
    let error =
        FabricConfig::from_toml(&concurrency).expect_err("provider concurrency exceeds bound");
    assert!(error.to_string().contains("max_in_flight"));
}
