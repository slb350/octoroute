//! Which rejection a route that ran out of steps reports to the client.

use super::super::*;

/// Mount a healthy member whose every slot is taken, so the pool admits
/// nothing and the step falls forward as `busy`.
async fn mount_busy_local(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .and(query_param("fail_on_no_slot", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": true}])))
        .mount(server)
        .await;
}

/// A context overflow is terminal and the caller can fix it; the earlier busy
/// pool is neither. Reporting the busy pool answers a request that can never
/// succeed with a retryable 503, so the client retries forever.
#[tokio::test]
async fn a_terminal_context_overflow_outranks_an_earlier_busy_pool() {
    let server = MockServer::start().await;
    mount_busy_local(&server).await;
    let mut config = local_config(&server);
    let supervisor = config
        .local_pools
        .get_mut("supervisor-local")
        .expect("supervisor pool");
    supervisor.enabled = true;
    // The output reservation alone exceeds the window, so the step is refused
    // before any probe is sent to the member.
    supervisor.context_window = 4096;
    supervisor.context_safety_tokens = 0;
    supervisor.default_max_output_tokens = 8192;
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("local"))
        .await;

    assert_eq!(response.status(), 400);
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "local_context_overflow");
}

/// `unauthenticated` is kept out of the default fallback trigger set so that a
/// missing or expired credential surfaces. Letting an earlier busy pool mask it
/// puts it back in the logs only.
#[tokio::test]
async fn an_unresolvable_provider_credential_outranks_an_earlier_busy_pool() {
    let server = MockServer::start().await;
    mount_busy_local(&server).await;
    let mut config = local_config(&server);
    let route = config.routes.get_mut("auto-route").expect("auto route");
    route.steps = vec![
        RouteTarget::LocalPool("workers".to_string()),
        RouteTarget::Provider("zai".to_string()),
    ];
    // No `ZAI_API_KEY`, so the provider step is refused as unauthenticated
    // without any request reaching the network.
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("auto-route"))
        .await;

    assert_eq!(response.status(), 503);
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    assert_eq!(body["error"]["code"], "provider_unauthenticated");
}

/// A busy pool followed by a disabled one is two capacity conditions, and the
/// first is the one the operator can act on: enabling the pool that was last
/// would not have helped.
#[tokio::test]
async fn terminal_error_names_the_rejection_that_governed_the_route() {
    let server = MockServer::start().await;
    mount_busy_local(&server).await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("local"))
        .await;

    assert_eq!(response.status(), 503);
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    // A 503 typed `invalid_request_error` tells a client to retry and to fix
    // its request at the same time.
    assert_eq!(body["error"]["type"], "upstream_error");
    assert_eq!(body["error"]["code"], "local_busy");
}

/// The provider side records the same observation as the local one. Both calls
/// need a test, or half the histogram can be deleted unnoticed.
#[tokio::test]
async fn a_rejected_provider_step_is_observed_in_the_routing_histogram() {
    let server = MockServer::start().await;
    let config = single_provider_config(&server, "zai");
    // No `ZAI_API_KEY`: the step is refused at admission, after the same
    // credential resolution an admitted step would have done.
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service.handle_chat(&headers(), cloud_request()).await;

    assert_eq!(response.status(), 503);
    let metrics = service.metrics_text();
    assert!(
        metrics.contains("octoroute_fabric_routing_duration_seconds_count 1"),
        "{metrics}"
    );
}

/// A rejected step does the same probe work an admitted one does. A histogram
/// that covered admissions only would go quiet during the outage an operator is
/// trying to read.
#[tokio::test]
async fn a_rejected_local_step_is_observed_in_the_routing_histogram() {
    let server = MockServer::start().await;
    let mut config = local_config(&server);
    config
        .local_pools
        .get_mut("workers")
        .expect("workers pool")
        .enabled = false;
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("worker"))
        .await;

    assert_eq!(response.status(), 503);
    let metrics = service.metrics_text();
    assert!(
        metrics.contains("octoroute_fabric_routing_duration_seconds_count 1"),
        "{metrics}"
    );
}
