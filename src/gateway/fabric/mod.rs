//! Octoroute v3 inference-fabric primitives.
//!
//! The v3 module is deliberately separate from the proven v2 request path while the
//! multi-pool and multi-provider runtime is built out. It owns the validated schema and
//! deterministic policy that the v3 service layer will consume.

mod config;
mod policy;
mod presets;

pub use config::{
    FabricConfig, FabricConfigError, FabricObservabilityConfig, FabricServerConfig,
    FallbackTrigger, LocalMemberConfig, LocalPoolConfig, PoolStrategy, ProviderConfig,
    ProviderKind, ProviderProfile, ProviderProtocol, ReasoningEffort, RoutePrivacy, RouteTarget,
    VirtualRoute, FABRIC_CONFIG_VERSION,
};
pub use policy::{
    FabricRouteError, LocalRequirements, LocalSelection, MemberSnapshot, PoolSelectionError,
    RoutePlan,
};
pub use presets::{provider_preset, ProviderPreset, PROVIDER_PRESETS};

#[cfg(test)]
mod tests;
