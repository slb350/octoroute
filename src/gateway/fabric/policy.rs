//! Deterministic virtual-model planning and local-pool member selection.

use super::{FabricConfig, FallbackTrigger, ReasoningEffort, RoutePrivacy, RouteTarget};
use axum::http::HeaderMap;
use std::collections::BTreeSet;
use thiserror::Error;

const PRIVACY_HEADER: &str = "x-octoroute-privacy";

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

impl FabricConfig {
    /// Return a validated route plan, optionally narrowing it to local-only targets.
    pub fn route_plan(
        &self,
        requested_model: &str,
        local_only: bool,
    ) -> Result<RoutePlan, FabricRouteError> {
        let model = if requested_model == "auto" {
            self.default_model.as_str()
        } else {
            requested_model
        };
        let route = self
            .routes
            .get(model)
            .ok_or_else(|| FabricRouteError::UnknownModel(model.to_string()))?;

        if local_only && route.privacy == RoutePrivacy::CloudOnly {
            return Err(FabricRouteError::ContradictoryPrivacy);
        }

        // A route declaring `local_only` is filtered whether or not the caller
        // sent the header. Config validation also refuses provider steps on such
        // a route, but the plan must not depend on that check having run: this is
        // the boundary the privacy promise is made at.
        let local_only = local_only || route.privacy == RoutePrivacy::LocalOnly;
        let steps = if local_only {
            route
                .steps
                .iter()
                .filter(|step| matches!(step, RouteTarget::LocalPool(_)))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            route.steps.clone()
        };
        if steps.is_empty() {
            return Err(FabricRouteError::NoEligibleTarget);
        }

        Ok(RoutePlan {
            model: route.model.clone(),
            steps,
            default_reasoning_effort: route.default_reasoning_effort,
            fallback_on: route.fallback_on.clone(),
        })
    }
}

impl RoutePlan {
    /// Whether a step failing with `trigger` may fall forward to the next step.
    pub fn may_fall_forward(&self, has_more: bool, trigger: FallbackTrigger) -> bool {
        has_more && self.fallback_on.contains(&trigger)
    }
}

/// Route returned to the v3 service layer.
#[derive(Debug, Clone)]
pub struct RoutePlan {
    pub model: String,
    /// Targets in route order, filtered for the effective privacy boundary.
    pub steps: Vec<RouteTarget>,
    pub default_reasoning_effort: ReasoningEffort,
    pub fallback_on: BTreeSet<FallbackTrigger>,
}

/// Virtual-model planning failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FabricRouteError {
    #[error("unknown virtual model `{0}`")]
    UnknownModel(String),
    #[error("local-only privacy conflicts with a cloud-only route")]
    ContradictoryPrivacy,
    #[error("no eligible target remains after applying privacy")]
    NoEligibleTarget,
}
