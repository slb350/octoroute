//! Integration tests for /health endpoint

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use octoroute::handlers;
use tower::ServiceExt; // for `oneshot` and `ready`

#[tokio::test]
async fn test_health_endpoint_returns_ok() {
    let app = Router::new().route("/health", get(handlers::health::handler));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"OK");
}

#[tokio::test]
async fn test_health_endpoint_not_found() {
    let app = Router::new().route("/health", get(handlers::health::handler));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
