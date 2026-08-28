//! Pre-commit transport for local, HTTP-provider, and Codex CLI leases.

use super::{
    PoolLease, ProviderLease,
    anthropic::{self, AnthropicSseTranslator},
    codex,
    provider::{HttpProviderDispatch, ProviderDispatch, ProviderHttpAdapter},
};
use crate::gateway::http_client::{authorized, build};
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderName, HeaderValue, Response, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER},
    },
};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::Client;
use secrecy::ExposeSecret;
use std::{
    collections::VecDeque,
    convert::Infallible,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use thiserror::Error;
use tokio::sync::OwnedSemaphorePermit;

const OPENROUTER_TITLE: &str = "x-openrouter-title";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TRANSLATED_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Testable v3 transport contract shared by every backend.
#[async_trait]
pub trait FabricUpstreamTransport: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Dispatch one selected local member and stop before client commitment.
    async fn local(&self, lease: PoolLease) -> Result<PreparedUpstreamResponse, Self::Error>;

    /// Dispatch one selected provider and stop before client commitment.
    async fn provider(&self, lease: ProviderLease)
    -> Result<PreparedUpstreamResponse, Self::Error>;
}

/// Production v3 transport with held-first-byte semantics for every backend.
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
        let (chat_url, api_key, request_body, timeout, permit) = lease.into_transport_parts();
        let request = authorized(
            self.client
                .post(chat_url)
                .timeout(timeout)
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
        PendingUpstreamResponse::new(response, permit)
            .prepare_passthrough()
            .await
    }

    async fn provider(
        &self,
        lease: ProviderLease,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        let (dispatch, timeout, permit) = lease.into_transport_parts();
        let operation = async {
            match dispatch {
                ProviderDispatch::Http(dispatch) => {
                    dispatch_http(&self.client, dispatch, timeout, permit).await
                }
                ProviderDispatch::Codex(request) => {
                    let stream = request.stream;
                    let bytes = codex::execute(request)
                        .await
                        .map_err(|_| FabricTransportError::Codex)?;
                    Ok(PreparedUpstreamResponse::from_bytes(
                        StatusCode::OK,
                        if stream {
                            "text/event-stream"
                        } else {
                            "application/json"
                        },
                        bytes,
                        permit,
                    ))
                }
            }
        };
        tokio::time::timeout(timeout, operation)
            .await
            .map_err(|_| FabricTransportError::ProviderTimeout)?
    }
}

async fn dispatch_http(
    client: &Client,
    dispatch: HttpProviderDispatch,
    timeout: Duration,
    permit: OwnedSemaphorePermit,
) -> Result<PreparedUpstreamResponse, FabricTransportError> {
    let mut request = client
        .post(dispatch.url)
        .timeout(timeout)
        .header(CONTENT_TYPE, "application/json")
        .body(dispatch.body);
    request = match dispatch.adapter {
        ProviderHttpAdapter::OpenAi => request.bearer_auth(dispatch.api_key.expose_secret()),
        ProviderHttpAdapter::Anthropic { .. } => request
            .header("x-api-key", dispatch.api_key.expose_secret())
            .header("anthropic-version", ANTHROPIC_VERSION),
    };
    if dispatch.openrouter_profile {
        request = request.header(OPENROUTER_TITLE, "Octoroute");
    }
    let request = request
        .build()
        .map_err(FabricTransportError::BuildRequest)?;
    let response = client
        .execute(request)
        .await
        .map_err(FabricTransportError::Send)?;
    match dispatch.adapter {
        ProviderHttpAdapter::OpenAi => {
            PendingUpstreamResponse::new(response, permit)
                .prepare_passthrough()
                .await
        }
        ProviderHttpAdapter::Anthropic { stream } => {
            prepare_anthropic(response, &dispatch.model, stream, permit).await
        }
    }
}

/// Upstream response whose headers are available but body is not committed.
struct PendingUpstreamResponse {
    response: reqwest::Response,
    permit: OwnedSemaphorePermit,
}

impl PendingUpstreamResponse {
    fn new(response: reqwest::Response, permit: OwnedSemaphorePermit) -> Self {
        Self { response, permit }
    }

    async fn prepare_passthrough(
        mut self,
    ) -> Result<PreparedUpstreamResponse, FabricTransportError> {
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

async fn prepare_anthropic(
    response: reqwest::Response,
    model: &str,
    stream: bool,
    permit: OwnedSemaphorePermit,
) -> Result<PreparedUpstreamResponse, FabricTransportError> {
    let status = response.status();
    if !status.is_success() {
        let code = match status {
            StatusCode::TOO_MANY_REQUESTS => "provider_rate_limited",
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "provider_authentication_failed",
            _ if status.is_server_error() => "provider_server_error",
            _ => "provider_request_failed",
        };
        return Ok(PreparedUpstreamResponse::from_bytes(
            status,
            "application/json",
            anthropic::open_ai_error_body(code),
            permit,
        ));
    }
    if stream {
        return prepare_anthropic_stream(response, model, permit).await;
    }
    let body = read_bounded(response, MAX_TRANSLATED_RESPONSE_BYTES).await?;
    let body = anthropic::translate_message_response(&body, model)
        .map_err(|_| FabricTransportError::InvalidProviderResponse)?;
    Ok(PreparedUpstreamResponse::from_bytes(
        status,
        "application/json",
        body,
        permit,
    ))
}

async fn prepare_anthropic_stream(
    response: reqwest::Response,
    model: &str,
    permit: OwnedSemaphorePermit,
) -> Result<PreparedUpstreamResponse, FabricTransportError> {
    let status = response.status();
    let mut headers = safe_response_headers(response.headers());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    let mut state = AnthropicStreamState {
        upstream: Box::pin(response.bytes_stream()),
        translator: AnthropicSseTranslator::new(model),
        queued: VecDeque::new(),
        finished: false,
        _permit: permit,
    };
    let first = match state.next_output().await {
        Some(Ok(bytes)) => bytes,
        Some(Err(_)) | None => return Err(FabricTransportError::InvalidProviderResponse),
    };
    let remaining = futures::stream::unfold(state, |mut state| async move {
        state.next_output().await.map(|item| (item, state))
    });
    let stream = futures::stream::once(async move { Ok::<_, io::Error>(first) }).chain(remaining);
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(PreparedUpstreamResponse { response })
}

struct AnthropicStreamState {
    upstream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    translator: AnthropicSseTranslator,
    queued: VecDeque<Bytes>,
    finished: bool,
    _permit: OwnedSemaphorePermit,
}

impl AnthropicStreamState {
    async fn next_output(&mut self) -> Option<Result<Bytes, io::Error>> {
        loop {
            if let Some(bytes) = self.queued.pop_front() {
                return Some(Ok(bytes));
            }
            if self.finished {
                return None;
            }
            match self.upstream.next().await {
                Some(Ok(bytes)) => match self.translator.push(&bytes) {
                    Ok(output) => self.queued.extend(output),
                    Err(_) => {
                        self.finished = true;
                        return Some(Err(io::Error::other("invalid translated provider stream")));
                    }
                },
                Some(Err(_)) => {
                    self.finished = true;
                    return Some(Err(io::Error::other("provider stream failed")));
                }
                None => {
                    self.finished = true;
                    match self.translator.finish() {
                        Ok(output) => self.queued.extend(output),
                        Err(_) => {
                            return Some(Err(io::Error::other(
                                "provider stream ended before completion",
                            )));
                        }
                    }
                }
            }
        }
    }
}

async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, FabricTransportError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(FabricTransportError::ReadFirstChunk)?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(FabricTransportError::ProviderResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

/// Client-ready upstream response with safe headers and a held body.
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

    fn from_bytes(
        status: StatusCode,
        content_type: &'static str,
        bytes: Bytes,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        let stream = HeldBytesStream {
            bytes: Some(bytes),
            _permit: permit,
        };
        let mut response = Response::new(Body::from_stream(stream));
        *response.status_mut() = status;
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        Self { response }
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

struct HeldBytesStream {
    bytes: Option<Bytes>,
    _permit: OwnedSemaphorePermit,
}

impl Stream for HeldBytesStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.bytes.take().map(Ok))
    }
}

/// Safe transport construction and pre-commit I/O failures.
#[derive(Debug, Error)]
pub enum FabricTransportError {
    #[error("could not build gateway HTTP client")]
    HttpClient(#[source] reqwest::Error),
    #[error("could not build upstream request")]
    BuildRequest(#[source] reqwest::Error),
    #[error("upstream request failed before response headers")]
    Send(#[source] reqwest::Error),
    #[error("upstream response failed before the first body byte")]
    ReadFirstChunk(#[source] reqwest::Error),
    #[error("configured provider deadline elapsed before the first body byte")]
    ProviderTimeout,
    #[error("provider response exceeded its configured bound")]
    ProviderResponseTooLarge,
    #[error("provider returned an invalid response before commitment")]
    InvalidProviderResponse,
    #[error("Codex CLI failed before response commitment")]
    Codex,
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
