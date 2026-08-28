//! Translation between OpenAI chat completions and Anthropic Messages.
//!
//! [`request`] builds an Anthropic Messages request from a validated OpenAI
//! chat-completion body; [`response`] translates buffered and streamed Anthropic
//! responses back into OpenAI shape.

mod request;
mod response;

#[cfg(test)]
mod tests;

pub(super) use request::build_request;
pub(super) use response::{AnthropicSseTranslator, open_ai_error_body, translate_message_response};

use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

/// Count of Anthropic content blocks, SSE events, and deltas Octoroute did not
/// recognize and skipped. Forward compatibility is deliberate: a provider adding
/// a block or event type must not truncate a committed stream. The counter is
/// unlabeled so it cannot carry provider or prompt-derived values.
static IGNORED_UNKNOWN_TYPES: AtomicU64 = AtomicU64::new(0);

/// Skip one unrecognized Anthropic type, recording it for `/metrics`.
fn ignore_unknown_type() {
    IGNORED_UNKNOWN_TYPES.fetch_add(1, Ordering::Relaxed);
}

/// Read the ignored-unknown-type counter for the Prometheus registry.
pub(super) fn ignored_unknown_types() -> u64 {
    IGNORED_UNKNOWN_TYPES.load(Ordering::Relaxed)
}

#[derive(Debug, Error)]
pub(crate) enum AnthropicAdapterError {
    #[error("request is incompatible with the Anthropic adapter ({0})")]
    Incompatible(&'static str),
    #[error("could not serialize an Anthropic-compatible payload")]
    Serialization,
    #[error("Anthropic-compatible provider returned an invalid response")]
    Response,
    #[error(transparent)]
    Request(#[from] crate::gateway::request::GatewayRequestError),
}

impl AnthropicAdapterError {
    pub(super) const fn is_incompatible(&self) -> bool {
        matches!(self, Self::Incompatible(_))
    }
}
