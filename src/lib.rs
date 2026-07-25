//! Octoroute v2 local-first OpenAI-compatible gateway.
//!
//! The gateway sends compatible work to one local llama.cpp service and routes
//! everything else to OpenRouter while preserving explicit local-only privacy.

pub mod cli;
pub mod gateway;
pub mod telemetry;
