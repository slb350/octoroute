//! Deterministic virtual-model planning and local-pool member selection.

use super::{
    FabricConfig, FallbackTrigger, PoolStrategy, ReasoningEffort, RoutePrivacy, RouteTarget,
};
use crate::gateway::config::LocalCapability;
use reqwest::Url;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

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
            local_only,
        })
    }

    /// Select an idle local member using least-load selection with rotating tie-breaking.
    pub fn select_local_member(
        &self,
        pool_name: &str,
        requirements: &LocalRequirements,
        snapshots: &[MemberSnapshot],
        cursor: usize,
    ) -> Result<LocalSelection, PoolSelectionError> {
        let pool = self
            .local_pools
            .get(pool_name)
            .ok_or_else(|| PoolSelectionError::UnknownPool(pool_name.to_string()))?;
        if !pool.enabled || pool.members.is_empty() {
            return Err(PoolSelectionError::Disabled);
        }
        if !requirements
            .capabilities
            .iter()
            .all(|capability| pool.capabilities.contains(capability))
        {
            return Err(PoolSelectionError::Incompatible);
        }

        let used_context = u64::from(requirements.input_tokens)
            + u64::from(requirements.output_tokens)
            + u64::from(pool.context_safety_tokens);
        if used_context > u64::from(pool.context_window) {
            return Err(PoolSelectionError::ContextOverflow);
        }

        let snapshot_by_member = snapshots
            .iter()
            .filter(|snapshot| snapshot.pool == pool_name)
            .map(|snapshot| (snapshot.member.as_str(), snapshot))
            .collect::<BTreeMap<_, _>>();
        let pool_len = pool.members.len();
        let mut any_enabled = false;
        let mut any_healthy = false;
        let mut candidates = Vec::new();

        for (index, member) in pool.members.iter().enumerate() {
            if !member.enabled {
                continue;
            }
            any_enabled = true;
            let Some(snapshot) = snapshot_by_member.get(member.name.as_str()) else {
                continue;
            };
            if !snapshot.healthy {
                continue;
            }
            any_healthy = true;
            if snapshot.in_flight >= member.max_in_flight {
                continue;
            }
            let rotation = (index + pool_len - (cursor % pool_len)) % pool_len;
            candidates.push((snapshot.in_flight, member.priority, rotation, index, member));
        }

        if !any_enabled {
            return Err(PoolSelectionError::Disabled);
        }

        match pool.strategy {
            PoolStrategy::LeastLoaded => candidates.sort_by(|left, right| {
                left.0
                    .cmp(&right.0)
                    .then(left.1.cmp(&right.1))
                    .then(left.2.cmp(&right.2))
                    .then_with(|| left.4.name.cmp(&right.4.name))
            }),
        }

        let Some((_, _, _, index, member)) = candidates.into_iter().next() else {
            return Err(if any_healthy {
                PoolSelectionError::Busy
            } else {
                PoolSelectionError::Unhealthy
            });
        };

        Ok(LocalSelection {
            pool: pool.name.clone(),
            member: member.name.clone(),
            base_url: member.base_url.clone(),
            model: pool.model.clone(),
            reasoning_effort: pool.default_reasoning_effort,
            next_cursor: (index + 1) % pool_len,
        })
    }
}

/// Route returned to the v3 service layer.
#[derive(Debug, Clone)]
pub struct RoutePlan {
    pub model: String,
    pub steps: Vec<RouteTarget>,
    pub default_reasoning_effort: ReasoningEffort,
    pub fallback_on: BTreeSet<FallbackTrigger>,
    pub local_only: bool,
}

/// Exact local request requirements used for admission and member selection.
#[derive(Debug, Clone)]
pub struct LocalRequirements {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub capabilities: BTreeSet<LocalCapability>,
}

/// Live bounded state for one local pool member.
#[derive(Debug, Clone)]
pub struct MemberSnapshot {
    pub pool: String,
    pub member: String,
    pub healthy: bool,
    pub in_flight: usize,
}

/// Selected local endpoint and the cursor to use for the next tie.
#[derive(Debug, Clone)]
pub struct LocalSelection {
    pub pool: String,
    pub member: String,
    pub base_url: Url,
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
    pub next_cursor: usize,
}

/// Local pool selection failures, suitable for bounded route-reason mapping.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PoolSelectionError {
    #[error("unknown local pool `{0}`")]
    UnknownPool(String),
    #[error("local pool is disabled")]
    Disabled,
    #[error("local pool does not support the request")]
    Incompatible,
    #[error("request exceeds the local pool context budget")]
    ContextOverflow,
    #[error("every healthy local member is busy")]
    Busy,
    #[error("no healthy local member is available")]
    Unhealthy,
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
