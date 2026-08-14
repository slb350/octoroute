//! Deterministic model intent and local/cloud routing decisions.

use crate::gateway::{
    config::{GatewayConfig, RouteDefault},
    request::{GatewayRequest, RequestFeature},
};
use axum::http::HeaderMap;
use thiserror::Error;

const PRIVACY_HEADER: &str = "x-octoroute-privacy";

/// Explicit destination intent resolved from the OpenAI `model` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelIntent {
    /// Apply Octoroute's automatic policy.
    Auto,
    /// Use the configured local alias with no cloud fallback.
    Local,
    /// Use OpenRouter Auto.
    CloudAuto,
    /// Use an exact OpenRouter model slug.
    CloudModel(String),
}

impl ModelIntent {
    /// Resolve virtual names, the local alias, and provider-qualified slugs.
    pub fn resolve(
        requested: &str,
        local_model: &str,
        cloud_auto_model: &str,
    ) -> Result<Self, ModelIntentError> {
        if requested == "auto" {
            Ok(Self::Auto)
        } else if requested == "local" || requested == local_model {
            Ok(Self::Local)
        } else if requested == "cloud" || requested == cloud_auto_model {
            Ok(Self::CloudAuto)
        } else if requested.contains('/') {
            Ok(Self::CloudModel(requested.to_string()))
        } else {
            Err(ModelIntentError::UnknownModel(requested.to_string()))
        }
    }
}

/// Invalid client model identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelIntentError {
    /// An unqualified identifier matched neither a virtual model nor local alias.
    #[error("unknown model `{0}`; use auto, local, cloud, the local alias, or provider/model")]
    UnknownModel(String),
}

/// Optional request-level privacy constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyDirective {
    /// No additional privacy constraint.
    None,
    /// The request must never leave the local gateway.
    LocalOnly,
}

impl PrivacyDirective {
    /// Parse Octoroute's single-valued privacy header.
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, PrivacyDirectiveError> {
        let mut values = headers.get_all(PRIVACY_HEADER).iter();
        let Some(value) = values.next() else {
            return Ok(Self::None);
        };
        if values.next().is_some() {
            return Err(PrivacyDirectiveError::Invalid);
        }
        let value = value.to_str().map_err(|_| PrivacyDirectiveError::Invalid)?;
        if value == "local-only" {
            Ok(Self::LocalOnly)
        } else {
            Err(PrivacyDirectiveError::Invalid)
        }
    }
}

/// Invalid privacy headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PrivacyDirectiveError {
    /// Header was repeated, non-UTF-8, or used an unsupported value.
    #[error("invalid X-Octoroute-Privacy header; the only accepted value is `local-only`")]
    Invalid,
}

/// Current result of local health, context, and slot admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAdmissionState {
    /// Strix can accept the request now.
    Ready,
    /// All configured local capacity is occupied.
    Busy,
    /// The local upstream is not ready.
    Unhealthy,
    /// Input plus output budget cannot fit safely.
    ContextOverflow,
}

/// Final gateway destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDestination {
    /// Configured local llama.cpp upstream.
    Local,
    /// Configured OpenRouter upstream.
    Cloud,
}

impl RouteDestination {
    /// Stable bounded value used in response headers, logs, and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }
}

/// Bounded explanation for a route decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteReason {
    /// Caller selected the local virtual model or alias.
    ExplicitLocal,
    /// Caller selected cloud or a provider-qualified model.
    ExplicitCloud,
    /// Caller supplied the local-only privacy directive.
    LocalOnly,
    /// Automatic policy selected compatible idle local capacity.
    LocalCapable,
    /// Request needs a feature not enabled locally.
    LocalIncompatible,
    /// Local request exceeded the safe context budget.
    LocalContextLimit,
    /// Local capacity was occupied.
    LocalBusy,
    /// Local readiness failed.
    LocalUnhealthy,
    /// A local request failed before the response commitment point.
    LocalEarlyFailure,
    /// Configuration defaults automatic work to cloud.
    CloudDefault,
    /// Semantic routing determined that stronger cloud intelligence is warranted.
    CloudQuality,
    /// Semantic routing failed safely, so automatic work went to cloud.
    RouterFailure,
}

impl RouteReason {
    /// Stable bounded value used in response headers, logs, and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitLocal => "explicit_local",
            Self::ExplicitCloud => "explicit_cloud",
            Self::LocalOnly => "local_only",
            Self::LocalCapable => "local_capable",
            Self::LocalIncompatible => "local_incompatible",
            Self::LocalContextLimit => "local_context_limit",
            Self::LocalBusy => "local_busy",
            Self::LocalUnhealthy => "local_unhealthy",
            Self::LocalEarlyFailure => "local_early_failure",
            Self::CloudDefault => "cloud_default",
            Self::CloudQuality => "cloud_quality",
            Self::RouterFailure => "router_failure",
        }
    }
}

/// Complete policy result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDecision {
    destination: RouteDestination,
    reason: RouteReason,
    fallback_before_commit: bool,
}

impl RouteDecision {
    /// Selected destination.
    pub fn destination(&self) -> RouteDestination {
        self.destination
    }

    /// Bounded reason code.
    pub fn reason(&self) -> RouteReason {
        self.reason
    }

    /// Whether an early local failure may spill to cloud.
    pub fn fallback_before_commit(&self) -> bool {
        self.fallback_before_commit
    }
}

/// Ordered deterministic local/cloud policy.
pub struct RoutePolicy<'a> {
    config: &'a GatewayConfig,
}

/// Policy result before live local admission is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutePlan {
    /// The request can go directly to cloud without probing local state.
    Cloud(RouteReason),
    /// The request needs a live local admission outcome.
    Local(LocalRoutePlan),
}

impl RoutePlan {
    /// Resolve a plan from a supplied local state, primarily for policy tests.
    #[cfg(test)]
    pub(crate) fn resolve(
        self,
        local_state: LocalAdmissionState,
    ) -> Result<RouteDecision, RoutePolicyError> {
        match self {
            Self::Cloud(reason) => Ok(cloud(reason)),
            Self::Local(plan) => plan.resolve(local_state),
        }
    }
}

/// Policy state needed to resolve one local admission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalRoutePlan {
    /// Explicit local or local-only traffic cannot spill to cloud.
    Forced { reason: RouteReason },
    /// Automatic traffic may fall back before response commitment.
    Automatic { fallback_before_commit: bool },
}

impl LocalRoutePlan {
    pub(crate) fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic { .. })
    }

    /// Complete a successful local admission.
    pub(crate) fn admitted(self) -> RouteDecision {
        let (reason, fallback_before_commit) = match self {
            Self::Forced { reason } => (reason, false),
            Self::Automatic {
                fallback_before_commit,
            } => (RouteReason::LocalCapable, fallback_before_commit),
        };
        RouteDecision {
            destination: RouteDestination::Local,
            reason,
            fallback_before_commit,
        }
    }

    /// Complete the plan from a live local admission state.
    pub(crate) fn resolve(
        self,
        state: LocalAdmissionState,
    ) -> Result<RouteDecision, RoutePolicyError> {
        let reason = match state {
            LocalAdmissionState::Ready => return Ok(self.admitted()),
            LocalAdmissionState::Busy => RouteReason::LocalBusy,
            LocalAdmissionState::Unhealthy => RouteReason::LocalUnhealthy,
            LocalAdmissionState::ContextOverflow => RouteReason::LocalContextLimit,
        };
        if matches!(self, Self::Forced { .. }) {
            return Err(RoutePolicyError::LocalUnavailable { state });
        }
        Ok(cloud(reason))
    }
}

impl<'a> RoutePolicy<'a> {
    /// Build a policy over immutable validated configuration.
    pub fn new(config: &'a GatewayConfig) -> Self {
        Self { config }
    }

    /// Classify an immediate cloud route or a request needing local admission.
    pub(crate) fn plan(
        &self,
        request: &GatewayRequest,
        intent: &ModelIntent,
        privacy: PrivacyDirective,
    ) -> Result<RoutePlan, RoutePolicyError> {
        if privacy == PrivacyDirective::LocalOnly
            && matches!(intent, ModelIntent::CloudAuto | ModelIntent::CloudModel(_))
        {
            return Err(RoutePolicyError::ContradictoryIntent);
        }

        if matches!(intent, ModelIntent::CloudAuto | ModelIntent::CloudModel(_)) {
            return Ok(RoutePlan::Cloud(RouteReason::ExplicitCloud));
        }

        let forced_local = *intent == ModelIntent::Local || privacy == PrivacyDirective::LocalOnly;
        if !self.local_compatible(request) {
            return if forced_local {
                Err(RoutePolicyError::LocalIncompatible)
            } else {
                Ok(RoutePlan::Cloud(RouteReason::LocalIncompatible))
            };
        }

        if forced_local {
            let reason = if privacy == PrivacyDirective::LocalOnly {
                RouteReason::LocalOnly
            } else {
                RouteReason::ExplicitLocal
            };
            return Ok(RoutePlan::Local(LocalRoutePlan::Forced { reason }));
        }

        if self.config.routing().default() == RouteDefault::Cloud {
            return Ok(RoutePlan::Cloud(RouteReason::CloudDefault));
        }

        Ok(RoutePlan::Local(LocalRoutePlan::Automatic {
            fallback_before_commit: self.config.routing().fallback_before_commit(),
        }))
    }

    fn local_compatible(&self, request: &GatewayRequest) -> bool {
        request.features().iter().all(|feature| match feature {
            RequestFeature::Capability(capability) => self.config.local().supports(*capability),
            RequestFeature::OpenRouterPlugins
            | RequestFeature::NonTextOutput
            | RequestFeature::UnsupportedContent => false,
        })
    }
}

fn cloud(reason: RouteReason) -> RouteDecision {
    RouteDecision {
        destination: RouteDestination::Cloud,
        reason,
        fallback_before_commit: false,
    }
}

/// Policy failures where silently choosing cloud would violate caller intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RoutePolicyError {
    /// Local-only and cloud intent were supplied together.
    #[error("local-only privacy cannot be combined with a cloud model")]
    ContradictoryIntent,
    /// Explicit local work requested an unsupported feature.
    #[error("the request requires a capability not enabled for the local model")]
    LocalIncompatible,
    /// Explicit local work cannot be admitted now.
    #[error("the local model cannot accept the request: {state:?}")]
    LocalUnavailable {
        /// Failed admission state.
        state: LocalAdmissionState,
    },
}
