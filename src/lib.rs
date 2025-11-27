//! Octoroute - Intelligent multi-model router for self-hosted LLMs
//!
//! This library provides intelligent routing between multiple local LLM endpoints
//! based on task complexity, importance, and resource availability.

pub mod cli;
pub mod config;
pub mod error;
pub mod handlers;
pub mod metrics;
pub mod middleware;
pub mod models;
pub mod router;
pub mod shared;
pub mod telemetry;
