//! Octoroute v3 inference-fabric primitives.
//!
//! The v3 module is deliberately separate from the proven v2 request path while the
//! multi-pool and multi-provider runtime is built out. It owns the validated schema and
//! deterministic policy that the v3 service layer will consume.

mod config;
mod policy;
mod presets;

pub use config::{
    FABRIC_CONFIG_VERSION, FabricConfig, FabricConfigError, FabricObservabilityConfig,
    FabricServerConfig, FallbackTrigger, LocalMemberConfig, LocalPoolConfig, PoolStrategy,
    ProviderConfig, ProviderKind, ProviderProfile, ProviderProtocol, ReasoningEffort, RoutePrivacy,
    RouteTarget, VirtualRoute,
};
pub use policy::{
    FabricRouteError, LocalRequirements, LocalSelection, MemberSnapshot, PoolSelectionError,
    RoutePlan,
};
pub use presets::{PROVIDER_PRESETS, ProviderPreset, provider_preset};

#[cfg(test)]
mod tests;
