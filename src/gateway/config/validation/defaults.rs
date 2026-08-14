use super::{LogLevel, RouteDefault, SemanticRoutingMode};

pub(super) const fn default_max_request_bytes() -> usize {
    8 * 1024 * 1024
}

pub(super) const fn default_max_header_bytes() -> usize {
    32 * 1024
}

pub(super) const fn default_server_max_in_flight() -> usize {
    32
}

pub(super) const fn default_requests_per_minute() -> u32 {
    120
}

pub(super) const fn default_max_output_tokens() -> u32 {
    4096
}

pub(super) const fn default_health_cache_ttl_ms() -> u64 {
    1000
}

pub(super) const fn default_probe_timeout_ms() -> u64 {
    2000
}

pub(super) fn default_auto_model() -> String {
    "openrouter/auto".to_string()
}

pub(super) const fn default_cost_quality_tradeoff() -> u8 {
    9
}

pub(super) fn default_app_title() -> String {
    "Octoroute".to_string()
}

pub(super) const fn default_cloud_max_in_flight() -> usize {
    8
}

pub(super) const fn default_cloud_health_cache_ttl_ms() -> u64 {
    10_000
}

pub(super) const fn default_cloud_probe_timeout_ms() -> u64 {
    3000
}

pub(super) const fn default_route() -> RouteDefault {
    RouteDefault::PreferLocal
}

pub(super) const fn default_true() -> bool {
    true
}

pub(super) const fn default_routing_decision_timeout_ms() -> u64 {
    30_000
}

pub(super) const fn default_local_success_threshold() -> f64 {
    0.50
}

pub(super) const fn default_boundary_threshold_step() -> f64 {
    0.10
}

pub(super) const fn default_session_latch_ttl_ms() -> u64 {
    15 * 60 * 1000
}

pub(super) const fn default_session_latch_max_entries() -> usize {
    1024
}

pub(super) const fn default_session_latch_evidence_threshold() -> u8 {
    2
}

pub(super) const fn default_semantic_routing_mode() -> SemanticRoutingMode {
    SemanticRoutingMode::Shadow
}

pub(super) const fn default_log_level() -> LogLevel {
    LogLevel::Info
}
