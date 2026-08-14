//! Octoroute v2 local/cloud gateway.

pub mod auth;
pub mod config;
pub mod env;
pub mod http;
mod http_client;
pub(crate) mod intelligence;
pub mod local;
pub mod metrics;
pub mod openrouter;
pub mod request;
pub mod routing;
pub mod service;
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
mod service_limits_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod transport_tests;
