//! Axum surface for the Octoroute v2 gateway.

use crate::gateway::{
    routing::LocalAdmissionState,
    service::{
        GatewayService, MetadataAuthorizationError, OCTOROUTE_REQUEST_ID_HEADER, REQUEST_ID_HEADER,
        error_response, metadata_authorization_error as build_metadata_authorization_error,
    },
    transport::UpstreamTransport,
};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// Build the complete v2 HTTP application.
pub fn gateway_app<T>(service: GatewayService<T>) -> Router
where
    T: UpstreamTransport + 'static,
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

pub(crate) async fn security_headers(request: Request<Body>, next: Next) -> Response<Body> {
    let mut response = next.run(request).await;
    let gateway_request_id = response
        .headers()
        .get(OCTOROUTE_REQUEST_ID_HEADER)
        .cloned()
        .unwrap_or_else(|| {
            HeaderValue::from_str(&Uuid::new_v4().to_string()).expect("UUID is a valid header")
        });
    if !response.headers().contains_key(OCTOROUTE_REQUEST_ID_HEADER) {
        response
            .headers_mut()
            .insert(OCTOROUTE_REQUEST_ID_HEADER, gateway_request_id.clone());
    }
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, gateway_request_id);
    }
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=(), payment=()",
        ),
        (
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

async fn chat<T>(
    State(service): State<Arc<GatewayService<T>>>,
    request: Request<Body>,
) -> Response<Body>
where
    T: UpstreamTransport + 'static,
{
    service.handle_http_chat(request).await
}

async fn models<T>(
    State(service): State<Arc<GatewayService<T>>>,
    headers: HeaderMap,
) -> Response<Body>
where
    T: UpstreamTransport + 'static,
{
    if let Err(error) = service.authorize_metadata(&headers) {
        return metadata_authorization_error(error);
    }
    let data: Vec<_> = service
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
        .collect();
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn liveness() -> Response<Body> {
    Json(json!({"status": "ok"})).into_response()
}

async fn readiness<T>(State(service): State<Arc<GatewayService<T>>>) -> Response<Body>
where
    T: UpstreamTransport + 'static,
{
    let readiness = service.readiness().await;
    let status = if readiness.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "status": if readiness.is_ready() { "ready" } else { "not_ready" },
            "local": local_state(readiness.local()),
            "openrouter": if readiness.openrouter() { "ready" } else { "unavailable" }
        })),
    )
        .into_response()
}

async fn metrics<T>(
    State(service): State<Arc<GatewayService<T>>>,
    headers: HeaderMap,
) -> Response<Body>
where
    T: UpstreamTransport + 'static,
{
    if let Err(error) = service.authorize_metadata(&headers) {
        return metadata_authorization_error(error);
    }
    match service.metrics_text() {
        Ok(metrics) => {
            let mut response = Response::new(Body::from(metrics));
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            response
        }
        Err(error) => {
            tracing::error!(%error, "failed to encode gateway metrics");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to encode metrics",
                "server_error",
                "metrics_encoding_error",
                &Uuid::new_v4().to_string(),
            )
        }
    }
}

fn metadata_authorization_error(error: MetadataAuthorizationError) -> Response<Body> {
    build_metadata_authorization_error(error, &Uuid::new_v4().to_string())
}

fn local_state(state: LocalAdmissionState) -> &'static str {
    match state {
        LocalAdmissionState::Ready => "ready",
        LocalAdmissionState::Busy => "busy",
        LocalAdmissionState::Unhealthy => "unavailable",
        LocalAdmissionState::ContextOverflow => "unavailable",
    }
}
