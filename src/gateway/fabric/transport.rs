//! Pre-commit HTTP transport for local-pool and provider leases.

use super::{PoolLease, ProviderLease};
use crate::gateway::http_client::{authorized, build};
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
use reqwest::Client;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use thiserror::Error;
use tokio::sync::OwnedSemaphorePermit;

const OPENROUTER_TITLE: &str = "x-openrouter-title";

/// Testable v3 transport contract shared by local and HTTP-provider dispatch.
#[async_trait]
pub trait FabricUpstreamTransport: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Dispatch one selected local member and stop before client commitment.
    async fn local(&self, lease: PoolLease) -> Result<PreparedUpstreamResponse, Self::Error>;

    /// Dispatch one selected HTTP provider and stop before client commitment.
    async fn provider(&self, lease: ProviderLease)
    -> Result<PreparedUpstreamResponse, Self::Error>;
}

/// Production v3 transport using a held first-byte stream for every backend.
#[derive(Clone)]
pub struct FabricTransport {
    client: Client,
}

impl FabricTransport {
    pub fn new() -> Result<Self, FabricTransportError> {
        Ok(Self {
            client: build().map_err(FabricTransportError::HttpClient)?,
        })
    }
}

#[async_trait]
impl FabricUpstreamTransport for FabricTransport {
    type Error = FabricTransportError;

    async fn local(&self, lease: PoolLease) -> Result<PreparedUpstreamResponse, Self::Error> {
        let (chat_url, api_key, request_body, permit) = lease.into_transport_parts();
        let request = authorized(
            self.client
                .post(chat_url)
                .header(CONTENT_TYPE, "application/json")
                .body(request_body),
            api_key.as_ref(),
        )
        .build()
        .map_err(FabricTransportError::BuildRequest)?;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(FabricTransportError::Send)?;
        PendingUpstreamResponse::from_parts(response, permit)
            .prepare()
            .await
    }

    async fn provider(
        &self,
        lease: ProviderLease,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        let (chat_url, api_key, request_body, timeout, openrouter_profile, permit) =
            lease.into_transport_parts();
        let dispatch = async {
            let mut request = self
                .client
                .post(chat_url)
                .timeout(timeout)
                .header(CONTENT_TYPE, "application/json")
                .body(request_body);
            if openrouter_profile {
                request = request.header(OPENROUTER_TITLE, "Octoroute");
            }
            let request = authorized(request, Some(&api_key))
                .build()
                .map_err(FabricTransportError::BuildRequest)?;
            let response = self
                .client
                .execute(request)
                .await
                .map_err(FabricTransportError::Send)?;
            PendingUpstreamResponse::from_parts(response, permit)
                .prepare()
                .await
        };
        tokio::time::timeout(timeout, dispatch)
            .await
            .map_err(|_| FabricTransportError::ProviderTimeout)?
    }
}

/// Upstream response whose headers are available but body is not committed.
struct PendingUpstreamResponse {
    response: reqwest::Response,
    permit: OwnedSemaphorePermit,
}

impl PendingUpstreamResponse {
    fn from_parts(response: reqwest::Response, permit: OwnedSemaphorePermit) -> Self {
        Self { response, permit }
    }

    /// Buffer the first body chunk and convert the remainder to a held stream.
    async fn prepare(mut self) -> Result<PreparedUpstreamResponse, FabricTransportError> {
        let status = self.response.status();
        let headers = safe_response_headers(self.response.headers());
        let first = self
            .response
            .chunk()
            .await
            .map_err(FabricTransportError::ReadFirstChunk)?;
        let stream = HeldBodyStream {
            first,
            remaining: Box::pin(self.response.bytes_stream()),
            _permit: self.permit,
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
    /// Upstream status available before client commitment.
    pub fn status(&self) -> StatusCode {
        self.response.status()
    }

    /// Consume the wrapper and return the complete HTTP response.
    pub fn into_response(self) -> Response<Body> {
        self.response
    }
}

struct HeldBodyStream {
    first: Option<Bytes>,
    remaining: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    _permit: OwnedSemaphorePermit,
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
pub enum FabricTransportError {
    /// Shared client construction failed.
    #[error("could not build gateway HTTP client")]
    HttpClient(#[source] reqwest::Error),
    /// Request serialization or construction failed.
    #[error("could not build upstream request")]
    BuildRequest(#[source] reqwest::Error),
    /// Upstream connection failed before response headers.
    #[error("upstream request failed before response headers")]
    Send(#[source] reqwest::Error),
    /// Upstream body failed before the first client-visible byte.
    #[error("upstream response failed before the first body byte")]
    ReadFirstChunk(#[source] reqwest::Error),
    /// Configured provider deadline elapsed before the first body byte.
    #[error("provider timed out before the first body byte")]
    ProviderTimeout,
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
