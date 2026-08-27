use crate::gateway::metrics::ResponseObservation;
use axum::{
    body::{Body, BodyDataStream},
    http::{HeaderMap, Response},
};
use futures::Stream;
use prometheus::IntCounter;
use std::{
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::OwnedSemaphorePermit;

pub(crate) fn header_bytes(headers: &HeaderMap) -> usize {
    headers.iter().fold(0usize, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
            .saturating_add(4)
    })
}

pub(crate) fn hold_response_guard(
    response: Response<Body>,
    guard: OwnedSemaphorePermit,
) -> Response<Body> {
    let (parts, body) = response.into_parts();
    let stream = GuardedResponseBody {
        inner: body.into_data_stream(),
        _guard: guard,
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

pub(crate) fn observe_response_body(
    response: Response<Body>,
    mid_stream_failures: Option<IntCounter>,
    response_observation: Option<ResponseObservation>,
) -> Response<Body> {
    let (parts, body) = response.into_parts();
    let stream = ObservedResponseBody {
        inner: body.into_data_stream(),
        mid_stream_failures,
        failure_recorded: false,
        _response_observation: response_observation,
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

struct GuardedResponseBody {
    inner: BodyDataStream,
    _guard: OwnedSemaphorePermit,
}

struct ObservedResponseBody {
    inner: BodyDataStream,
    mid_stream_failures: Option<IntCounter>,
    failure_recorded: bool,
    _response_observation: Option<ResponseObservation>,
}

impl Stream for ObservedResponseBody {
    type Item = Result<bytes::Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = Pin::new(&mut self.inner).poll_next(context);
        if matches!(result, Poll::Ready(Some(Err(_)))) && !self.failure_recorded {
            if let Some(counter) = &self.mid_stream_failures {
                counter.inc();
            }
            self.failure_recorded = true;
        }
        result
    }
}

impl Stream for GuardedResponseBody {
    type Item = Result<bytes::Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(context)
    }
}

pub(crate) struct FixedWindowRateLimiter {
    limit: u32,
    state: Mutex<RateWindow>,
}

struct RateWindow {
    started: Instant,
    requests: u32,
}

impl FixedWindowRateLimiter {
    pub(crate) fn new(limit: u32) -> Self {
        Self {
            limit,
            state: Mutex::new(RateWindow {
                started: Instant::now(),
                requests: 0,
            }),
        }
    }

    pub(crate) fn allow(&self) -> bool {
        let mut state = self.state.lock().expect("rate limiter mutex poisoned");
        if state.started.elapsed() >= Duration::from_secs(60) {
            state.started = Instant::now();
            state.requests = 0;
        }
        if state.requests >= self.limit {
            false
        } else {
            state.requests += 1;
            true
        }
    }
}
