//! Octoroute v3 OpenAI-compatible tiered inference fabric.
//!
//! Virtual routes select among local llama.cpp pools and isolated provider
//! adapters while preserving strict local-only privacy and pre-commit fallback.

pub mod cli;
pub mod gateway;
pub mod telemetry;
