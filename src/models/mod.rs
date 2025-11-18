//! Model client and selection logic
//!
//! Provides wrappers around `open-agent-sdk` clients and intelligent
//! model selection from multi-model configuration.

pub mod client;
pub mod selector;

pub use client::ModelClient;
pub use selector::ModelSelector;
