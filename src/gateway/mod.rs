//! Octoroute v2 local/cloud gateway and v3 inference-fabric foundations.

pub mod auth;
pub mod config;
pub mod env;
pub mod fabric;
pub mod http;
mod http_client;
pub(crate) mod intelligence;
pub mod local;
pub mod metrics;
pub mod openrouter;
pub mod request;
pub mod routing;
pub(crate) mod sampling;
pub mod service;
pub(crate) mod session_latch;
pub(crate) mod trajectory;
pub mod transport;

#[cfg(test)]
mod auth_tests;
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod env_tests;
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod intelligence_tests;
#[cfg(test)]
mod local_tests;
#[cfg(test)]
mod metrics_tests;
#[cfg(test)]
mod openrouter_tests;
#[cfg(test)]
mod request_tests;
#[cfg(test)]
mod routing_policy_tests;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod sampling_tests;
#[cfg(test)]
mod service_limits_tests;
#[cfg(test)]
mod service_security_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod session_latch_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod trajectory_tests;
#[cfg(test)]
mod transport_tests;
