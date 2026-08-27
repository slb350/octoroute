//! Shared pre-commit HTTP transport for v3 local-pool and provider leases.

use super::{PoolLease, ProviderLease};
use crate::gateway::{
    http_client::{authorized, build},
    transport::{GatewayTransportError, PendingUpstreamResponse, PreparedUpstreamResponse},
};
use async_trait::async_trait;
use reqwest::{Client, header::CONTENT_TYPE};

const OPENROUTER_TITLE: &str = "x-openrouter-title";

/// Testable v3 transport contract shared by local and HTTP-provider dispatch.
#[async_trait]
pub trait FabricUpstreamTransport: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Dispatch one selected local member and stop before client commitment.
    async fn local(&self, lease: PoolLease) -> Result<PreparedUpstreamResponse, Self::Error>;

    /// Dispatch one selected HTTP provider and stop before client commitment.
    async fn provider(&self, lease: ProviderLease)
    -> Result<PreparedUpstreamResponse, Self::Error>;
}

/// Production v3 transport using a held first-byte stream for every backend.
#[derive(Clone)]
pub struct FabricTransport {
    client: Client,
}

impl FabricTransport {
    pub fn new() -> Result<Self, GatewayTransportError> {
        Ok(Self {
            client: build().map_err(GatewayTransportError::HttpClient)?,
        })
    }
}

#[async_trait]
impl FabricUpstreamTransport for FabricTransport {
    type Error = GatewayTransportError;

    async fn local(&self, lease: PoolLease) -> Result<PreparedUpstreamResponse, Self::Error> {
        let (chat_url, api_key, request_body, permit) = lease.into_transport_parts();
        let request = authorized(
            self.client
                .post(chat_url)
                .header(CONTENT_TYPE, "application/json")
                .body(request_body),
            api_key.as_ref(),
        )
        .build()
        .map_err(GatewayTransportError::BuildRequest)?;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(GatewayTransportError::Send)?;
        PendingUpstreamResponse::from_parts(response, Some(permit))
            .prepare()
            .await
    }

    async fn provider(
        &self,
        lease: ProviderLease,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        let (chat_url, api_key, request_body, timeout, openrouter_profile, permit) =
            lease.into_transport_parts();
        let dispatch = async {
            let mut request = self
                .client
                .post(chat_url)
                .timeout(timeout)
                .header(CONTENT_TYPE, "application/json")
                .body(request_body);
            if openrouter_profile {
                request = request.header(OPENROUTER_TITLE, "Octoroute");
            }
            let request = authorized(request, Some(&api_key))
                .build()
                .map_err(GatewayTransportError::BuildRequest)?;
            let response = self
                .client
                .execute(request)
                .await
                .map_err(GatewayTransportError::Send)?;
            PendingUpstreamResponse::from_parts(response, Some(permit))
                .prepare()
                .await
        };
        tokio::time::timeout(timeout, dispatch)
            .await
            .map_err(|_| GatewayTransportError::ProviderTimeout)?
    }
}
