//! Axum HTTP surface for the executable v3 inference fabric.

use super::{FabricGatewayService, FabricUpstreamTransport, PoolAdmissionState};
use crate::gateway::{
    http::security_headers,
    service::{
        MetadataAuthorizationError,
        metadata_authorization_error as build_metadata_authorization_error,
    },
};
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

async fn readiness<T>(State(service): State<Arc<FabricGatewayService<T>>>) -> Response<Body>
where
    T: FabricUpstreamTransport + 'static,
{
    let readiness = service.readiness().await;
    let pools = readiness
        .pools()
        .iter()
        .map(|(name, state)| (name.clone(), Value::String(pool_state(*state).to_string())))
        .collect::<Map<_, _>>();
    let status = if readiness.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "status": if readiness.is_ready() { "ready" } else { "not_ready" },
            "config_version": 3,
            "pools": pools,
            "provider_runtime": "pending"
        })),
    )
        .into_response()
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
        PoolAdmissionState::Incompatible => "incompatible",
        PoolAdmissionState::Busy => "busy",
        PoolAdmissionState::Unhealthy => "unavailable",
        PoolAdmissionState::ContextOverflow => "context_overflow",
    }
}
