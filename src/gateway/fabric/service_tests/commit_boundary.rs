//! The response commit boundary: the target is fixed at the first client-visible byte.

use super::*;

/// Once the first body byte is client-visible the target is fixed.
///
/// The member here streams one SSE chunk and then ends without `[DONE]`, which
/// is exactly the shape a later "retry the next step on a truncated stream"
/// change would want to act on. The truncated body must reach the client as-is
/// and the reachable second step must stay untouched.
#[tokio::test]
async fn a_committed_local_stream_that_ends_early_never_switches_target() {
    let supervisor_server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(0)
        .mount(&supervisor_server)
        .await;
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    "data: {\"id\":\"truncated\",\"choices\":[]}\n\n",
                    "text/event-stream",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut config = local_config(&server);
    let supervisor = config
        .local_pools
        .get_mut("supervisor-local")
        .expect("supervisor pool");
    supervisor.enabled = true;
    supervisor.members[0].base_url =
        Url::parse(&supervisor_server.uri()).expect("supervisor mock URL");
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("local"))
        .await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-destination")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-upstream")
            .and_then(|value| value.to_str().ok()),
        Some("workers/worker-0")
    );
    let body = response_body(response).await;
    let text = std::str::from_utf8(&body).expect("UTF-8");
    assert_eq!(text, "data: {\"id\":\"truncated\",\"choices\":[]}\n\n");
    assert!(
        supervisor_server
            .received_requests()
            .await
            .expect("request recording")
            .is_empty(),
        "the second step must never be contacted after commitment"
    );
}
