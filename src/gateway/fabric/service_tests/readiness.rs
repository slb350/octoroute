//! The readiness snapshot cache.
//!
//! `/health/ready` and `/health` are unauthenticated by contract and a
//! readiness pass spawns `codex doctor` and sends credentialed `/models`
//! probes. The snapshot cache is what stops an anonymous caller turning one
//! cheap request into that work, as fast as it can issue them.

use super::*;
use crate::gateway::fabric::{PoolAdmissionState, fabric_gateway_app};
use tower::ServiceExt as _;
use wiremock::matchers::any;

#[tokio::test]
async fn an_orphan_ready_target_cannot_make_unservable_routes_ready() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(0)
        .mount(&server)
        .await;
    let mut config = single_enabled_provider_config(&server, "zai");
    config
        .local_pools
        .get_mut("workers")
        .expect("workers pool")
        .enabled = false;
    for route in config.routes.values_mut() {
        route.steps = vec![RouteTarget::LocalPool("workers".to_string())];
    }
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");

    let readiness = service.readiness().await;

    assert!(!readiness.is_ready());
    assert!(!readiness.providers().contains_key("zai"));
    assert_eq!(
        readiness.pools().get("workers"),
        Some(&PoolAdmissionState::Disabled)
    );
}

#[tokio::test]
async fn repeated_anonymous_readiness_requests_probe_the_fleet_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer zai-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&server)
        .await;

    let mut config = single_enabled_provider_config(&server, "zai");
    // The provider keeps its own readiness answer for `readiness_ttl_ms`. Zero
    // here retires that cache immediately, so the snapshot cache is the only
    // thing that can hold the probe count at one - without this the test would
    // pass on the provider TTL alone and say nothing about the guard it names.
    config
        .providers
        .get_mut("zai")
        .expect("zai provider")
        .readiness_ttl_ms = 0;
    let app = fabric_gateway_app(
        FabricGatewayService::from_config(
            config,
            TestEnvironment::default()
                .with("OCTOROUTE_API_KEY", "inbound-test-key")
                .with("ZAI_API_KEY", "zai-test-key"),
        )
        .expect("service"),
    );

    for endpoint in ["/health/ready", "/health", "/health/ready", "/health"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(endpoint)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), 200, "{endpoint}");
        let body: Value =
            serde_json::from_slice(&response_body(response).await).expect("readiness JSON");
        assert_eq!(body["status"], "ready");
    }

    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .len(),
        1,
        "four anonymous readiness requests must not become four credentialed probes"
    );
}

/// Probe one HTTP provider pointed at `server` and report its verdict.
async fn provider_verdict(server: &MockServer) -> ProviderAdmissionState {
    let config = single_enabled_provider_config(server, "zai");
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");
    *service
        .readiness()
        .await
        .providers()
        .get("zai")
        .expect("the configured provider is probed")
}

async fn assert_models_status_proves_reachability(status: u16) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(status))
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(
        provider_verdict(&server).await,
        ProviderAdmissionState::Ready,
        "{status} proves the provider endpoint is reachable"
    );
}

#[tokio::test]
async fn method_not_allowed_from_models_proves_reachability() {
    assert_models_status_proves_reachability(405).await;
}

#[tokio::test]
async fn rate_limited_models_probe_proves_reachability() {
    assert_models_status_proves_reachability(429).await;
}

/// An operator who types the wrong base path - `/v1beta/` where the provider
/// serves `/v1/` - gets a 404 from every path under it. Readiness has to say so.
///
/// Calling that `Ready` because `/models` answered spends the only warning
/// anyone gets: readiness does not gate admission, so the step is dispatched,
/// the provider answers 404, and 404 is not one of the statuses a route falls
/// forward on, so it commits to the client with no fallback and no metric.
#[tokio::test]
async fn a_base_path_where_nothing_is_routed_is_not_ready() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
        .mount(&server)
        .await;

    assert_eq!(
        provider_verdict(&server).await,
        ProviderAdmissionState::Unavailable
    );
}

/// The other side of the same 404: a provider that implements only its
/// inference path is healthy, and the inference path answering - here 405,
/// because the probe is a GET against a POST-only route - is what proves it.
#[tokio::test]
async fn a_provider_without_a_models_endpoint_is_ready_when_its_inference_path_answers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(405))
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(
        provider_verdict(&server).await,
        ProviderAdmissionState::Ready
    );
}

/// A provider in an outage answers 5xx on the path that exists. That is not
/// corroboration of anything; reading it as "something answered, so the base
/// path is right" would report an outage as ready.
#[tokio::test]
async fn an_inference_path_in_an_outage_does_not_corroborate_a_missing_models_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(
        provider_verdict(&server).await,
        ProviderAdmissionState::Unavailable
    );
}

/// A credential the provider rejects is an operator error, not an outage: it is
/// outside the default fallback set precisely so it surfaces instead of
/// silently rerouting the traffic and the spend to the next provider.
#[tokio::test]
async fn a_rejected_credential_reports_unauthenticated_rather_than_unavailable() {
    for status in [401, 403] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            provider_verdict(&server).await,
            ProviderAdmissionState::Unauthenticated,
            "{status} is a refused credential"
        );
    }
}

/// A provider that 404s `/models` and refuses the credential on the path it does
/// serve is the same operator error, reached by the longer route.
#[tokio::test]
async fn a_credential_refused_on_the_inference_path_is_unauthenticated_too() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(
        provider_verdict(&server).await,
        ProviderAdmissionState::Unauthenticated
    );
}
