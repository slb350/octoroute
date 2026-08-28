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
    ProviderAdmissionState, RoutePlan,
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
    let status = match &error {
        FabricRouteError::UnknownModel(_) | FabricRouteError::ContradictoryPrivacy => {
            StatusCode::BAD_REQUEST
        }
        FabricRouteError::NoEligibleTarget => StatusCode::SERVICE_UNAVAILABLE,
    };
    error_response(
        status,
        &error.to_string(),
        "invalid_request_error",
        "routing_error",
        request_id,
    )
}

pub(super) fn pool_state_error(
    state: PoolAdmissionState,
    pool: &str,
    request_id: &str,
) -> Response<Body> {
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
    };
    tracing::info!(
        request_id,
        pool,
        state = ?state,
        "v3 local route could not be admitted"
    );
    error_response(status, message, "invalid_request_error", code, request_id)
}

pub(super) fn provider_state_error(
    state: ProviderAdmissionState,
    provider: &str,
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
    tracing::info!(
        request_id,
        provider,
        state = ?state,
        "v3 provider route could not be admitted"
    );
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
