use super::MetadataAuthorizationError;
use crate::gateway::routing::RoutePolicyError;
use axum::{
    Json,
    body::Body,
    http::{
        HeaderMap, HeaderName, HeaderValue, Response, StatusCode,
        header::{CONTENT_TYPE, WWW_AUTHENTICATE},
    },
    response::IntoResponse,
};
use serde_json::json;

const REQUEST_ID_HEADER: &str = "x-request-id";

pub(crate) fn authorization_error(
    error: MetadataAuthorizationError,
    request_id: &str,
) -> Response<Body> {
    let (status, message, error_type, code) = match error {
        MetadataAuthorizationError::HeadersTooLarge => (
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "request headers exceed the configured size limit",
            "invalid_request_error",
            "headers_too_large",
        ),
        MetadataAuthorizationError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            "bearer authentication failed",
            "authentication_error",
            "authentication_error",
        ),
    };
    let mut response = error_response(status, message, error_type, code, request_id);
    if error == MetadataAuthorizationError::Unauthorized {
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

pub(super) fn route_error(error: RoutePolicyError, request_id: &str) -> Response<Body> {
    let status = match error {
        RoutePolicyError::LocalUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        RoutePolicyError::ContradictoryIntent | RoutePolicyError::LocalIncompatible => {
            StatusCode::BAD_REQUEST
        }
    };
    error_response(
        status,
        &error.to_string(),
        "invalid_request_error",
        "routing_error",
        request_id,
    )
}

pub(crate) fn error_response(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
    request_id: &str,
) -> Response<Body> {
    let mut response = (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": code
            }
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    insert_header(response.headers_mut(), REQUEST_ID_HEADER, request_id);
    response
}

pub(super) fn rate_limit_response(message: &str, code: &str, request_id: &str) -> Response<Body> {
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        message,
        "rate_limit_error",
        code,
        request_id,
    );
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("60"));
    response
}

pub(super) fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    let name = HeaderName::from_static(name);
    let value =
        HeaderValue::from_str(value).expect("configuration produced an invalid HTTP header");
    headers.insert(name, value);
}
