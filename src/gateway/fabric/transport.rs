//! Pre-commit transport for local, HTTP-provider, and Codex CLI leases.

use super::{
    PoolLease, ProviderLease,
    anthropic::{self, AnthropicSseTranslator},
    bounded_response::{self, BoundedResponseError},
    codex,
    provider::{
        HttpProviderDispatch, ProviderDispatch, ProviderHttpAdapter, authorize_http,
        is_provider_credential_rejection,
    },
};
use crate::gateway::http_client::authorized;
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
use std::{
    collections::VecDeque,
    convert::Infallible,
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use thiserror::Error;
use tokio::sync::OwnedSemaphorePermit;

const OPENROUTER_TITLE: &str = "x-openrouter-title";
/// Bound on a provider response Octoroute has to buffer whole before it can
/// translate it.
pub(super) const MAX_TRANSLATED_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Bound on an upstream error body.
///
/// `open_ai_error_body` keeps at most `MAX_ERROR_MESSAGE_BYTES` of it, and this
/// path is taken by every request during an outage, so reading it under the
/// 16 MiB success bound would buy nothing and cost a full read of whatever error
/// page the provider emits.
const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod mutation_tests {
    use super::*;

    #[test]
    fn translated_and_error_response_bounds_have_the_exact_byte_budgets() {
        assert_eq!(MAX_TRANSLATED_RESPONSE_BYTES, 16_777_216);
        assert_eq!(MAX_ERROR_RESPONSE_BYTES, 65_536);
    }
}

/// The two deadlines one upstream attempt is bounded by.
///
/// `total` covers the complete response and is legitimately long. `first_byte`
/// bounds how long a hung upstream may hold the member and inbound permits
/// before the route falls forward; it is `None` when the operator has not
/// measured one, and Octoroute then invents no deadline.
#[derive(Debug, Clone, Copy)]
pub struct UpstreamDeadlines {
    pub total: Duration,
    pub first_byte: Option<Duration>,
}

impl UpstreamDeadlines {
    pub(super) fn new(total_ms: u64, first_byte_ms: Option<u64>) -> Self {
        Self {
            total: Duration::from_millis(total_ms),
            first_byte: first_byte_ms.map(Duration::from_millis),
        }
    }

    /// Await `operation` under the first-byte deadline, if one is configured.
    pub(super) async fn hold_first_byte<T>(
        self,
        operation: impl Future<Output = Result<T, FabricTransportError>>,
    ) -> Result<T, FabricTransportError> {
        match self.first_byte {
            Some(deadline) => tokio::time::timeout(deadline, operation)
                .await
                .map_err(|_| FabricTransportError::FirstByteTimeout)?,
            None => operation.await,
        }
    }
}

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
    /// Build a transport over the gateway's one pooled client.
    ///
    /// The only constructor: every upstream shares one client, so a transport
    /// that builds its own would defeat the shared connection and TLS session
    /// pool.
    pub(crate) fn with_client(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl FabricUpstreamTransport for FabricTransport {
    type Error = FabricTransportError;

    async fn local(&self, lease: PoolLease) -> Result<PreparedUpstreamResponse, Self::Error> {
        let (chat_url, api_key, request_body, deadlines, permit) = lease.into_transport_parts();
        let request = authorized(
            self.client
                .post(chat_url)
                .timeout(deadlines.total)
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
        reject_redirect(&response)?;
        deadlines
            .hold_first_byte(PendingUpstreamResponse::new(response, permit).prepare_passthrough())
            .await
    }

    async fn provider(
        &self,
        lease: ProviderLease,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        let (dispatch, deadlines, permit) = lease.into_transport_parts();
        let operation = async {
            match dispatch {
                ProviderDispatch::Http(dispatch) => {
                    dispatch_http(&self.client, dispatch, deadlines, permit).await
                }
                ProviderDispatch::Codex(request) => {
                    let stream = request.stream;
                    // The CLI answers in one final message, so its first
                    // client-visible byte is the whole run: `first_byte_timeout_ms`
                    // bounds how long a stuck `codex exec` may hold the provider
                    // and inbound permits before the route falls forward. Without
                    // this the setting is accepted for a `codex_cli` provider and
                    // silently ignored.
                    let bytes = deadlines
                        .hold_first_byte(async {
                            codex::execute(request)
                                .await
                                .map_err(|error| FabricTransportError::Codex(Box::new(error)))
                        })
                        .await?;
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
        tokio::time::timeout(deadlines.total, operation)
            .await
            .map_err(|_| FabricTransportError::ProviderTimeout)?
    }
}

async fn dispatch_http(
    client: &Client,
    dispatch: HttpProviderDispatch,
    deadlines: UpstreamDeadlines,
    permit: OwnedSemaphorePermit,
) -> Result<PreparedUpstreamResponse, FabricTransportError> {
    let mut request = client
        .post(dispatch.url)
        .timeout(deadlines.total)
        .header(CONTENT_TYPE, "application/json")
        .body(dispatch.body);
    // The same mapping the readiness probe uses; see `provider::authorize_http`.
    request = authorize_http(request, &dispatch.api_key, dispatch.adapter.protocol());
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
    reject_redirect(&response)?;
    match dispatch.adapter {
        ProviderHttpAdapter::OpenAi => {
            deadlines
                .hold_first_byte(
                    PendingUpstreamResponse::new(response, permit).prepare_passthrough(),
                )
                .await
        }
        ProviderHttpAdapter::Anthropic { stream } => {
            // Wider than passthrough on purpose. A non-streaming Anthropic
            // response has to be read in full before any of it can be
            // translated, so there is no client-visible first byte until the
            // whole upstream body has arrived; the deadline therefore bounds
            // the complete read. Set it from a measured full-response time for
            // an Anthropic provider, not from a time-to-first-token.
            deadlines
                .hold_first_byte(prepare_anthropic(response, &dispatch.model, stream, permit))
                .await
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
            status if is_provider_credential_rejection(status) => "provider_authentication_failed",
            _ if status.is_server_error() => "provider_server_error",
            _ => "provider_request_failed",
        };
        let upstream = read_bounded(response, MAX_ERROR_RESPONSE_BYTES)
            .await
            .unwrap_or_default();
        let body = anthropic::open_ai_error_body(code, &upstream);
        tracing::warn!(
            status = status.as_u16(),
            code,
            model,
            "anthropic provider returned an error response"
        );
        return Ok(PreparedUpstreamResponse::from_bytes(
            status,
            "application/json",
            body,
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

#[cfg(test)]
pub(super) async fn prepare_anthropic_for_test(
    response: reqwest::Response,
    model: &str,
    stream: bool,
    permit: OwnedSemaphorePermit,
) -> Result<PreparedUpstreamResponse, FabricTransportError> {
    prepare_anthropic(response, model, stream, permit).await
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

/// Read a whole upstream body under `limit`, distinguishing the two ways it can
/// fail.
pub(super) async fn read_bounded(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, FabricTransportError> {
    bounded_response::read(response, limit)
        .await
        .map_err(|error| match error {
            // Only a read failure before any byte arrived is a first-byte
            // failure. A truncated response has a different operator cause.
            BoundedResponseError::Read {
                source,
                after_body: false,
            } => FabricTransportError::ReadFirstChunk(source),
            BoundedResponseError::Read {
                source,
                after_body: true,
            } => FabricTransportError::ReadBody(source),
            BoundedResponseError::TooLarge => FabricTransportError::ProviderResponseTooLarge,
        })
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
    #[error("upstream response failed part-way through its body")]
    ReadBody(#[source] reqwest::Error),
    #[error("configured provider deadline elapsed before the first body byte")]
    ProviderTimeout,
    #[error("provider response exceeded its configured bound")]
    ProviderResponseTooLarge,
    #[error("provider returned an invalid response before commitment")]
    InvalidProviderResponse,
    #[error("Codex CLI failed before response commitment")]
    Codex(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("upstream answered with a redirect to an unconfigured host")]
    UnexpectedRedirect,
    #[error("configured first-byte deadline elapsed before the response committed")]
    FirstByteTimeout,
}

/// Refuse a 3xx before any adapter treats it as a real answer.
///
/// The shared client does not follow redirects, so a 3xx means the configured
/// endpoint is pointing somewhere the operator did not configure. It is a
/// pre-commit failure, which lets the route fall forward to its next step.
fn reject_redirect(response: &reqwest::Response) -> Result<(), FabricTransportError> {
    if response.status().is_redirection() {
        tracing::warn!(
            status = response.status().as_u16(),
            "upstream answered with a redirect; refusing to follow it"
        );
        return Err(FabricTransportError::UnexpectedRedirect);
    }
    Ok(())
}

/// Rebuild the client-visible response headers from a fixed allowlist.
///
/// `x-request-id` is deliberately absent. The gateway sets its own on every
/// response and only when the header is not already present, so forwarding the
/// upstream's would let an upstream claim the gateway's correlation id.
/// Upstream identifiers still reach the client through `x-generation-id` and
/// `openai-request-id`.
pub(super) fn safe_response_headers(source: &HeaderMap) -> HeaderMap {
    let allowed = [
        CONTENT_TYPE,
        CACHE_CONTROL,
        RETRY_AFTER,
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
