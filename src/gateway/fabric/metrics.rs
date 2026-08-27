//! Bounded in-memory metrics for the v3 provider runtime.

use super::{FabricConfig, FallbackTrigger, ProviderAdmissionState};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, MutexGuard, PoisonError},
};

const ADMISSION_STATES: &[&str] = &[
    "admitted",
    "disabled",
    "incompatible",
    "busy",
    "unavailable",
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
];
const PROBE_STATES: &[&str] = &["ready", "disabled", "incompatible", "busy", "unavailable"];

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
}

/// Fixed-label metrics keyed only by validated provider names and closed enums.
pub(super) struct FabricMetrics {
    providers: BTreeSet<String>,
    counters: Mutex<MetricCounters>,
}

impl FabricMetrics {
    pub(super) fn new(config: &FabricConfig) -> Self {
        Self {
            providers: config.providers.keys().cloned().collect(),
            counters: Mutex::new(MetricCounters::default()),
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
        self.increment(provider, fallback_trigger(trigger), |counters| {
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
        let mut output = String::from(
            "# HELP octoroute_fabric_runtime_info V3 inference-fabric runtime information.\n\
             # TYPE octoroute_fabric_runtime_info gauge\n\
             octoroute_fabric_runtime_info{config_version=\"3\",provider_runtime=\"complete\"} 1\n\
             # HELP octoroute_fabric_pool_enabled Whether a configured local pool is enabled.\n\
             # TYPE octoroute_fabric_pool_enabled gauge\n",
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
            "octoroute_fabric_provider_admissions_total",
            "Provider admission decisions.",
            "state",
            ADMISSION_STATES,
            &self.providers,
            &counters.admissions,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_responses_total",
            "Provider response outcomes before client commitment.",
            "outcome",
            RESPONSE_STATES,
            &self.providers,
            &counters.responses,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_fallbacks_total",
            "Provider fallbacks by closed policy trigger.",
            "trigger",
            FALLBACK_STATES,
            &self.providers,
            &counters.fallbacks,
        );
        render_family(
            &mut output,
            "octoroute_fabric_provider_probes_total",
            "Bounded provider readiness probe outcomes.",
            "state",
            PROBE_STATES,
            &self.providers,
            &counters.probes,
        );
        output
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

fn render_family(
    output: &mut String,
    metric: &str,
    help: &str,
    value_label: &str,
    values: &[&'static str],
    providers: &BTreeSet<String>,
    counters: &BTreeMap<(String, &'static str), u64>,
) {
    output.push_str(&format!(
        "# HELP {metric} {help}\n# TYPE {metric} counter\n"
    ));
    for provider in providers {
        for value in values {
            let count = counters
                .get(&(provider.clone(), *value))
                .copied()
                .unwrap_or_default();
            output.push_str(&format!(
                "{metric}{{provider=\"{provider}\",{value_label}=\"{value}\"}} {count}\n"
            ));
        }
    }
}

const fn provider_state(state: ProviderAdmissionState) -> &'static str {
    match state {
        ProviderAdmissionState::Ready => "ready",
        ProviderAdmissionState::Disabled => "disabled",
        ProviderAdmissionState::Incompatible => "incompatible",
        ProviderAdmissionState::Busy => "busy",
        ProviderAdmissionState::Unavailable => "unavailable",
    }
}

const fn fallback_trigger(trigger: FallbackTrigger) -> &'static str {
    match trigger {
        FallbackTrigger::Busy => "busy",
        FallbackTrigger::Unhealthy => "unhealthy",
        FallbackTrigger::ContextOverflow => "context_overflow",
        FallbackTrigger::Incompatible => "incompatible",
        FallbackTrigger::RateLimited => "rate_limited",
        FallbackTrigger::PrecommitFailure => "precommit_failure",
    }
}
