use super::*;
use crate::gateway::fabric::{FabricRouteError, PrivacyDirective, RoutePrivacy};

const REPOSITORY_CONFIG: &str = include_str!("../../../../config.toml");
const LAPTOP_CONFIG: &str = include_str!("../../../../config.laptop.toml");

fn example() -> FabricConfig {
    FabricConfig::from_toml(REPOSITORY_CONFIG).expect("the repository v3 example must remain valid")
}

/// Derive a config fixture by replacing a unique anchor in the repository
/// example.
///
/// A plain `str::replace` whose anchor has moved silently becomes a no-op, and
/// the test then asserts against the unmodified example while still passing.
/// An anchor that matches more than once is the same hazard one step removed:
/// an unrelated edit repeating the line either retargets a first-match fixture
/// or widens a replace-all one, again without failing anything. Both are
/// asserted, so every fixture anchor here has to be unique.
fn config_with(anchor: &str, replacement: &str) -> String {
    assert_eq!(
        REPOSITORY_CONFIG.matches(anchor).count(),
        1,
        "fixture anchor must appear exactly once in config.toml: {anchor:?}"
    );
    REPOSITORY_CONFIG.replace(anchor, replacement)
}

#[test]
fn repository_example_parses() {
    let config = example();
    assert_eq!(config.default_model, "auto-route");
    assert_eq!(config.local_pools["workers"].members.len(), 3);
    assert!(matches!(
        config.providers["kimi"].runtime,
        ProviderRuntimeConfig::Http {
            protocol: ProviderProtocol::Anthropic,
            ..
        }
    ));
    assert_eq!(
        config.providers["codex"].runtime.kind(),
        ProviderKind::CodexCli
    );
    let ProviderRuntimeConfig::Http { endpoint, .. } = &config.providers["zai"].runtime else {
        panic!("z.ai is an HTTP provider");
    };
    assert_eq!(endpoint.path(), "/api/coding/paas/v4/");
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
    let input = config_with(
        "strategy = \"least_loaded\"\ndefault_reasoning_effort = \"medium\"",
        "strategy = \"least_loaded\"\ndefault_reasoning_effort = \"low\"",
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
    let ProviderRuntimeConfig::CodexCli { executable } = &config.providers["codex"].runtime else {
        panic!("codex is a CLI provider");
    };
    assert_eq!(executable, "codex");

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
    let timeout = config_with(
        "default_max_output_tokens = 16384\ntimeout_ms = 1800000",
        "default_max_output_tokens = 16384\ntimeout_ms = 3600001",
    );
    let error = FabricConfig::from_toml(&timeout).expect_err("local timeout exceeds one hour");
    assert!(error.to_string().contains("timeout_ms"));

    let concurrency = config_with(
        "api_key_env = \"KIMI_API_KEY\"\nmax_in_flight = 2",
        "api_key_env = \"KIMI_API_KEY\"\nmax_in_flight = 10001",
    );
    let error =
        FabricConfig::from_toml(&concurrency).expect_err("provider concurrency exceeds bound");
    assert!(error.to_string().contains("max_in_flight"));
}

#[test]
fn only_config_version_3_is_accepted() {
    let wrong = config_with("config_version = 3", "config_version = 2");
    assert!(matches!(
        FabricConfig::from_toml(&wrong).expect_err("version 2 must be rejected"),
        FabricConfigError::UnsupportedVersion(2)
    ));

    let missing = config_with("config_version = 3\n", "");
    assert!(matches!(
        FabricConfig::from_toml(&missing).expect_err("an absent version must be rejected"),
        FabricConfigError::Parse { .. }
    ));
}

/// An HTTP provider with no credential source is as invalid as one with two.
/// Only the two-source arm had a fixture, so the `has_env == has_command`
/// equality could be narrowed to a conjunction without failing anything.
#[test]
fn http_provider_requires_at_least_one_credential_source() {
    let input = config_with("api_key_env = \"KIMI_API_KEY\"\n", "");
    let error = FabricConfig::from_toml(&input).expect_err("no credential source is invalid");
    assert!(error.to_string().contains("exactly one"));
}

/// The laptop development profile ships in the repository and is loaded by
/// `--config config.laptop.toml`, so it has to satisfy the same validator.
#[test]
fn laptop_profile_parses_and_stays_local() {
    let config =
        FabricConfig::from_toml(LAPTOP_CONFIG).expect("the laptop profile must remain valid");
    assert_eq!(config.default_model, "auto-route");
    assert!(
        config.server.host.is_loopback(),
        "the laptop profile binds loopback only"
    );
    assert_eq!(config.routes["worker"].privacy, RoutePrivacy::LocalOnly);
    let plan = config
        .route_plan("worker", true)
        .expect("the worker route has local targets");
    assert!(
        plan.steps
            .iter()
            .all(|step| matches!(step, RouteTarget::LocalPool(_))),
        "the laptop worker route must not plan a provider step"
    );
}

/// A route step is unvalidated operator text. It is rejected by the parser,
/// which never interpolates it, so no validation error may echo it either - an
/// operator who pasted a credential into the wrong field would otherwise see it
/// on stderr and in the startup log.
#[test]
fn duplicate_step_errors_never_echo_the_raw_step() {
    let input = config_with(
        "steps = [\"provider:codex\", \"provider:openrouter\", \"provider:openai\"]",
        "steps = [\"provider:hunter2\\nleaked\", \"provider:hunter2\\nleaked\"]",
    );
    let error = FabricConfig::from_toml(&input).expect_err("an unparseable step is rejected");
    let rendered = error.to_string();
    assert!(!rendered.contains("hunter2"), "error echoed the raw step");
    assert!(!rendered.contains("leaked"), "error echoed the raw step");
    assert!(
        !rendered.contains('\n'),
        "error echoed a newline from config"
    );
}

/// Repeating a target that does not exist reports the missing reference, which
/// is the actionable fault, rather than the repetition of it.
#[test]
fn a_repeated_unknown_target_reports_the_unknown_reference() {
    let input = config_with(
        "steps = [\"provider:codex\", \"provider:openrouter\", \"provider:openai\"]",
        "steps = [\"provider:absent\", \"provider:absent\"]",
    );
    let error = FabricConfig::from_toml(&input).expect_err("an unknown provider is rejected");
    assert!(error.to_string().contains("unknown provider"));
}

/// Every name that keys a map is checked for collision on insert. A collision
/// that silently overwrote would drop a pool, provider, or route from a
/// validated configuration without any diagnostic.
#[test]
fn pool_provider_route_and_member_names_must_be_unique() {
    for (anchor, replacement, expected) in [
        (
            "name = \"supervisor-local\"",
            "name = \"workers\"",
            "duplicate pool",
        ),
        ("name = \"zai\"", "name = \"kimi\"", "duplicate provider"),
        (
            "model = \"local\"\nprivacy = \"local_only\"",
            "model = \"worker\"\nprivacy = \"local_only\"",
            "duplicate virtual model",
        ),
        (
            "name = \"worker-1\"",
            "name = \"worker-0\"",
            "duplicate member",
        ),
    ] {
        let input = config_with(anchor, replacement);
        let error =
            FabricConfig::from_toml(&input).expect_err(&format!("{expected} must be rejected"));
        assert!(
            error.to_string().contains(expected),
            "expected `{expected}`, got `{error}`"
        );
    }
}

/// A step naming a pool or provider that no longer exists is a configuration
/// error, never a route that silently loses a step at startup.
#[test]
fn route_steps_must_reference_configured_targets() {
    let unknown_pool = config_with("steps = [\"pool:workers\"]", "steps = [\"pool:absent\"]");
    let error = FabricConfig::from_toml(&unknown_pool).expect_err("an unknown pool is rejected");
    assert!(error.to_string().contains("unknown local pool"));

    let unknown_provider = config_with(
        "steps = [\"provider:codex\", \"provider:openrouter\", \"provider:openai\"]",
        "steps = [\"provider:absent\"]",
    );
    let error =
        FabricConfig::from_toml(&unknown_provider).expect_err("an unknown provider is rejected");
    assert!(error.to_string().contains("unknown provider"));
}

/// A route's declared privacy and its declared steps must agree at validation
/// time. Plan filtering enforces the promise at runtime, but a configuration
/// whose two halves disagree is an operator error that should never start.
#[test]
fn route_privacy_and_steps_must_agree() {
    let local_only_with_provider = config_with(
        "steps = [\"pool:workers\"]",
        "steps = [\"pool:workers\", \"provider:openrouter\"]",
    );
    let error = FabricConfig::from_toml(&local_only_with_provider)
        .expect_err("a local_only route cannot name a provider");
    assert!(
        error
            .to_string()
            .contains("local_only routes cannot reference providers")
    );

    let cloud_only_with_pool = config_with(
        "steps = [\"provider:codex\", \"provider:openrouter\", \"provider:openai\"]",
        "steps = [\"pool:workers\", \"provider:codex\"]",
    );
    let error = FabricConfig::from_toml(&cloud_only_with_pool)
        .expect_err("a cloud_only route cannot name a pool");
    assert!(
        error
            .to_string()
            .contains("cloud_only routes cannot reference local pools")
    );
}

/// The default fallback set has six triggers and deliberately excludes
/// `unauthenticated`: falling forward on a rejected credential turns an expired
/// key into silently redirected traffic and spend. Membership is pinned exactly
/// so neither an addition nor a removal can arrive unnoticed.
#[test]
fn omitted_fallback_on_defaults_to_six_triggers_without_unauthenticated() {
    let input = config_with(
        "steps = [\"pool:workers\"]\ndefault_reasoning_effort = \"medium\"\nfallback_on = [\"busy\", \"unhealthy\", \"context_overflow\", \"incompatible\", \"precommit_failure\"]",
        "steps = [\"pool:workers\"]\ndefault_reasoning_effort = \"medium\"",
    );
    let config = FabricConfig::from_toml(&input).expect("fallback_on is optional");
    let fallback_on = &config.routes["worker"].fallback_on;
    assert_eq!(
        *fallback_on,
        BTreeSet::from([
            FallbackTrigger::Busy,
            FallbackTrigger::Unhealthy,
            FallbackTrigger::ContextOverflow,
            FallbackTrigger::Incompatible,
            FallbackTrigger::RateLimited,
            FallbackTrigger::PrecommitFailure,
        ])
    );
    assert!(
        !fallback_on.contains(&FallbackTrigger::Unauthenticated),
        "`unauthenticated` must never be a default fallback trigger"
    );
}

/// `unauthenticated` is selectable, just never default.
#[test]
fn unauthenticated_is_an_accepted_configured_trigger() {
    let input = config_with(
        "steps = [\"pool:workers\"]\ndefault_reasoning_effort = \"medium\"\nfallback_on = [\"busy\", \"unhealthy\", \"context_overflow\", \"incompatible\", \"precommit_failure\"]",
        "steps = [\"pool:workers\"]\ndefault_reasoning_effort = \"medium\"\nfallback_on = [\"busy\", \"unauthenticated\"]",
    );
    let config = FabricConfig::from_toml(&input).expect("unauthenticated is a valid trigger");
    assert!(
        config.routes["worker"]
            .fallback_on
            .contains(&FallbackTrigger::Unauthenticated)
    );
}

/// The token-count deadline is bounded separately from the total deadline, and
/// a first-byte deadline must fit inside the total it subdivides.
#[test]
fn local_pool_deadlines_are_bounded_against_their_own_limits() {
    let token_count = config_with(
        "default_max_output_tokens = 16384\ntimeout_ms = 1800000",
        "default_max_output_tokens = 16384\ntimeout_ms = 1800000\ntoken_count_timeout_ms = 120001",
    );
    let error =
        FabricConfig::from_toml(&token_count).expect_err("the token-count deadline is bounded");
    assert!(error.to_string().contains("token_count_timeout_ms"));

    let first_byte = config_with(
        "default_max_output_tokens = 16384\ntimeout_ms = 1800000",
        "default_max_output_tokens = 16384\ntimeout_ms = 1800000\nfirst_byte_timeout_ms = 1800001",
    );
    let error = FabricConfig::from_toml(&first_byte)
        .expect_err("a first-byte deadline cannot exceed the total");
    assert!(error.to_string().contains("first_byte_timeout_ms"));
}
