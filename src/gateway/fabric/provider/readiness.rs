//! Cached provider readiness: the HTTP `/models` probe, the Codex `doctor`
//! probe, and the invalidation that discards a cached verdict a real dispatch
//! has contradicted.
//!
//! Both probes are bounded and cached for the provider's configured TTL, so a
//! readiness pass never fans out an unbounded number of credentialed requests
//! or child processes.

use super::{
    CodexProvider, HttpProvider, ProviderAdmissionState, ProviderProtocol, codex,
    is_provider_credential_rejection,
};
use axum::http::StatusCode;
use reqwest::{RequestBuilder, Url};
use secrecy::{ExposeSecret, SecretString};
use std::time::{Duration, Instant};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One provider's last probe verdict and when it was taken.
#[derive(Clone, Copy)]
pub(super) struct CachedReadiness {
    pub(super) checked_at: Option<Instant>,
    pub(super) state: ProviderAdmissionState,
}

impl Default for CachedReadiness {
    fn default() -> Self {
        Self {
            checked_at: None,
            state: ProviderAdmissionState::Unavailable,
        }
    }
}

impl CachedReadiness {
    /// The cached verdict, if it is still inside `ttl` at `now`.
    ///
    /// Freshness is strict: a verdict taken exactly `ttl` ago has expired. The
    /// difference is the whole meaning of `readiness_ttl_ms = 0`, which asks
    /// for a probe every pass and which a `<=` boundary would turn into "reuse
    /// the first answer forever".
    pub(super) fn reusable(&self, now: Instant, ttl: Duration) -> Option<ProviderAdmissionState> {
        let checked_at = self.checked_at?;
        (now.duration_since(checked_at) < ttl).then_some(self.state)
    }
}

impl HttpProvider {
    /// Discard cached readiness after a real dispatch proved it wrong.
    ///
    /// Without this a provider that dies right after a successful probe keeps
    /// reporting `ready` for the whole TTL, which can be an hour.
    pub(super) async fn invalidate_readiness(&self, status: Option<StatusCode>) {
        if status.is_some_and(is_provider_credential_rejection) {
            self.credential.invalidate().await;
        }
        *self.readiness.lock().await = CachedReadiness::default();
    }

    pub(super) async fn readiness(&self) -> ProviderAdmissionState {
        if self.permits.available_permits() == 0 {
            return ProviderAdmissionState::Busy;
        }
        let mut cached = self.readiness.lock().await;
        let ttl = Duration::from_millis(self.config.readiness_ttl_ms);
        if let Some(state) = cached.reusable(Instant::now(), ttl) {
            return state;
        }
        let state = match self.credential.resolve().await {
            Ok(api_key) => self.probe(&api_key).await,
            Err(_) => ProviderAdmissionState::Unauthenticated,
        };
        *cached = CachedReadiness {
            checked_at: Some(Instant::now()),
            state,
        };
        self.metrics.record_probe(&self.config.name, state);
        state
    }

    /// Read the provider's verdict from one credentialed `/models` request.
    async fn probe(&self, api_key: &SecretString) -> ProviderAdmissionState {
        let Some(status) = self.probe_status(self.models_url.clone(), api_key).await else {
            return ProviderAdmissionState::Unavailable;
        };
        match status {
            status if status.is_success() => ProviderAdmissionState::Ready,
            // A credential the provider rejects is an operator error, not an
            // outage, and must not be collapsed into it.
            status if is_provider_credential_rejection(status) => self.reject_credential().await,
            // `/models` is not uniformly implemented. These statuses prove the
            // endpoint answered, which is what readiness asks.
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::TOO_MANY_REQUESTS => {
                ProviderAdmissionState::Ready
            }
            StatusCode::NOT_FOUND => self.corroborate_missing_models(api_key).await,
            _ => ProviderAdmissionState::Unavailable,
        }
    }

    /// Decide what a 404 from `/models` means.
    ///
    /// A 404 is ambiguous, and the two readings are opposites. It can be a
    /// minimal provider that implements only its inference path, which must not
    /// be marked down. It can equally be an endpoint whose base path is wrong -
    /// `/v1beta/` where the provider serves `/v1/` - and then every path under
    /// it 404s. Reporting that `Ready` loses the only signal an operator gets:
    /// readiness does not gate admission, so the step is dispatched anyway and
    /// the 404 commits to the client, and 404 is not one of the statuses a route
    /// falls forward on.
    ///
    /// One GET against the configured inference path separates them. A provider
    /// that serves it answers *something* - 405 for the wrong method, 400, 401,
    /// or a real 200 - while a wrong base path 404s again. Note what this
    /// deliberately does not claim: a 404 says nothing about the credential,
    /// which is usually checked after routing, so `Ready` here means reachable,
    /// not authenticated.
    ///
    /// The cost is a provider whose framework answers 404 rather than 405 for an
    /// unrouted method: it reports unavailable, which is the behaviour before
    /// the 404 arm existed. That spends a health signal, not the traffic.
    async fn corroborate_missing_models(&self, api_key: &SecretString) -> ProviderAdmissionState {
        match self.probe_status(self.request_url.clone(), api_key).await {
            None | Some(StatusCode::NOT_FOUND) => ProviderAdmissionState::Unavailable,
            Some(status) if is_provider_credential_rejection(status) => {
                self.reject_credential().await
            }
            Some(status) if status.is_server_error() => ProviderAdmissionState::Unavailable,
            Some(_) => ProviderAdmissionState::Ready,
        }
    }

    /// Send one bounded, credentialed GET and report the status it answered.
    ///
    /// `None` is "no answer at all": a transport failure or the readiness
    /// deadline, both of which are outages rather than verdicts.
    async fn probe_status(&self, url: Url, api_key: &SecretString) -> Option<StatusCode> {
        let request = authorize_http(self.client.get(url), api_key, self.protocol);
        match tokio::time::timeout(
            Duration::from_millis(self.config.readiness_timeout_ms),
            request.send(),
        )
        .await
        {
            Ok(Ok(response)) => Some(response.status()),
            Ok(Err(_)) | Err(_) => None,
        }
    }

    /// Drop the cached credential the provider just refused, and say so.
    async fn reject_credential(&self) -> ProviderAdmissionState {
        self.credential.invalidate().await;
        ProviderAdmissionState::Unauthenticated
    }
}

impl CodexProvider {
    /// Discard cached readiness after a real dispatch proved it wrong.
    pub(super) async fn invalidate_readiness(&self, _status: Option<StatusCode>) {
        *self.readiness.lock().await = CachedReadiness::default();
    }

    pub(super) async fn readiness(&self) -> ProviderAdmissionState {
        if self.permits.available_permits() == 0 {
            return ProviderAdmissionState::Busy;
        }
        let mut cached = self.readiness.lock().await;
        let ttl = Duration::from_millis(self.config.readiness_ttl_ms);
        if let Some(state) = cached.reusable(Instant::now(), ttl) {
            return state;
        }
        let state = match codex::probe(
            &self.executable,
            &self.environment,
            Duration::from_millis(self.config.readiness_timeout_ms),
        )
        .await
        {
            Ok(()) => ProviderAdmissionState::Ready,
            // A CLI logged in with an API key instead of a ChatGPT
            // subscription, or one whose doctor output is not the contract
            // Octoroute checks, is an operator error rather than an outage.
            // Collapsing it into `Unavailable` maps it to the `unhealthy`
            // trigger, which is in the default fallback set, and every request
            // and its spend would spill silently to the next step.
            Err(codex::CodexAdapterError::NotChatGpt | codex::CodexAdapterError::Diagnostic) => {
                ProviderAdmissionState::Unauthenticated
            }
            // Spawn failures, timeouts, and workspace errors are transient.
            //
            // Listed rather than caught by `_`: `Unavailable` maps to the
            // `unhealthy` trigger, which is in the default fallback set, so a
            // future auth-shaped variant falling through here would silently
            // spill the traffic and its spend to the next provider. Naming them
            // makes the next variant a compile error instead.
            Err(
                codex::CodexAdapterError::Missing
                | codex::CodexAdapterError::Workspace
                | codex::CodexAdapterError::Timeout
                | codex::CodexAdapterError::OutputTooLarge
                | codex::CodexAdapterError::Process
                | codex::CodexAdapterError::Contract
                | codex::CodexAdapterError::Incompatible
                | codex::CodexAdapterError::Request(_),
            ) => ProviderAdmissionState::Unavailable,
        };
        *cached = CachedReadiness {
            checked_at: Some(Instant::now()),
            state,
        };
        self.metrics.record_probe(&self.config.name, state);
        state
    }
}

/// Apply the protocol's credential header to one outgoing provider request.
///
/// Shared by the readiness probe and the dispatch transport: two copies of this
/// mapping is two places to forget `anthropic-version` or to send a bearer
/// token to an endpoint that reads `x-api-key`.
pub(in crate::gateway::fabric) fn authorize_http(
    request: RequestBuilder,
    api_key: &SecretString,
    protocol: ProviderProtocol,
) -> RequestBuilder {
    match protocol {
        ProviderProtocol::OpenAi => request.bearer_auth(api_key.expose_secret()),
        ProviderProtocol::Anthropic => request
            .header("x-api-key", api_key.expose_secret())
            .header("anthropic-version", ANTHROPIC_VERSION),
    }
}
