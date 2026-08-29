//! Bounded in-memory metrics for the v3 provider runtime.

use super::{
    FabricConfig, FallbackTrigger, PoolAdmissionState, ProviderAdmissionState, unknown_types,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

/// Completeness marker for the v3 provider runtime.
///
/// One definition, because it is reported twice: as a label on
/// `octoroute_fabric_runtime_info` and as a field in the readiness payload. Two
/// literals would let the two surfaces disagree about the same fact.
pub(super) const PROVIDER_RUNTIME: &str = "complete";

const ADMISSION_STATES: &[&str] = &[
    "admitted",
    "disabled",
    "incompatible",
    "busy",
    "unavailable",
    "unauthenticated",
];
const RESPONSE_STATES: &[&str] = &[
    "success",
    "rate_limited",
    "client_error",
    "server_error",
    "transport_error",
];
const FALLBACK_STATES: &[&str] = &[
    "busy",
    "unhealthy",
    "context_overflow",
    "incompatible",
    "rate_limited",
    "precommit_failure",
    "unauthenticated",
];
const PROBE_STATES: &[&str] = &[
    "ready",
    "disabled",
    "incompatible",
    "busy",
    "unavailable",
    "unauthenticated",
];
const POOL_ADMISSION_STATES: &[&str] = &[
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
const ROUTING_BUCKETS_SECONDS: &[f64] = &[
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

#[derive(Default)]
struct MetricCounters {
    admissions: BTreeMap<(String, &'static str), u64>,
    responses: BTreeMap<(String, &'static str), u64>,
    fallbacks: BTreeMap<(String, &'static str), u64>,
    probes: BTreeMap<(String, &'static str), u64>,
    pool_admissions: BTreeMap<(String, &'static str), u64>,
    pool_fallbacks: BTreeMap<(String, &'static str), u64>,
    routing_buckets: Vec<u64>,
    routing_count: u64,
    routing_sum_seconds: f64,
}

/// Fixed-label metrics keyed only by validated pool and provider names and
/// closed enums.
pub(super) struct FabricMetrics {
    providers: BTreeSet<String>,
    pools: BTreeSet<String>,
    counters: Mutex<MetricCounters>,
}

impl FabricMetrics {
    pub(super) fn new(config: &FabricConfig) -> Self {
        Self {
            providers: config.providers.keys().cloned().collect(),
            pools: config.local_pools.keys().cloned().collect(),
            counters: Mutex::new(MetricCounters {
                routing_buckets: vec![0; ROUTING_BUCKETS_SECONDS.len()],
                ..MetricCounters::default()
            }),
        }
    }

    pub(super) fn record_pool_admitted(&self, pool: &str) {
        self.increment_pool(pool, "admitted", |counters| &mut counters.pool_admissions);
    }

    pub(super) fn record_pool_rejected(&self, pool: &str, state: PoolAdmissionState) {
        self.increment_pool(pool, pool_state_label(state), |counters| {
            &mut counters.pool_admissions
        });
    }

    /// Record a local pool spilling forward to the next route step.
    pub(super) fn record_pool_fallback(&self, pool: &str, trigger: FallbackTrigger) {
        self.increment_pool(pool, fallback_trigger_label(trigger), |counters| {
            &mut counters.pool_fallbacks
        });
    }

    /// Record the admission time for one route step, excluding its upstream call.
    pub(super) fn record_routing_latency(&self, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        let mut counters = self.lock();
        counters.routing_count = counters.routing_count.saturating_add(1);
        counters.routing_sum_seconds += seconds;
        for (index, bound) in ROUTING_BUCKETS_SECONDS.iter().enumerate() {
            if seconds <= *bound {
                counters.routing_buckets[index] = counters.routing_buckets[index].saturating_add(1);
            }
        }
    }

    pub(super) fn record_admitted(&self, provider: &str) {
        self.increment(provider, "admitted", |counters| &mut counters.admissions);
    }

    pub(super) fn record_rejected(&self, provider: &str, state: ProviderAdmissionState) {
        self.increment(provider, provider_state(state), |counters| {
            &mut counters.admissions
        });
    }

    pub(super) fn record_response(&self, provider: &str, outcome: ProviderResponseOutcome) {
        self.increment(provider, outcome.as_str(), |counters| {
            &mut counters.responses
        });
    }

    pub(super) fn record_fallback(&self, provider: &str, trigger: FallbackTrigger) {
        self.increment(provider, fallback_trigger_label(trigger), |counters| {
            &mut counters.fallbacks
        });
    }

    pub(super) fn record_probe(&self, provider: &str, state: ProviderAdmissionState) {
        self.increment(provider, provider_state(state), |counters| {
            &mut counters.probes
        });
    }

    pub(super) fn render(&self, config: &FabricConfig) -> String {
        let counters = self.lock();
        let mut output = format!(
            "# HELP octoroute_fabric_runtime_info V3 inference-fabric runtime information.\n\
             # TYPE octoroute_fabric_runtime_info gauge\n\
             octoroute_fabric_runtime_info{{config_version=\"3\",provider_runtime=\"{PROVIDER_RUNTIME}\"}} 1\n\
             # HELP octoroute_fabric_pool_enabled Whether a configured local pool is enabled.\n\
             # TYPE octoroute_fabric_pool_enabled gauge\n"
        );
        for (name, pool) in &config.local_pools {
            let enabled = u8::from(pool.enabled);
            output.push_str(&format!(
                "octoroute_fabric_pool_enabled{{pool=\"{name}\"}} {enabled}\n"
            ));
        }
        output.push_str(
            "# HELP octoroute_fabric_provider_enabled Whether a configured provider is enabled.\n\
             # TYPE octoroute_fabric_provider_enabled gauge\n",
        );
        for (name, provider) in &config.providers {
            let enabled = u8::from(provider.enabled);
            output.push_str(&format!(
                "octoroute_fabric_provider_enabled{{provider=\"{name}\"}} {enabled}\n"
            ));
        }
        render_family(
            &mut output,
            "octoroute_fabric_pool_admissions_total",
            "Local pool admission decisions.",
            "pool",
            "state",
            POOL_ADMISSION_STATES,
            &self.pools,
            &counters.pool_admissions,
        );
        render_family(
            &mut output,
            "octoroute_fabric_pool_fallbacks_total",
            "Local pool spillovers to the next route step by closed policy trigger.",
            "pool",
            "trigger",
            FALLBACK_STATES,
            &self.pools,
            &counters.pool_fallbacks,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_admissions_total",
            "Provider admission decisions.",
            "provider",
            "state",
            ADMISSION_STATES,
            &self.providers,
            &counters.admissions,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_responses_total",
            "Provider response outcomes before client commitment.",
            "provider",
            "outcome",
            RESPONSE_STATES,
            &self.providers,
            &counters.responses,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_fallbacks_total",
            "Provider fallbacks by closed policy trigger.",
            "provider",
            "trigger",
            FALLBACK_STATES,
            &self.providers,
            &counters.fallbacks,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_probes_total",
            "Bounded provider readiness probe outcomes.",
            "provider",
            "state",
            PROBE_STATES,
            &self.providers,
            &counters.probes,
        );
        render_routing_histogram(&mut output, &counters);
        render_unknown_types(&mut output);
        output
    }

    fn increment_pool(
        &self,
        pool: &str,
        value: &'static str,
        family: impl FnOnce(&mut MetricCounters) -> &mut BTreeMap<(String, &'static str), u64>,
    ) {
        if !self.pools.contains(pool) {
            return;
        }
        let mut counters = self.lock();
        let counter = family(&mut counters)
            .entry((pool.to_string(), value))
            .or_default();
        *counter = counter.saturating_add(1);
    }

    fn increment(
        &self,
        provider: &str,
        value: &'static str,
        family: impl FnOnce(&mut MetricCounters) -> &mut BTreeMap<(String, &'static str), u64>,
    ) {
        if !self.providers.contains(provider) {
            return;
        }
        let mut counters = self.lock();
        let counter = family(&mut counters)
            .entry((provider.to_string(), value))
            .or_default();
        *counter = counter.saturating_add(1);
    }

    fn lock(&self) -> MutexGuard<'_, MetricCounters> {
        self.counters.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one closed-label Prometheus family is described by exactly these fields"
)]
fn render_family(
    output: &mut String,
    metric: &str,
    help: &str,
    name_label: &str,
    value_label: &str,
    values: &[&'static str],
    names: &BTreeSet<String>,
    counters: &BTreeMap<(String, &'static str), u64>,
) {
    output.push_str(&format!(
        "# HELP {metric} {help}\n# TYPE {metric} counter\n"
    ));
    for name in names {
        for value in values {
            let count = counters
                .get(&(name.clone(), *value))
                .copied()
                .unwrap_or_default();
            output.push_str(&format!(
                "{metric}{{{name_label}=\"{name}\",{value_label}=\"{value}\"}} {count}\n"
            ));
        }
    }
}

/// Render the per-adapter skipped-unknown-variant counter.
fn render_unknown_types(output: &mut String) {
    const METRIC: &str = "octoroute_fabric_unknown_upstream_types_total";
    output.push_str(&format!(
        "# HELP {METRIC} Upstream content blocks, events, and deltas skipped as unrecognized.\n\
         # TYPE {METRIC} counter\n"
    ));
    for adapter in unknown_types::Adapter::ALL {
        output.push_str(&format!(
            "{METRIC}{{adapter=\"{}\"}} {}\n",
            adapter.as_str(),
            unknown_types::count(adapter)
        ));
    }
}

/// Render the routing-latency histogram in Prometheus text format.
fn render_routing_histogram(output: &mut String, counters: &MetricCounters) {
    const METRIC: &str = "octoroute_fabric_routing_duration_seconds";
    output.push_str(&format!(
        "# HELP {METRIC} Admission time for one route step, excluding its upstream call.\n\
         # TYPE {METRIC} histogram\n"
    ));
    for (index, bound) in ROUTING_BUCKETS_SECONDS.iter().enumerate() {
        let count = counters.routing_buckets.get(index).copied().unwrap_or(0);
        output.push_str(&format!("{METRIC}_bucket{{le=\"{bound}\"}} {count}\n"));
    }
    output.push_str(&format!(
        "{METRIC}_bucket{{le=\"+Inf\"}} {count}\n{METRIC}_sum {sum}\n{METRIC}_count {count}\n",
        count = counters.routing_count,
        sum = counters.routing_sum_seconds
    ));
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
        FabricConfig::from_toml(include_str!("../../../config.toml")).expect("repository config")
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

    /// Every adapter emits a series even at zero. An omitted series is
    /// indistinguishable on a dashboard from a condition that never occurs, so
    /// the exposition iterates `Adapter::ALL` rather than only what has fired.
    #[test]
    fn unknown_upstream_types_emits_a_series_per_adapter() {
        let mut output = String::new();
        render_unknown_types(&mut output);
        assert!(output.contains("# TYPE octoroute_fabric_unknown_upstream_types_total counter"));
        for adapter in unknown_types::Adapter::ALL {
            assert!(
                output.contains(&format!(
                    "octoroute_fabric_unknown_upstream_types_total{{adapter=\"{}\"}}",
                    adapter.as_str()
                )),
                "missing series for {}",
                adapter.as_str()
            );
        }
    }

    /// The counter is per adapter, not shared.
    ///
    /// Both counters are process-global statics, and the adapter tests in this
    /// same binary record into them from parallel harness threads, so any
    /// assertion on an absolute value is a coin flip. Two race-proof claims
    /// replace it. Recording an adapter must advance that adapter's counter:
    /// concurrent recorders only ever add, so a strict increase cannot be
    /// fooled. And there must exist a window in which our own Anthropic record
    /// left the Codex counter untouched: concurrent recording is finite, so a
    /// quiet window arrives, while a shared counter can never produce one -
    /// this test's own record moves it every single time.
    #[test]
    fn each_adapter_counts_separately() {
        const ATTEMPTS: usize = 256;

        let mut isolated = false;
        for _ in 0..ATTEMPTS {
            let codex_before = unknown_types::count(unknown_types::Adapter::Codex);
            let anthropic_before = unknown_types::count(unknown_types::Adapter::Anthropic);
            unknown_types::record(unknown_types::Adapter::Anthropic);
            assert!(
                unknown_types::count(unknown_types::Adapter::Anthropic) > anthropic_before,
                "recording anthropic must advance the anthropic counter"
            );
            if unknown_types::count(unknown_types::Adapter::Codex) == codex_before {
                isolated = true;
                break;
            }
        }
        assert!(
            isolated,
            "recording anthropic moved the codex counter in every one of {ATTEMPTS} windows: \
             the adapters are sharing a counter"
        );
    }
}
