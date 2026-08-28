//! One llama.cpp member: health, slot, and token-count probing.

use super::{HEALTH_CACHE_TTL, PROBE_TIMEOUT, PoolAdmissionState};
use crate::gateway::fabric::bounded_response::{self, BoundedResponseError};
use crate::gateway::http_client::authorized;
use bytes::Bytes;
use reqwest::{Client, Response, StatusCode, Url};
use secrecy::SecretString;
use serde::{Deserialize, de::DeserializeOwned};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};

/// Ceiling on a probe response body.
///
/// `/health` and `/v1/chat/completions/input_tokens` answer with one small JSON
/// object. Reading them with `Response::json` would accept whatever an upstream
/// chose to send, so a member that had been replaced or hijacked could size
/// Octoroute's memory. Anything past this ceiling is not an answer this code can
/// use, and is treated as an unusable response rather than truncated.
///
/// `/slots` sets the size: llama.cpp emits roughly 1.1 KB per slot, and with
/// `LLAMA_SERVER_SLOTS_DEBUG` each slot additionally carries its prompt and
/// generated text. A 64 KiB ceiling was reached at about 59 slots, which a
/// `-np 64` member exceeds in normal operation, and the failure was silent and
/// permanent: the member read as unhealthy on every probe forever. One mebibyte
/// covers roughly 950 plain slots while still bounding a hostile response.
const MAX_PROBE_BODY_BYTES: usize = 1024 * 1024;

/// Deserialize a probe body, refusing to buffer more than [`MAX_PROBE_BODY_BYTES`].
async fn bounded_json<T: DeserializeOwned>(response: Response) -> Option<T> {
    let body = match bounded_response::read(response, MAX_PROBE_BODY_BYTES).await {
        Ok(body) => body,
        Err(BoundedResponseError::TooLarge) => {
            // Without this, an over-ceiling body is indistinguishable from an
            // unparseable one, and the member reads as unhealthy with nothing
            // to explain it. The ceiling is logged; the body never is.
            tracing::warn!(
                ceiling_bytes = MAX_PROBE_BODY_BYTES,
                "a llama.cpp probe response exceeded the bounded read ceiling and was discarded"
            );
            return None;
        }
        Err(BoundedResponseError::Read { .. }) => return None,
    };
    serde_json::from_slice(&body).ok()
}

pub(super) struct Member {
    pub(super) name: String,
    pub(super) priority: u16,
    pub(super) api_key: Option<SecretString>,
    pub(super) client: Client,
    pub(super) health_url: Url,
    pub(super) slots_url: Url,
    pub(super) input_tokens_url: Url,
    pub(super) chat_url: Url,
    pub(super) permits: Arc<Semaphore>,
    pub(super) max_in_flight: usize,
    pub(super) cached_health: Mutex<Option<CachedHealth>>,
    pub(super) token_count_timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CachedHealth {
    checked_at: Instant,
    healthy: bool,
}

impl CachedHealth {
    fn is_fresh_at(self, now: Instant) -> bool {
        now.saturating_duration_since(self.checked_at) < HEALTH_CACHE_TTL
    }
}

#[derive(Debug, Deserialize)]
struct HealthResponse {
    status: String,
}

#[derive(Debug, Deserialize)]
struct SlotResponse {
    is_processing: bool,
}

#[derive(Debug, Deserialize)]
struct InputTokenResponse {
    input_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MemberState {
    Ready,
    Busy,
    Unhealthy,
}

/// Why a token count could not be obtained from a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputTokenError {
    /// The member could not be reached, or the probe deadline elapsed.
    Transport,
    /// The member answered, but not with a usable token count. The endpoint is
    /// missing or has changed shape; the member itself is up.
    Unsupported,
    /// The member has no spare capacity to count this body right now.
    ///
    /// Load is not ill-health. A route that enables `busy` and deliberately
    /// omits `unhealthy`, so that a genuinely sick pool surfaces instead of
    /// spending on cloud, still has to fall forward on this.
    Busy,
    /// The member, or a proxy in front of it, rejected the credential.
    ///
    /// Kept distinct from ill-health because `unhealthy` is in the default
    /// fallback set: a rotated member key would otherwise bill every request to
    /// a paid provider with nothing surfaced to the operator.
    Unauthenticated,
    /// The member understood the endpoint and refused this request body.
    ///
    /// `/v1/chat/completions/input_tokens` applies the chat template, so this
    /// verdict is a property of the request rather than of the member: every
    /// equivalent member would reach it. Retrying re-discloses the same prompt
    /// for the same answer, and reporting it as member ill-health spills the
    /// prompt to a paid provider on a `cloud_allowed` route.
    RequestRejected,
}

impl Member {
    pub(super) async fn availability_state(&self) -> MemberState {
        if let Some(healthy) = self.fresh_cached_health().await {
            return if healthy {
                self.slot_state().await
            } else {
                MemberState::Unhealthy
            };
        }
        let (healthy, slot) = tokio::join!(self.refresh_health(), self.slot_state());
        if healthy {
            slot
        } else {
            MemberState::Unhealthy
        }
    }

    async fn fresh_cached_health(&self) -> Option<bool> {
        let cached = self.cached_health.lock().await;
        cached
            .filter(|value| value.is_fresh_at(Instant::now()))
            .map(|value| value.healthy)
    }

    async fn refresh_health(&self) -> bool {
        let mut cached = self.cached_health.lock().await;
        if let Some(value) = *cached
            && value.is_fresh_at(Instant::now())
        {
            return value.healthy;
        }
        let healthy = self.probe_health().await;
        *cached = Some(CachedHealth {
            checked_at: Instant::now(),
            healthy,
        });
        healthy
    }

    async fn probe_health(&self) -> bool {
        match authorized(
            self.client.get(self.health_url.clone()),
            self.api_key.as_ref(),
        )
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        {
            Ok(response) if response.status().is_success() => {
                bounded_json::<HealthResponse>(response)
                    .await
                    .is_some_and(|response| response.status == "ok")
            }
            _ => false,
        }
    }

    async fn slot_state(&self) -> MemberState {
        let response = match authorized(
            self.client.get(self.slots_url.clone()),
            self.api_key.as_ref(),
        )
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        {
            Ok(response) => response,
            Err(_) => return MemberState::Unhealthy,
        };
        if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            return MemberState::Busy;
        }
        if !response.status().is_success() {
            return MemberState::Unhealthy;
        }
        match bounded_json::<Vec<SlotResponse>>(response).await {
            Some(slots) if slots.iter().any(|slot| !slot.is_processing) => MemberState::Ready,
            Some(slots) if !slots.is_empty() => MemberState::Busy,
            _ => MemberState::Unhealthy,
        }
    }

    pub(super) async fn input_tokens(&self, body: Bytes) -> Result<u32, InputTokenError> {
        let response = authorized(
            self.client.post(self.input_tokens_url.clone()),
            self.api_key.as_ref(),
        )
        .timeout(self.token_count_timeout)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| InputTokenError::Transport)?;
        let status = response.status();
        if !status.is_success() {
            tracing::warn!(
                member = self.name.as_str(),
                status = status.as_u16(),
                "llama.cpp token-count endpoint answered with a non-success status"
            );
            return Err(classify_token_count_status(status));
        }
        bounded_json::<InputTokenResponse>(response)
            .await
            .map(|response| response.input_tokens)
            .ok_or(InputTokenError::Unsupported)
    }

    /// Probe the token-count endpoint the admission path depends on.
    ///
    /// Readiness must exercise the same endpoint as admission, or a member whose
    /// token endpoint has gone missing reports `ready` forever while rejecting
    /// every request it is offered.
    pub(super) async fn token_count_readiness(&self, probe_body: Bytes) -> PoolAdmissionState {
        match self.input_tokens(probe_body).await {
            Ok(_) => PoolAdmissionState::Ready,
            Err(InputTokenError::Transport) => PoolAdmissionState::Unhealthy,
            Err(InputTokenError::Busy) => PoolAdmissionState::Busy,
            Err(InputTokenError::Unauthenticated) => PoolAdmissionState::Unauthenticated,
            // The readiness probe body is fixed and minimal, so a refusal of it
            // is a property of the endpoint, not of any client request.
            Err(InputTokenError::Unsupported | InputTokenError::RequestRejected) => {
                PoolAdmissionState::TokenCountUnavailable
            }
        }
    }
}

/// Separate a request-caused token-count refusal from a member-caused one.
///
/// A 4xx generally means the endpoint applied the chat template to this exact
/// body and refused it. Four statuses describe the member instead, and each
/// belongs to a different fallback class:
///
/// - 401, 403, and 407 are a credential rejection, which must surface rather
///   than read as ill-health that the default fallback set spills to cloud.
/// - 429 is capacity, which a route configures `busy` for.
/// - 408 is a deadline, indistinguishable in effect from a transport failure.
/// - 404 means the endpoint is absent. So does any 5xx, 501 included, which the
///   final arm already covers.
fn classify_token_count_status(status: StatusCode) -> InputTokenError {
    match status {
        StatusCode::UNAUTHORIZED
        | StatusCode::FORBIDDEN
        | StatusCode::PROXY_AUTHENTICATION_REQUIRED => InputTokenError::Unauthenticated,
        StatusCode::TOO_MANY_REQUESTS => InputTokenError::Busy,
        StatusCode::REQUEST_TIMEOUT => InputTokenError::Transport,
        StatusCode::NOT_FOUND => InputTokenError::Unsupported,
        status if status.is_client_error() => InputTokenError::RequestRejected,
        _ => InputTokenError::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CachedHealth, HEALTH_CACHE_TTL, HealthResponse, MAX_PROBE_BODY_BYTES, Member, bounded_json,
    };
    use reqwest::{Client, Url};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::{Mutex, Semaphore};

    #[test]
    fn cached_health_expires_exactly_at_its_ttl() {
        let checked_at = Instant::now();
        let cached = CachedHealth {
            checked_at,
            healthy: true,
        };

        assert!(cached.is_fresh_at(checked_at));
        assert!(cached.is_fresh_at(checked_at + HEALTH_CACHE_TTL - Duration::from_nanos(1)));
        assert!(!cached.is_fresh_at(checked_at + HEALTH_CACHE_TTL));
        assert!(!cached.is_fresh_at(checked_at + HEALTH_CACHE_TTL + Duration::from_nanos(1)));
    }

    #[tokio::test]
    async fn fresh_cached_health_returns_the_cached_verdict() {
        let url = Url::parse("http://127.0.0.1/").expect("URL");
        let member = Member {
            name: "worker-0".to_string(),
            priority: 1,
            api_key: None,
            client: Client::new(),
            health_url: url.clone(),
            slots_url: url.clone(),
            input_tokens_url: url.clone(),
            chat_url: url,
            permits: Arc::new(Semaphore::new(1)),
            max_in_flight: 1,
            cached_health: Mutex::new(Some(CachedHealth {
                checked_at: Instant::now(),
                healthy: false,
            })),
            token_count_timeout: Duration::from_secs(1),
        };

        assert_eq!(member.fresh_cached_health().await, Some(false));
    }

    /// A `/health` body of exactly `bytes` bytes, delivered as one frame.
    ///
    /// The single frame is the point: over a socket the first chunk is whatever
    /// the peer's read buffer happened to return, so the boundary cases below
    /// cannot be posed exactly through a mock server.
    fn health_response(bytes: usize) -> reqwest::Response {
        let padding = bytes - r#"{"status":"ok","pad":""}"#.len();
        let body = format!(r#"{{"status":"ok","pad":"{}"}}"#, "x".repeat(padding));
        assert_eq!(body.len(), bytes);
        reqwest::Response::from(axum::http::Response::new(reqwest::Body::from(body)))
    }

    /// The ceiling is inclusive. A body that lands exactly on it is still an
    /// answer this code can use, and a `/slots` payload one byte inside the
    /// bound must not be discarded.
    #[tokio::test]
    async fn a_probe_body_of_exactly_the_ceiling_is_accepted() {
        let health: Option<HealthResponse> =
            bounded_json(health_response(MAX_PROBE_BODY_BYTES)).await;

        assert_eq!(health.map(|health| health.status).as_deref(), Some("ok"));
    }

    /// One byte past the ceiling is refused before it is buffered, including
    /// when the whole body arrives in the very first frame.
    #[tokio::test]
    async fn a_probe_body_one_byte_past_the_ceiling_is_refused() {
        let response = health_response(MAX_PROBE_BODY_BYTES + 1);

        assert!(bounded_json::<HealthResponse>(response).await.is_none());
    }
}
