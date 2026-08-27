use super::*;
use std::collections::BTreeSet;

fn example() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../config.toml"))
        .expect("the repository v3 example must remain valid")
}

fn worker_requirements() -> LocalRequirements {
    LocalRequirements {
        input_tokens: 32_000,
        output_tokens: 16_000,
        capabilities: BTreeSet::from([
            LocalCapability::Chat,
            LocalCapability::Tools,
            LocalCapability::Reasoning,
        ]),
    }
}

fn worker_snapshots() -> Vec<MemberSnapshot> {
    (0..3)
        .map(|index| MemberSnapshot {
            pool: "workers".to_string(),
            member: format!("worker-{index}"),
            healthy: true,
            in_flight: 0,
        })
        .collect()
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
fn local_selection_rotates_equal_workers() {
    let config = example();
    let requirements = worker_requirements();
    let snapshots = worker_snapshots();

    let first = config
        .select_local_member("workers", &requirements, &snapshots, 0)
        .expect("worker zero");
    let second = config
        .select_local_member("workers", &requirements, &snapshots, first.next_cursor)
        .expect("worker one");
    let third = config
        .select_local_member("workers", &requirements, &snapshots, second.next_cursor)
        .expect("worker two");

    assert_eq!(first.member, "worker-0");
    assert_eq!(second.member, "worker-1");
    assert_eq!(third.member, "worker-2");
}

#[test]
fn selection_prefers_lower_live_load() {
    let config = example();
    let requirements = worker_requirements();
    let mut snapshots = worker_snapshots();
    snapshots[0].in_flight = 1;
    snapshots[1].in_flight = 0;
    snapshots[2].in_flight = 1;

    let selection = config
        .select_local_member("workers", &requirements, &snapshots, 0)
        .expect("the idle worker");
    assert_eq!(selection.member, "worker-1");
}

#[test]
fn selection_distinguishes_busy_from_unhealthy() {
    let config = example();
    let requirements = worker_requirements();
    let busy = worker_snapshots()
        .into_iter()
        .map(|mut snapshot| {
            snapshot.in_flight = 1;
            snapshot
        })
        .collect::<Vec<_>>();
    assert_eq!(
        config
            .select_local_member("workers", &requirements, &busy, 0)
            .expect_err("every worker is occupied"),
        PoolSelectionError::Busy
    );

    let unhealthy = worker_snapshots()
        .into_iter()
        .map(|mut snapshot| {
            snapshot.healthy = false;
            snapshot
        })
        .collect::<Vec<_>>();
    assert_eq!(
        config
            .select_local_member("workers", &requirements, &unhealthy, 0)
            .expect_err("every worker is unhealthy"),
        PoolSelectionError::Unhealthy
    );
}

#[test]
fn exact_context_budget_is_enforced() {
    let config = example();
    let mut requirements = worker_requirements();
    requirements.input_tokens = 120_000;
    requirements.output_tokens = 16_000;

    assert_eq!(
        config
            .select_local_member("workers", &requirements, &worker_snapshots(), 0)
            .expect_err("the budget exceeds 128K"),
        PoolSelectionError::ContextOverflow
    );
}

#[test]
fn local_only_narrows_auto_route_before_cloud_disclosure() {
    let config = example();
    let plan = config
        .route_plan("auto", true)
        .expect("the auto route has local targets");
    assert!(plan.local_only);
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
    let input = include_str!("../../../config.toml").replacen(
        "default_reasoning_effort = \"medium\"",
        "default_reasoning_effort = \"low\"",
        1,
    );
    let config = FabricConfig::from_toml(&input).expect("low must parse");
    assert_eq!(
        config.local_pools["workers"].default_reasoning_effort,
        ReasoningEffort::Low
    );
}

#[test]
fn provider_presets_cover_the_initial_cloud_options() {
    for key in ["openrouter", "zai", "kimi", "openai", "codex"] {
        assert!(provider_preset(key).is_some(), "missing preset {key}");
    }
    assert_eq!(
        provider_preset("kimi").and_then(|preset| preset.max_tokens),
        Some(200_000)
    );
    assert_eq!(
        provider_preset("codex").map(|preset| preset.kind),
        Some(ProviderKind::CodexCli)
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
    let input = include_str!("../../../config.toml").replace(
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
    let input = include_str!("../../../config.toml").replace(
        "api_key_env = \"KIMI_API_KEY\"",
        "api_key_env = \"KIMI_API_KEY\"\napi_key_command = [\"secret-tool\", \"lookup\", \"kimi\"]",
    );
    let error = FabricConfig::from_toml(&input).expect_err("two credential sources are invalid");
    assert!(error.to_string().contains("exactly one"));
}
