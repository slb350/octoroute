//! One llama.cpp member: health, slot, and token-count probing.

use super::{HEALTH_CACHE_TTL, PROBE_TIMEOUT, PoolAdmissionState};
use crate::gateway::http_client::authorized;
use bytes::Bytes;
use reqwest::{Client, StatusCode, Url};
use secrecy::SecretString;
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};

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
            .filter(|value| value.checked_at.elapsed() < HEALTH_CACHE_TTL)
            .map(|value| value.healthy)
    }

    async fn refresh_health(&self) -> bool {
        let mut cached = self.cached_health.lock().await;
        if let Some(value) = *cached
            && value.checked_at.elapsed() < HEALTH_CACHE_TTL
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
            Ok(response) if response.status().is_success() => response
                .json::<HealthResponse>()
                .await
                .is_ok_and(|response| response.status == "ok"),
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
        match response.json::<Vec<SlotResponse>>().await {
            Ok(slots) if slots.iter().any(|slot| !slot.is_processing) => MemberState::Ready,
            Ok(slots) if !slots.is_empty() => MemberState::Busy,
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
        if !response.status().is_success() {
            tracing::warn!(
                member = self.name.as_str(),
                status = response.status().as_u16(),
                "llama.cpp token-count endpoint answered with a non-success status"
            );
            return Err(InputTokenError::Unsupported);
        }
        response
            .json::<InputTokenResponse>()
            .await
            .map(|response| response.input_tokens)
            .map_err(|_| InputTokenError::Unsupported)
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
            Err(InputTokenError::Unsupported) => PoolAdmissionState::TokenCountUnavailable,
        }
    }
}
