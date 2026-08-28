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

use crate::gateway::fabric::unknown_types;
use thiserror::Error;

/// Skip one unrecognized Anthropic type, recording it for `/metrics`.
fn ignore_unknown_type() {
    unknown_types::record(unknown_types::Adapter::Anthropic);
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
