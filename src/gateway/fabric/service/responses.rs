//! Bounded error responses and gateway response decoration.

use super::{
    DESTINATION_HEADER, FabricGatewayServiceBuildError, MEMBER_HEADER, MODEL_REVISION_HEADER,
    POOL_HEADER, PROVIDER_HEADER, REASON_HEADER, ROUTE_HEADER, TARGET_HEADER, UPSTREAM_HEADER,
};
use crate::gateway::env::Environment;
use crate::gateway::fabric::http_support::{
    OCTOROUTE_REQUEST_ID_HEADER, REQUEST_ID_HEADER, error_response, insert_header,
};
use crate::gateway::fabric::metrics::ProviderResponseOutcome;
use crate::gateway::fabric::{
    FabricRouteError, FallbackTrigger, PoolAdmissionState, PreparedUpstreamResponse,
    ProviderAdmissionState, ProviderRequestError, RoutePlan,
};
use axum::{
    body::Body,
    http::{Response, StatusCode},
};
use secrecy::{ExposeSecret, SecretString};

pub(super) fn resolve_secret(
    environment: &(impl Environment + ?Sized),
    field: &str,
    name: &str,
) -> Result<SecretString, FabricGatewayServiceBuildError> {
    let value = environment
        .get(name)
        .filter(|value| !value.expose_secret().trim().is_empty())
        .ok_or_else(
            || FabricGatewayServiceBuildError::MissingEnvironmentVariable {
                field: field.to_string(),
                name: name.to_string(),
            },
        )?;
    if !value
        .expose_secret()
        .bytes()
        .all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(FabricGatewayServiceBuildError::InvalidCredential {
            field: field.to_string(),
        });
    }
    Ok(SecretString::from(value))
}

pub(super) fn fallback_trigger(state: PoolAdmissionState) -> Option<FallbackTrigger> {
    match state {
        PoolAdmissionState::Ready => None,
        PoolAdmissionState::Disabled
        | PoolAdmissionState::Unhealthy
        | PoolAdmissionState::TokenCountUnavailable => Some(FallbackTrigger::Unhealthy),
        // Outside the default trigger set, exactly as on the provider side: a
        // member key the operator forgot to rotate must not read as ill health
        // and spill every request to a metered provider.
        PoolAdmissionState::Unauthenticated => Some(FallbackTrigger::Unauthenticated),
        PoolAdmissionState::Incompatible => Some(FallbackTrigger::Incompatible),
        PoolAdmissionState::Busy => Some(FallbackTrigger::Busy),
        PoolAdmissionState::ContextOverflow => Some(FallbackTrigger::ContextOverflow),
    }
}

pub(super) fn provider_fallback_trigger(state: ProviderAdmissionState) -> Option<FallbackTrigger> {
    match state {
        ProviderAdmissionState::Ready => None,
        ProviderAdmissionState::Disabled | ProviderAdmissionState::Unavailable => {
            Some(FallbackTrigger::Unhealthy)
        }
        ProviderAdmissionState::Incompatible => Some(FallbackTrigger::Incompatible),
        ProviderAdmissionState::Busy => Some(FallbackTrigger::Busy),
        // Outside the default trigger set: an expired or missing key must not
        // silently reroute traffic and spend to the next provider.
        ProviderAdmissionState::Unauthenticated => Some(FallbackTrigger::Unauthenticated),
    }
}

pub(super) fn provider_response_outcome(status: StatusCode) -> ProviderResponseOutcome {
    if status == StatusCode::TOO_MANY_REQUESTS {
        ProviderResponseOutcome::RateLimited
    } else if status.is_server_error() {
        ProviderResponseOutcome::ServerError
    } else if status.is_client_error() {
        ProviderResponseOutcome::ClientError
    } else {
        ProviderResponseOutcome::Success
    }
}

pub(super) fn route_error(error: FabricRouteError, request_id: &str) -> Response<Body> {
    // The error type has to agree with the status. A 503 typed
    // `invalid_request_error` tells a client to retry and to fix its request at
    // the same time, and a client keying on the type acts on the wrong one.
    let (status, error_type) = match &error {
        FabricRouteError::UnknownModel(_) | FabricRouteError::ContradictoryPrivacy => {
            (StatusCode::BAD_REQUEST, "invalid_request_error")
        }
        FabricRouteError::NoEligibleTarget => (StatusCode::SERVICE_UNAVAILABLE, "upstream_error"),
    };
    error_response(
        status,
        &error.to_string(),
        error_type,
        "routing_error",
        request_id,
    )
}

pub(super) fn provider_request_error(
    error: &ProviderRequestError,
    request_id: &str,
) -> Response<Body> {
    let (status, error_type, code) = if error.is_client_error() {
        (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "provider_request_invalid",
        )
    } else {
        (
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            "provider_translation_failed",
        )
    };
    error_response(status, &error.to_string(), error_type, code, request_id)
}

/// A provider rejected Octoroute's own credential after admission.
///
/// Forwarding the upstream's 401 would tell the caller that the bearer it sent
/// this gateway was refused, which is a different operator problem entirely.
pub(super) fn provider_credential_rejected(request_id: &str) -> Response<Body> {
    error_response(
        StatusCode::BAD_GATEWAY,
        "the selected provider rejected the gateway credential",
        "upstream_error",
        "provider_credential_rejected",
        request_id,
    )
}

pub(super) fn pool_state_error(state: PoolAdmissionState, request_id: &str) -> Response<Body> {
    let (status, message, code) = match state {
        PoolAdmissionState::Ready => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "local admission returned an inconsistent ready state",
            "internal_routing_error",
        ),
        PoolAdmissionState::Disabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected local pool is disabled",
            "local_pool_disabled",
        ),
        PoolAdmissionState::Incompatible => (
            StatusCode::BAD_REQUEST,
            "the selected local pool does not support this request",
            "local_incompatible",
        ),
        PoolAdmissionState::TokenCountUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected local pool could not count input tokens for this request",
            "local_token_count_unavailable",
        ),
        PoolAdmissionState::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "every eligible local member is busy",
            "local_busy",
        ),
        PoolAdmissionState::Unhealthy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no healthy local member is available",
            "local_unhealthy",
        ),
        PoolAdmissionState::ContextOverflow => (
            StatusCode::BAD_REQUEST,
            "the request exceeds the selected local pool context budget",
            "local_context_overflow",
        ),
        PoolAdmissionState::Unauthenticated => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected local pool member rejected or could not supply its credential",
            "local_unauthenticated",
        ),
    };
    // Same rule as `route_error`: the type follows the status. Only the two
    // 4xx states are the caller's request to fix; a busy or disabled pool typed
    // `invalid_request_error` tells a client keying on the type to stop
    // retrying and rewrite a request that was fine.
    let error_type = if status.is_client_error() {
        "invalid_request_error"
    } else {
        "upstream_error"
    };
    error_response(status, message, error_type, code, request_id)
}

pub(super) fn provider_state_error(
    state: ProviderAdmissionState,
    request_id: &str,
) -> Response<Body> {
    let (status, message, code) = match state {
        ProviderAdmissionState::Ready => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider admission returned an inconsistent ready state",
            "internal_routing_error",
        ),
        ProviderAdmissionState::Disabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider is disabled",
            "provider_disabled",
        ),
        ProviderAdmissionState::Incompatible => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider does not have a compatible runtime adapter",
            "provider_incompatible",
        ),
        ProviderAdmissionState::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider is at its concurrency limit",
            "provider_busy",
        ),
        ProviderAdmissionState::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider is unavailable",
            "provider_unavailable",
        ),
        ProviderAdmissionState::Unauthenticated => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider rejected or could not supply its credential",
            "provider_unauthenticated",
        ),
    };
    error_response(status, message, "upstream_error", code, request_id)
}

pub(super) fn decorate_local(
    response: PreparedUpstreamResponse,
    plan: &RoutePlan,
    pool: &str,
    member: &str,
    revision: &str,
    request_id: &str,
) -> Response<Body> {
    let upstream = format!("{pool}/{member}");
    let mut response = response.into_response();
    insert_header(response.headers_mut(), DESTINATION_HEADER, "local");
    insert_header(response.headers_mut(), REASON_HEADER, "local_pool");
    insert_header(response.headers_mut(), UPSTREAM_HEADER, &upstream);
    insert_header(response.headers_mut(), ROUTE_HEADER, &plan.model);
    insert_header(
        response.headers_mut(),
        TARGET_HEADER,
        &format!("pool:{pool}"),
    );
    insert_header(response.headers_mut(), POOL_HEADER, pool);
    insert_header(response.headers_mut(), MEMBER_HEADER, member);
    insert_header(response.headers_mut(), MODEL_REVISION_HEADER, revision);
    insert_header(
        response.headers_mut(),
        OCTOROUTE_REQUEST_ID_HEADER,
        request_id,
    );
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        insert_header(response.headers_mut(), REQUEST_ID_HEADER, request_id);
    }
    tracing::info!(
        request_id,
        route = plan.model.as_str(),
        destination = "local",
        pool,
        member,
        status = response.status().as_u16(),
        "v3 gateway response committed"
    );
    response
}

pub(super) fn decorate_provider(
    response: PreparedUpstreamResponse,
    plan: &RoutePlan,
    provider: &str,
    model: &str,
    request_id: &str,
) -> Response<Body> {
    let mut response = response.into_response();
    insert_header(response.headers_mut(), DESTINATION_HEADER, "cloud");
    insert_header(response.headers_mut(), REASON_HEADER, "provider");
    insert_header(response.headers_mut(), UPSTREAM_HEADER, provider);
    insert_header(response.headers_mut(), ROUTE_HEADER, &plan.model);
    insert_header(response.headers_mut(), PROVIDER_HEADER, provider);
    insert_header(
        response.headers_mut(),
        TARGET_HEADER,
        &format!("provider:{provider}"),
    );
    insert_header(
        response.headers_mut(),
        OCTOROUTE_REQUEST_ID_HEADER,
        request_id,
    );
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        insert_header(response.headers_mut(), REQUEST_ID_HEADER, request_id);
    }
    tracing::info!(
        request_id,
        route = plan.model.as_str(),
        destination = "cloud",
        provider,
        model,
        status = response.status().as_u16(),
        "v3 gateway response committed"
    );
    response
}
