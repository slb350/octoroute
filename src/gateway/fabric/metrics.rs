//! Bounded in-memory metrics for the v3 provider runtime.

use super::{
    FabricConfig, FallbackTrigger, PoolAdmissionState, ProviderAdmissionState, unknown_types,
};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// Completeness marker for the v3 provider runtime.
///
/// One definition, because it is reported twice: as a label on
/// `octoroute_fabric_runtime_info` and as a field in the readiness payload. Two
/// literals would let the two surfaces disagree about the same fact.
pub(super) const PROVIDER_RUNTIME: &str = "complete";

const ADMISSION_STATES: [&str; 6] = [
    "admitted",
    "disabled",
    "incompatible",
    "busy",
    "unavailable",
    "unauthenticated",
];
const RESPONSE_STATES: [&str; 5] = [
    "success",
    "rate_limited",
    "client_error",
    "server_error",
    "transport_error",
];
const FALLBACK_STATES: [&str; 7] = [
    "busy",
    "unhealthy",
    "context_overflow",
    "incompatible",
    "rate_limited",
    "precommit_failure",
    "unauthenticated",
];
const PROBE_STATES: [&str; 6] = [
    "ready",
    "disabled",
    "incompatible",
    "busy",
    "unavailable",
    "unauthenticated",
];
const POOL_ADMISSION_STATES: [&str; 8] = [
    "admitted",
    "disabled",
    "unhealthy",
    "incompatible",
    "busy",
    "context_overflow",
    "token_count_unavailable",
    "unauthenticated",
];

/// Upper bounds, in seconds, of the routing-latency histogram.
///
/// One observation is the admission work for one route step, so the buckets span
/// a sub-millisecond local hit through a probe that reaches its deadline. Every
/// step is observed, admitted or refused, and a request that falls forward
/// contributes one observation per step it reaches, so `_count` is admission
/// attempts rather than requests or admissions.
const ROUTING_BUCKETS_SECONDS: [f64; 12] = [
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Debug, Clone, Copy)]
pub(super) enum ProviderResponseOutcome {
    Success,
    RateLimited,
    ClientError,
    ServerError,
    TransportError,
}

impl ProviderResponseOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::RateLimited => "rate_limited",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
            Self::TransportError => "transport_error",
        }
    }
}

struct PoolCounters {
    admissions: [AtomicU64; POOL_ADMISSION_STATES.len()],
    fallbacks: [AtomicU64; FALLBACK_STATES.len()],
}

impl Default for PoolCounters {
    fn default() -> Self {
        Self {
            admissions: std::array::from_fn(|_| AtomicU64::new(0)),
            fallbacks: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

struct ProviderCounters {
    admissions: [AtomicU64; ADMISSION_STATES.len()],
    responses: [AtomicU64; RESPONSE_STATES.len()],
    fallbacks: [AtomicU64; FALLBACK_STATES.len()],
    probes: [AtomicU64; PROBE_STATES.len()],
}

impl Default for ProviderCounters {
    fn default() -> Self {
        Self {
            admissions: std::array::from_fn(|_| AtomicU64::new(0)),
            responses: std::array::from_fn(|_| AtomicU64::new(0)),
            fallbacks: std::array::from_fn(|_| AtomicU64::new(0)),
            probes: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

/// Fixed-label, lock-free metrics keyed only by validated pool and provider
/// names and closed enums.
pub(super) struct FabricMetrics {
    pools: BTreeMap<String, PoolCounters>,
    providers: BTreeMap<String, ProviderCounters>,
    routing_buckets: [AtomicU64; ROUTING_BUCKETS_SECONDS.len()],
    routing_count: AtomicU64,
    routing_sum_nanos: AtomicU64,
}

impl FabricMetrics {
    pub(super) fn new(config: &FabricConfig) -> Self {
        Self {
            pools: config
                .local_pools
                .keys()
                .map(|name| (name.clone(), PoolCounters::default()))
                .collect(),
            providers: config
                .providers
                .keys()
                .map(|name| (name.clone(), ProviderCounters::default()))
                .collect(),
            routing_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            routing_count: AtomicU64::new(0),
            routing_sum_nanos: AtomicU64::new(0),
        }
    }

    pub(super) fn record_pool_admitted(&self, pool: &str) {
        increment_target(&self.pools, pool, "admitted", &POOL_ADMISSION_STATES, |c| {
            &c.admissions
        });
    }

    pub(super) fn record_pool_rejected(&self, pool: &str, state: PoolAdmissionState) {
        increment_target(
            &self.pools,
            pool,
            pool_state_label(state),
            &POOL_ADMISSION_STATES,
            |c| &c.admissions,
        );
    }

    /// Record a local pool spilling forward to the next route step.
    pub(super) fn record_pool_fallback(&self, pool: &str, trigger: FallbackTrigger) {
        increment_target(
            &self.pools,
            pool,
            fallback_trigger_label(trigger),
            &FALLBACK_STATES,
            |c| &c.fallbacks,
        );
    }

    /// Record the admission time for one route step, excluding its upstream call.
    pub(super) fn record_routing_latency(&self, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.routing_count.fetch_add(1, Ordering::Relaxed);
        self.routing_sum_nanos.fetch_add(nanos, Ordering::Relaxed);
        for (index, bound) in ROUTING_BUCKETS_SECONDS.iter().enumerate() {
            if seconds <= *bound {
                self.routing_buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn record_admitted(&self, provider: &str) {
        increment_target(
            &self.providers,
            provider,
            "admitted",
            &ADMISSION_STATES,
            |c| &c.admissions,
        );
    }

    pub(super) fn record_rejected(&self, provider: &str, state: ProviderAdmissionState) {
        increment_target(
            &self.providers,
            provider,
            provider_state(state),
            &ADMISSION_STATES,
            |c| &c.admissions,
        );
    }

    pub(super) fn record_response(&self, provider: &str, outcome: ProviderResponseOutcome) {
        increment_target(
            &self.providers,
            provider,
            outcome.as_str(),
            &RESPONSE_STATES,
            |c| &c.responses,
        );
    }

    pub(super) fn record_fallback(&self, provider: &str, trigger: FallbackTrigger) {
        increment_target(
            &self.providers,
            provider,
            fallback_trigger_label(trigger),
            &FALLBACK_STATES,
            |c| &c.fallbacks,
        );
    }

    pub(super) fn record_probe(&self, provider: &str, state: ProviderAdmissionState) {
        increment_target(
            &self.providers,
            provider,
            provider_state(state),
            &PROBE_STATES,
            |c| &c.probes,
        );
    }

    pub(super) fn render(&self, config: &FabricConfig) -> String {
        let mut output = String::with_capacity(8192);
        let _ = writeln!(
            output,
            "# HELP octoroute_fabric_runtime_info V3 inference-fabric runtime information.\n\
             # TYPE octoroute_fabric_runtime_info gauge\n\
             octoroute_fabric_runtime_info{{config_version=\"3\",provider_runtime=\"{PROVIDER_RUNTIME}\"}} 1\n\
             # HELP octoroute_fabric_pool_enabled Whether a configured local pool is enabled.\n\
             # TYPE octoroute_fabric_pool_enabled gauge"
        );
        for (name, pool) in &config.local_pools {
            let enabled = u8::from(pool.enabled);
            let _ = writeln!(
                output,
                "octoroute_fabric_pool_enabled{{pool=\"{name}\"}} {enabled}"
            );
        }
        output.push_str(
            "# HELP octoroute_fabric_provider_enabled Whether a configured provider is enabled.\n\
             # TYPE octoroute_fabric_provider_enabled gauge\n",
        );
        for (name, provider) in &config.providers {
            let enabled = u8::from(provider.enabled);
            let _ = writeln!(
                output,
                "octoroute_fabric_provider_enabled{{provider=\"{name}\"}} {enabled}"
            );
        }
        render_family(
            &mut output,
            "octoroute_fabric_pool_admissions_total",
            "Local pool admission decisions.",
            "pool",
            "state",
            &POOL_ADMISSION_STATES,
            &self.pools,
            |c| &c.admissions,
        );
        render_family(
            &mut output,
            "octoroute_fabric_pool_fallbacks_total",
            "Local pool spillovers to the next route step by closed policy trigger.",
            "pool",
            "trigger",
            &FALLBACK_STATES,
            &self.pools,
            |c| &c.fallbacks,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_admissions_total",
            "Provider admission decisions.",
            "provider",
            "state",
            &ADMISSION_STATES,
            &self.providers,
            |c| &c.admissions,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_responses_total",
            "Provider response outcomes before client commitment.",
            "provider",
            "outcome",
            &RESPONSE_STATES,
            &self.providers,
            |c| &c.responses,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_fallbacks_total",
            "Provider fallbacks by closed policy trigger.",
            "provider",
            "trigger",
            &FALLBACK_STATES,
            &self.providers,
            |c| &c.fallbacks,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_probes_total",
            "Bounded provider readiness probe outcomes.",
            "provider",
            "state",
            &PROBE_STATES,
            &self.providers,
            |c| &c.probes,
        );
        render_routing_histogram(&mut output, self);
        render_unknown_types(&mut output, &unknown_types::GLOBAL);
        output
    }
}

fn increment_target<C>(
    targets: &BTreeMap<String, C>,
    target: &str,
    value: &'static str,
    labels: &[&'static str],
    family: impl FnOnce(&C) -> &[AtomicU64],
) {
    if let Some(counters) = targets.get(target)
        && let Some(index) = labels.iter().position(|candidate| *candidate == value)
    {
        family(counters)[index].fetch_add(1, Ordering::Relaxed);
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one closed-label Prometheus family is described by exactly these fields"
)]
fn render_family<C>(
    output: &mut String,
    metric: &str,
    help: &str,
    name_label: &str,
    value_label: &str,
    values: &[&'static str],
    targets: &BTreeMap<String, C>,
    family: impl Fn(&C) -> &[AtomicU64],
) {
    let _ = writeln!(output, "# HELP {metric} {help}\n# TYPE {metric} counter");
    for (name, counters) in targets {
        let series = family(counters);
        for (value, counter) in values.iter().zip(series) {
            let count = counter.load(Ordering::Relaxed);
            let _ = writeln!(
                output,
                "{metric}{{{name_label}=\"{name}\",{value_label}=\"{value}\"}} {count}"
            );
        }
    }
}

/// Render the per-adapter skipped-unknown-variant counter.
fn render_unknown_types(output: &mut String, counters: &unknown_types::Counters) {
    const METRIC: &str = "octoroute_fabric_unknown_upstream_types_total";
    let _ = writeln!(
        output,
        "# HELP {METRIC} Upstream content blocks, events, and deltas skipped as unrecognized.\n\
         # TYPE {METRIC} counter"
    );
    for adapter in unknown_types::Adapter::ALL {
        let _ = writeln!(
            output,
            "{METRIC}{{adapter=\"{}\"}} {}",
            adapter.as_str(),
            counters.count(adapter)
        );
    }
}

/// Render the routing-latency histogram in Prometheus text format.
fn render_routing_histogram(output: &mut String, metrics: &FabricMetrics) {
    const METRIC: &str = "octoroute_fabric_routing_duration_seconds";
    let _ = writeln!(
        output,
        "# HELP {METRIC} Admission time for one route step, excluding its upstream call.\n\
         # TYPE {METRIC} histogram"
    );
    for (bound, counter) in ROUTING_BUCKETS_SECONDS.iter().zip(&metrics.routing_buckets) {
        let count = counter.load(Ordering::Relaxed);
        let _ = writeln!(output, "{METRIC}_bucket{{le=\"{bound}\"}} {count}");
    }
    let count = metrics.routing_count.load(Ordering::Relaxed);
    let sum = metrics.routing_sum_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
    let _ = writeln!(
        output,
        "{METRIC}_bucket{{le=\"+Inf\"}} {count}\n{METRIC}_sum {sum}\n{METRIC}_count {count}"
    );
}

pub(super) const fn pool_state_label(state: PoolAdmissionState) -> &'static str {
    match state {
        PoolAdmissionState::Ready => "admitted",
        PoolAdmissionState::Disabled => "disabled",
        PoolAdmissionState::Unhealthy => "unhealthy",
        PoolAdmissionState::Incompatible => "incompatible",
        PoolAdmissionState::Busy => "busy",
        PoolAdmissionState::ContextOverflow => "context_overflow",
        PoolAdmissionState::TokenCountUnavailable => "token_count_unavailable",
        PoolAdmissionState::Unauthenticated => "unauthenticated",
    }
}

pub(super) const fn provider_state(state: ProviderAdmissionState) -> &'static str {
    match state {
        ProviderAdmissionState::Ready => "ready",
        ProviderAdmissionState::Disabled => "disabled",
        ProviderAdmissionState::Incompatible => "incompatible",
        ProviderAdmissionState::Busy => "busy",
        ProviderAdmissionState::Unavailable => "unavailable",
        ProviderAdmissionState::Unauthenticated => "unauthenticated",
    }
}

pub(super) const fn fallback_trigger_label(trigger: FallbackTrigger) -> &'static str {
    match trigger {
        FallbackTrigger::Busy => "busy",
        FallbackTrigger::Unhealthy => "unhealthy",
        FallbackTrigger::ContextOverflow => "context_overflow",
        FallbackTrigger::Incompatible => "incompatible",
        FallbackTrigger::RateLimited => "rate_limited",
        FallbackTrigger::PrecommitFailure => "precommit_failure",
        FallbackTrigger::Unauthenticated => "unauthenticated",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FabricConfig {
        crate::gateway::fabric::test_support::example_config()
    }

    #[test]
    fn admission_and_latency_recorders_update_their_exact_series() {
        let config = config();
        let metrics = FabricMetrics::new(&config);

        metrics.record_pool_admitted("workers");
        metrics.record_pool_rejected("workers", PoolAdmissionState::Busy);
        metrics.record_admitted("zai");
        metrics.record_routing_latency(Duration::from_millis(250));

        let rendered = metrics.render(&config);
        for expected in [
            "octoroute_fabric_pool_admissions_total{pool=\"workers\",state=\"admitted\"} 1",
            "octoroute_fabric_pool_admissions_total{pool=\"workers\",state=\"busy\"} 1",
            "octoroute_fabric_provider_admissions_total{provider=\"zai\",state=\"admitted\"} 1",
            "octoroute_fabric_routing_duration_seconds_bucket{le=\"0.1\"} 0",
            "octoroute_fabric_routing_duration_seconds_bucket{le=\"0.25\"} 1",
            "octoroute_fabric_routing_duration_seconds_sum 0.25",
            "octoroute_fabric_routing_duration_seconds_count 1",
        ] {
            assert!(
                rendered.contains(expected),
                "missing `{expected}` in:\n{rendered}"
            );
        }
    }

    #[test]
    fn every_pool_admission_state_has_the_exact_metric_label() {
        for (state, expected) in [
            (PoolAdmissionState::Ready, "admitted"),
            (PoolAdmissionState::Disabled, "disabled"),
            (PoolAdmissionState::Unhealthy, "unhealthy"),
            (PoolAdmissionState::Incompatible, "incompatible"),
            (PoolAdmissionState::Busy, "busy"),
            (PoolAdmissionState::ContextOverflow, "context_overflow"),
            (
                PoolAdmissionState::TokenCountUnavailable,
                "token_count_unavailable",
            ),
            (PoolAdmissionState::Unauthenticated, "unauthenticated"),
        ] {
            assert_eq!(pool_state_label(state), expected, "{state:?}");
        }
    }

    /// Whether `text` names `identifier` whole, not only as part of a longer
    /// name such as a histogram's `_bucket` series.
    fn names_identifier(text: &str, identifier: &str) -> bool {
        let bytes = text.as_bytes();
        let is_identifier_byte = |byte: &u8| byte.is_ascii_alphanumeric() || *byte == b'_';
        text.match_indices(identifier).any(|(start, _)| {
            let before = start.checked_sub(1).map(|index| &bytes[index]);
            let after = bytes.get(start + identifier.len());
            !before.is_some_and(is_identifier_byte) && !after.is_some_and(is_identifier_byte)
        })
    }

    #[test]
    fn names_identifier_requires_a_whole_name() {
        assert!(names_identifier("`family_total` counts", "family_total"));
        assert!(names_identifier("family_total", "family_total"));
        assert!(!names_identifier("family_total_bucket", "family_total"));
        assert!(!names_identifier("pre_family_total", "family_total"));
        assert!(!names_identifier("unrelated", "family_total"));
    }

    /// Every authoritative reference - the operator docs and the AGENTS.md
    /// instruction file - names every family the registry renders.
    ///
    /// The API reference once listed only the four provider counters while the
    /// runtime also rendered pool admissions, pool fallbacks, the routing
    /// histogram, and the unknown-type counter, so an operator building
    /// dashboards from it never learned that the spill-to-cloud signal existed.
    /// AGENTS.md's `Metric families` inventory held the same kind of drift,
    /// naming four families directly and four more only inside a compressed
    /// brace form while omitting the runtime-info and enabled gauges, so an
    /// agent trusting the instructions could not reason about the full
    /// cardinality the exposition pins at startup. Families are read from the
    /// rendered `# TYPE` lines, so a new family fails here until it is
    /// documented.
    #[test]
    fn operator_references_name_every_rendered_metric_family() {
        let config = config();
        let rendered = FabricMetrics::new(&config).render(&config);
        let families: Vec<&str> = rendered
            .lines()
            .filter_map(|line| line.strip_prefix("# TYPE "))
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        assert!(
            families.contains(&"octoroute_fabric_routing_duration_seconds"),
            "family parsing missed the histogram in:\n{rendered}"
        );

        for document in [
            "AGENTS.md",
            "docs/api-reference.md",
            "docs/observability.md",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(document);
            let text = std::fs::read_to_string(&path).expect("readable operator reference");
            let missing: Vec<&str> = families
                .iter()
                .copied()
                .filter(|family| !names_identifier(&text, family))
                .collect();
            assert!(missing.is_empty(), "{document} does not name {missing:?}");
        }
    }

    /// Every adapter emits a series even at zero. An omitted series is
    /// indistinguishable on a dashboard from a condition that never occurs, so
    /// the exposition iterates `Adapter::ALL` rather than only what has fired.
    #[test]
    fn unknown_upstream_types_emits_a_series_per_adapter() {
        let counters = unknown_types::Counters::default();
        counters.record(unknown_types::Adapter::Codex);
        let mut output = String::new();
        render_unknown_types(&mut output, &counters);
        assert!(output.contains("# TYPE octoroute_fabric_unknown_upstream_types_total counter"));
        for (adapter, count) in [("anthropic", 0), ("codex", 1)] {
            assert!(
                output.contains(&format!(
                    "octoroute_fabric_unknown_upstream_types_total{{adapter=\"{adapter}\"}} {count}\n"
                )),
                "missing count for {adapter}"
            );
        }
    }
}
