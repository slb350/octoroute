//! OpenAI-compatible API handlers
//!
//! Provides OpenAI-compatible endpoints for Octoroute:
//! - `POST /v1/chat/completions` - Chat completions with SSE streaming
//! - `GET /v1/models` - List available models

pub mod completions;
pub mod models;
pub mod streaming;
pub mod types;
