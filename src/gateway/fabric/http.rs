//! Axum HTTP surface for the executable v3 inference fabric.

use super::http_support::{
    MetadataAuthorizationError, metadata_authorization_error as build_metadata_authorization_error,
    security_headers,
};
use super::metrics::provider_state;
use super::{FabricGatewayService, FabricUpstreamTransport, PoolAdmissionState};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header::CONTENT_TYPE},
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use uuid::Uuid;

/// Build the authenticated OpenAI-compatible v3 application.
pub fn fabric_gateway_app<T>(service: FabricGatewayService<T>) -> Router
where
    T: FabricUpstreamTransport + 'static,
{
    let state = Arc::new(service);
    Router::new()
        .route("/v1/chat/completions", post(chat::<T>))
        .route("/v1/models", get(models::<T>))
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness::<T>))
        .route("/health", get(readiness::<T>))
        .route("/metrics", get(metrics::<T>))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn chat<T>(
    State(service): State<Arc<FabricGatewayService<T>>>,
    request: Request<Body>,
) -> Response<Body>
where
    T: FabricUpstreamTransport + 'static,
{
    service.handle_http_chat(request).await
}

async fn models<T>(
    State(service): State<Arc<FabricGatewayService<T>>>,
    headers: HeaderMap,
) -> Response<Body>
where
    T: FabricUpstreamTransport + 'static,
{
    if let Err(error) = service.authorize_metadata(&headers) {
        return metadata_authorization_error(error);
    }
    let data = service
        .model_ids()
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": "octoroute"
            })
        })
        .collect::<Vec<_>>();
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn liveness() -> Response<Body> {
    Json(json!({"status": "ok", "config_version": 3})).into_response()
}

/// Aggregate readiness, unauthenticated by contract.
///
/// The per-target breakdown names every configured pool and provider, so it is
/// returned only to an authenticated caller. An anonymous caller - a load
/// balancer or an orchestrator probe - gets the status code and the aggregate,
/// which is all a health check needs. The snapshot is cached so an anonymous
/// caller cannot drive `codex doctor` spawns and credentialed `/models` probes
/// at request rate.
async fn readiness<T>(
    State(service): State<Arc<FabricGatewayService<T>>>,
    headers: HeaderMap,
) -> Response<Body>
where
    T: FabricUpstreamTransport + 'static,
{
    let readiness = service.cached_readiness().await;
    let ready = readiness.is_ready();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let status_label = if !ready {
        "not_ready"
    } else if readiness.is_degraded() {
        "degraded"
    } else {
        "ready"
    };
    let mut body = Map::from_iter([
        (
            "status".to_string(),
            Value::String(status_label.to_string()),
        ),
        ("config_version".to_string(), json!(3)),
    ]);
    if service.authorize_metadata(&headers).is_ok() {
        body.insert(
            "pools".to_string(),
            Value::Object(
                readiness
                    .pools()
                    .iter()
                    .map(|(name, state)| {
                        (name.clone(), Value::String(pool_state(*state).to_string()))
                    })
                    .collect(),
            ),
        );
        body.insert(
            "providers".to_string(),
            Value::Object(
                readiness
                    .providers()
                    .iter()
                    .map(|(name, state)| {
                        (
                            name.clone(),
                            Value::String(provider_state(*state).to_string()),
                        )
                    })
                    .collect(),
            ),
        );
        body.insert(
            "provider_runtime".to_string(),
            Value::String("complete".to_string()),
        );
    }
    (status, Json(Value::Object(body))).into_response()
}

async fn metrics<T>(
    State(service): State<Arc<FabricGatewayService<T>>>,
    headers: HeaderMap,
) -> Response<Body>
where
    T: FabricUpstreamTransport + 'static,
{
    if let Err(error) = service.authorize_metadata(&headers) {
        return metadata_authorization_error(error);
    }
    let mut response = Response::new(Body::from(service.metrics_text()));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

fn metadata_authorization_error(error: MetadataAuthorizationError) -> Response<Body> {
    build_metadata_authorization_error(error, &Uuid::new_v4().to_string())
}

fn pool_state(state: PoolAdmissionState) -> &'static str {
    match state {
        PoolAdmissionState::Ready => "ready",
        PoolAdmissionState::Disabled => "disabled",
        PoolAdmissionState::Busy => "busy",
        PoolAdmissionState::Unhealthy => "unavailable",
        PoolAdmissionState::TokenCountUnavailable => "token_count_unavailable",
        // `readiness_state` never produces these; they belong to per-request
        // admission. Rendering them as `unavailable` keeps the readiness label
        // set closed rather than silently adding request-scoped values to it.
        PoolAdmissionState::Incompatible | PoolAdmissionState::ContextOverflow => "unavailable",
    }
}
