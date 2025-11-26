//! OpenAI-compatible models list handler
//!
//! Handles GET /v1/models requests.

use crate::handlers::AppState;
use axum::{Json, extract::State, response::IntoResponse};

use super::types::{ModelObject, ModelsListResponse};

/// GET /v1/models handler
///
/// Returns a list of available models in OpenAI-compatible format.
///
/// # Response Format
///
/// Returns an object with:
/// - `object`: "list"
/// - `data`: Array of model objects, each with:
///   - `id`: Model identifier (tier name or endpoint name)
///   - `object`: "model"
///   - `created`: Unix timestamp
///   - `owned_by`: "octoroute" for tiers, "user" for configured endpoints
///
/// # Available Models
///
/// Tier-based models (use Octoroute routing):
/// - `auto` - Automatic tier selection via LLM/hybrid routing
/// - `fast` - Route to fast tier (smallest models, lowest latency)
/// - `balanced` - Route to balanced tier (medium models)
/// - `deep` - Route to deep tier (largest models, best quality)
///
/// Plus all configured endpoint names from config.toml, which bypass
/// routing and directly use that specific endpoint.
pub async fn handler(State(state): State<AppState>) -> impl IntoResponse {
    // Start with tier-based virtual models
    let mut models = vec![
        ModelObject::new("auto", "octoroute"),
        ModelObject::new("fast", "octoroute"),
        ModelObject::new("balanced", "octoroute"),
        ModelObject::new("deep", "octoroute"),
    ];

    // Add configured endpoint names from each tier
    for endpoint in &state.config().models.fast {
        models.push(ModelObject::new(endpoint.name(), "user"));
    }
    for endpoint in &state.config().models.balanced {
        models.push(ModelObject::new(endpoint.name(), "user"));
    }
    for endpoint in &state.config().models.deep {
        models.push(ModelObject::new(endpoint.name(), "user"));
    }

    Json(ModelsListResponse::new(models))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_object_tier() {
        let model = ModelObject::new("fast", "octoroute");
        assert_eq!(model.id, "fast");
        assert_eq!(model.object, "model");
        assert_eq!(model.owned_by, "octoroute");
        // OpenAI uses 0 for many models
        assert_eq!(model.created, 0);
    }

    #[test]
    fn test_model_object_endpoint() {
        let model = ModelObject::new("qwen3-8b", "user");
        assert_eq!(model.id, "qwen3-8b");
        assert_eq!(model.object, "model");
        assert_eq!(model.owned_by, "user");
    }

    #[test]
    fn test_models_list_response() {
        let models = vec![
            ModelObject::new("auto", "octoroute"),
            ModelObject::new("fast", "octoroute"),
        ];
        let response = ModelsListResponse::new(models);
        assert_eq!(response.object, "list");
        assert_eq!(response.data.len(), 2);
    }
}
