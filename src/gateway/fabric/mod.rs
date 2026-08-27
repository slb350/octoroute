//! Octoroute v3 inference-fabric primitives and executable runtime.

mod config;
mod http;
mod http_support;
mod local_pool;
mod policy;
mod presets;
mod provider;
mod service;
mod transport;

pub use config::{
    FABRIC_CONFIG_VERSION, FabricConfig, FabricConfigError, FabricObservabilityConfig,
    FabricServerConfig, FallbackTrigger, LocalCapability, LocalMemberConfig, LocalPoolConfig,
    PoolStrategy, ProviderConfig, ProviderKind, ProviderProfile, ProviderProtocol, ReasoningEffort,
    RoutePrivacy, RouteTarget, VirtualRoute,
};
pub use http::fabric_gateway_app;
pub use local_pool::{
    LlamaCppPool, LlamaCppPoolBuildError, PoolAdmissionOutcome, PoolAdmissionState, PoolLease,
};
pub use policy::{
    FabricRouteError, LocalRequirements, LocalSelection, MemberSnapshot, PoolSelectionError,
    PrivacyDirective, PrivacyDirectiveError, RoutePlan,
};
pub use presets::{PROVIDER_PRESETS, ProviderPreset, provider_preset};
pub use provider::{
    ProviderAdmissionOutcome, ProviderAdmissionState, ProviderLease, ProviderRegistry,
    ProviderRegistryBuildError, ProviderRequestError,
};
pub use service::{FabricGatewayService, FabricGatewayServiceBuildError, FabricReadiness};
pub use transport::{
    FabricTransport, FabricTransportError, FabricUpstreamTransport, PreparedUpstreamResponse,
};

#[cfg(test)]
mod local_pool_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod tests;
