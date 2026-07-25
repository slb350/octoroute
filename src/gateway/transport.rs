//! Shared HTTP transport with explicit pre-commit response state.

use crate::gateway::{
    config::GatewayConfig,
    http_client::{authorized, build},
    local::LocalLease,
    openrouter::OpenRouterRequest,
};
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderName, Response, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER},
    },
};
use bytes::Bytes;
use futures::Stream;
use reqwest::{Client, Request, RequestBuilder, Url};
use secrecy::SecretString;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit};

const LOCAL_CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";
const OPENROUTER_CHAT_COMPLETIONS_PATH: &str = "chat/completions";
const OPENROUTER_HEALTH_PATH: &str = "key";
const OPENROUTER_TITLE: &str = "x-openrouter-title";

/// Pooled, credential-isolated transport for local and cloud upstreams.
#[derive(Clone)]
pub struct GatewayTransport {
    client: Client,
    local_chat_url: Url,
    openrouter_chat_url: Url,
    openrouter_health_url: Url,
    local_api_key: Option<SecretString>,
    local_first_byte_timeout: Option<Duration>,
    openrouter_api_key: SecretString,
    openrouter_title: String,
    openrouter_health_ttl: Duration,
    openrouter_probe_timeout: Duration,
    openrouter_health: Arc<Mutex<Option<CachedOpenRouterHealth>>>,
}

#[derive(Debug, Clone, Copy)]
struct CachedOpenRouterHealth {
    checked_at: Instant,
    ready: bool,
}

/// Testable pre-commit transport contract used by gateway orchestration.
#[async_trait]
pub trait UpstreamTransport: Send + Sync {
    /// Concrete transport failure type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Dispatch locally and buffer the first upstream body chunk.
    async fn local(&self, lease: LocalLease) -> Result<PreparedUpstreamResponse, Self::Error>;

    /// Dispatch to OpenRouter and buffer the first upstream body chunk.
    async fn openrouter(
        &self,
        request: OpenRouterRequest,
    ) -> Result<PreparedUpstreamResponse, Self::Error>;

    /// Whether the configured OpenRouter credential is currently accepted.
    async fn openrouter_ready(&self) -> bool;
}

impl GatewayTransport {
    /// Build the shared rustls client and validated endpoint URLs.
    pub fn new(config: &GatewayConfig) -> Result<Self, GatewayTransportError> {
        let client = build().map_err(GatewayTransportError::HttpClient)?;
        let local_chat_url = endpoint_url(config.local().base_url(), LOCAL_CHAT_COMPLETIONS_PATH)?;
        let openrouter_chat_url = endpoint_url(
            config.openrouter().base_url(),
            OPENROUTER_CHAT_COMPLETIONS_PATH,
        )?;
        let openrouter_health_url =
            endpoint_url(config.openrouter().base_url(), OPENROUTER_HEALTH_PATH)?;

        Ok(Self {
            client,
            local_chat_url,
            openrouter_chat_url,
            openrouter_health_url,
            local_api_key: config.local().api_key().cloned(),
            local_first_byte_timeout: config
                .local()
                .first_byte_timeout_ms()
                .map(Duration::from_millis),
            openrouter_api_key: config.openrouter().api_key().clone(),
            openrouter_title: config.openrouter().app_title().to_string(),
            openrouter_health_ttl: Duration::from_millis(config.openrouter().health_cache_ttl_ms()),
            openrouter_probe_timeout: Duration::from_millis(config.openrouter().probe_timeout_ms()),
            openrouter_health: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn http_client(&self) -> Client {
        self.client.clone()
    }

    /// Build, but do not send, an OpenRouter request for inspection or dispatch.
    pub fn openrouter_request(
        &self,
        request: &OpenRouterRequest,
    ) -> Result<Request, GatewayTransportError> {
        self.openrouter_request_builder(request)
            .build()
            .map_err(GatewayTransportError::BuildRequest)
    }

    /// Build, but do not send, the authenticated OpenRouter key probe.
    pub fn openrouter_health_request(&self) -> Result<Request, GatewayTransportError> {
        authorized(
            self.client
                .get(self.openrouter_health_url.clone())
                .timeout(self.openrouter_probe_timeout),
            Some(&self.openrouter_api_key),
        )
        .build()
        .map_err(GatewayTransportError::BuildRequest)
    }

    async fn probe_openrouter(&self) -> bool {
        let mut cached = self.openrouter_health.lock().await;
        if let Some(value) = *cached
            && value.checked_at.elapsed() < self.openrouter_health_ttl
        {
            return value.ready;
        }
        let ready = match self.openrouter_health_request() {
            Ok(request) => self
                .client
                .execute(request)
                .await
                .is_ok_and(|response| response.status().is_success()),
            Err(_) => false,
        };
        *cached = Some(CachedOpenRouterHealth {
            checked_at: Instant::now(),
            ready,
        });
        ready
    }

    /// Dispatch one local request while retaining its capacity lease.
    pub async fn send_local(
        &self,
        lease: LocalLease,
    ) -> Result<PendingUpstreamResponse, GatewayTransportError> {
        let (body, permit) = lease.into_parts();
        let request = authorized(
            self.client
                .post(self.local_chat_url.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(body),
            self.local_api_key.as_ref(),
        )
        .build()
        .map_err(GatewayTransportError::BuildRequest)?;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(GatewayTransportError::Send)?;
        Ok(PendingUpstreamResponse {
            response,
            local_permit: Some(permit),
        })
    }

    /// Dispatch one prepared OpenRouter request.
    pub async fn send_openrouter(
        &self,
        request: OpenRouterRequest,
    ) -> Result<PendingUpstreamResponse, GatewayTransportError> {
        let http_request = self.openrouter_request(&request)?;
        drop(request);
        let response = self
            .client
            .execute(http_request)
            .await
            .map_err(GatewayTransportError::Send)?;
        Ok(PendingUpstreamResponse {
            response,
            local_permit: None,
        })
    }

    fn openrouter_request_builder(&self, request: &OpenRouterRequest) -> RequestBuilder {
        authorized(
            self.client
                .post(self.openrouter_chat_url.clone())
                .header(CONTENT_TYPE, "application/json")
                .header(OPENROUTER_TITLE, &self.openrouter_title)
                .json(request.body()),
            Some(&self.openrouter_api_key),
        )
    }
}

#[async_trait]
impl UpstreamTransport for GatewayTransport {
    type Error = GatewayTransportError;

    async fn local(&self, lease: LocalLease) -> Result<PreparedUpstreamResponse, Self::Error> {
        let dispatch = async { self.send_local(lease).await?.prepare().await };
        match self.local_first_byte_timeout {
            Some(timeout) => tokio::time::timeout(timeout, dispatch)
                .await
                .map_err(|_| GatewayTransportError::FirstByteTimeout)?,
            None => dispatch.await,
        }
    }

    async fn openrouter(
        &self,
        request: OpenRouterRequest,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        self.send_openrouter(request).await?.prepare().await
    }

    async fn openrouter_ready(&self) -> bool {
        self.probe_openrouter().await
    }
}

/// Upstream response whose headers are available but body is not committed.
pub struct PendingUpstreamResponse {
    response: reqwest::Response,
    local_permit: Option<OwnedSemaphorePermit>,
}

impl PendingUpstreamResponse {
    /// Upstream HTTP status available before reading a body byte.
    pub fn status(&self) -> StatusCode {
        self.response.status()
    }

    /// Buffer the first body chunk and convert the remainder to a held stream.
    pub async fn prepare(mut self) -> Result<PreparedUpstreamResponse, GatewayTransportError> {
        let status = self.response.status();
        let headers = safe_response_headers(self.response.headers());
        let first = self
            .response
            .chunk()
            .await
            .map_err(GatewayTransportError::ReadFirstChunk)?;
        let stream = HeldBodyStream {
            first,
            remaining: Box::pin(self.response.bytes_stream()),
            _local_permit: self.local_permit,
        };
        let mut response = Response::new(Body::from_stream(stream));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        Ok(PreparedUpstreamResponse { response })
    }
}

/// Client-ready upstream response with safe headers and a streaming body.
pub struct PreparedUpstreamResponse {
    response: Response<Body>,
}

impl PreparedUpstreamResponse {
    /// Upstream status.
    pub fn status(&self) -> StatusCode {
        self.response.status()
    }

    /// Strictly allowlisted response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.response.headers()
    }

    /// Consume the wrapper and return the streaming body.
    pub fn into_body(self) -> Body {
        self.response.into_body()
    }

    /// Consume the wrapper and return the complete HTTP response.
    pub fn into_response(self) -> Response<Body> {
        self.response
    }

    #[cfg(test)]
    pub(super) fn from_response(response: Response<Body>) -> Self {
        Self { response }
    }
}

struct HeldBodyStream {
    first: Option<Bytes>,
    remaining: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    _local_permit: Option<OwnedSemaphorePermit>,
}

impl Stream for HeldBodyStream {
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(first) = self.first.take() {
            return Poll::Ready(Some(Ok(first)));
        }
        self.remaining.as_mut().poll_next(context)
    }
}

/// Safe transport construction and pre-commit I/O failures.
#[derive(Debug, Error)]
pub enum GatewayTransportError {
    /// Shared client construction failed.
    #[error("could not build gateway HTTP client")]
    HttpClient(#[source] reqwest::Error),
    /// A configured base URL could not resolve chat completions.
    #[error("could not resolve the chat-completions endpoint")]
    InvalidUrl,
    /// Request serialization or construction failed.
    #[error("could not build upstream request")]
    BuildRequest(#[source] reqwest::Error),
    /// Upstream connection failed before response headers.
    #[error("upstream request failed before response headers")]
    Send(#[source] reqwest::Error),
    /// Upstream body failed before the first client-visible byte.
    #[error("upstream response failed before the first body byte")]
    ReadFirstChunk(#[source] reqwest::Error),
    /// Configured local deadline elapsed before the first body byte.
    #[error("local upstream timed out before the first body byte")]
    FirstByteTimeout,
}

fn endpoint_url(base: &Url, path: &str) -> Result<Url, GatewayTransportError> {
    base.join(path)
        .map_err(|_| GatewayTransportError::InvalidUrl)
}

fn safe_response_headers(source: &HeaderMap) -> HeaderMap {
    let allowed = [
        CONTENT_TYPE,
        CACHE_CONTROL,
        RETRY_AFTER,
        HeaderName::from_static("x-request-id"),
        HeaderName::from_static("x-generation-id"),
        HeaderName::from_static("openai-request-id"),
        HeaderName::from_static("x-ratelimit-limit"),
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderName::from_static("x-ratelimit-reset"),
        HeaderName::from_static("x-ratelimit-limit-requests"),
        HeaderName::from_static("x-ratelimit-limit-tokens"),
        HeaderName::from_static("x-ratelimit-remaining-requests"),
        HeaderName::from_static("x-ratelimit-remaining-tokens"),
        HeaderName::from_static("x-ratelimit-reset-requests"),
        HeaderName::from_static("x-ratelimit-reset-tokens"),
    ];
    let mut headers = HeaderMap::new();
    for name in allowed {
        for value in source.get_all(&name) {
            headers.append(name.clone(), value.clone());
        }
    }
    headers
}
