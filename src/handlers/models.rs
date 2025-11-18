//! Models endpoint handler
//!
//! Exposes model endpoint health status via GET /models

use crate::handlers::AppState;
use axum::{Json, extract::State};
use serde::Serialize;

/// Response for GET /models endpoint
#[derive(Serialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelStatus>,
}

/// Health status for a single model endpoint
#[derive(Serialize)]
pub struct ModelStatus {
    pub name: String,
    pub tier: String, // "fast", "balanced", or "deep"
    pub endpoint: String,
    pub healthy: bool,
    pub last_check_seconds_ago: u64,
    pub consecutive_failures: u32,
}

/// GET /models handler
///
/// Returns health status of all model endpoints across all tiers.
pub async fn handler(State(state): State<AppState>) -> Json<ModelsResponse> {
    let health_statuses = state.selector().health_checker().get_all_statuses().await;

    // Determine tier for each endpoint by checking which tier it belongs to
    let config = state.config();
    let models: Vec<ModelStatus> = health_statuses
        .into_iter()
        .map(|h| {
            // Determine tier by checking config
            let tier = if config.models.fast.iter().any(|e| e.name == h.name) {
                "fast".to_string()
            } else if config.models.balanced.iter().any(|e| e.name == h.name) {
                "balanced".to_string()
            } else if config.models.deep.iter().any(|e| e.name == h.name) {
                "deep".to_string()
            } else {
                "unknown".to_string()
            };

            ModelStatus {
                name: h.name,
                tier,
                endpoint: h.base_url,
                healthy: h.healthy,
                last_check_seconds_ago: h.last_check.elapsed().as_secs(),
                consecutive_failures: h.consecutive_failures,
            }
        })
        .collect();

    tracing::debug!(
        total_models = models.len(),
        healthy_count = models.iter().filter(|m| m.healthy).count(),
        "Retrieved model status for /models endpoint"
    );

    Json(ModelsResponse { models })
}
