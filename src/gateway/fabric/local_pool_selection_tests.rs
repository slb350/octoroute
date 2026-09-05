//! Member selection: rotation, priority, and least-loaded ordering.

use super::local_pool_tests::{
    EmptyEnvironment, lease, mount_available, mount_ready, request, worker_pool,
};
use super::{LlamaCppPool, PoolAdmissionState};
use serde_json::json;
use std::collections::BTreeSet;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn three_held_leases_fill_three_independent_workers() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        mount_ready(server, 10_000, 8_000).await;
    }
    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");

    let first = lease(pool.try_admit(&request(8_000)).await.expect("first"));
    let second = lease(pool.try_admit(&request(8_000)).await.expect("second"));
    let third = lease(pool.try_admit(&request(8_000)).await.expect("third"));
    let members = BTreeSet::from([
        first.member().to_string(),
        second.member().to_string(),
        third.member().to_string(),
    ]);
    assert_eq!(members.len(), 3);

    let fourth = pool.try_admit(&request(8_000)).await.expect("fourth");
    assert_eq!(fourth.state(), PoolAdmissionState::Busy);
}

#[tokio::test]
async fn unhealthy_member_is_skipped_before_disclosing_to_next_local_member() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&servers[0])
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .expect(1)
        .mount(&servers[0])
        .await;
    mount_ready(&servers[1], 20_000, 16_000).await;

    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");
    let lease = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(lease.member(), "worker-1");
}

/// Member `priority` sorts ascending: a lower number is preferred. This is the
/// production selection path, not a parallel copy of its ordering rules.
#[tokio::test]
async fn lower_priority_number_is_preferred_over_rotation() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        mount_available(server, 20_000).await;
    }
    let mut config = worker_pool(&servers);
    // worker-2 is the least preferred by rotation and the most preferred by
    // priority, so only priority can explain selecting it first.
    config.members[0].priority = 100;
    config.members[1].priority = 100;
    config.members[2].priority = 10;
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    let lease = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(lease.member(), "worker-2");
}

/// Least-loaded selection: a member already serving a request must lose to an
/// idle one even when rotation would pick it first.
///
/// Rotation has to point AT the busy member for this to discriminate. `try_admit`
/// advances the cursor past whatever it just picked, so the sequence below walks
/// the cursor all the way around back to the member that is still holding a
/// lease. Deleting the in-flight term from the sort key in `candidates` makes
/// rotation decide, and this test then fails.
#[tokio::test]
async fn busier_member_loses_to_an_idle_one_even_when_rotation_favours_it() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        mount_available(server, 20_000).await;
    }
    // worker-0 keeps spare capacity so it stays selectable while holding a lease;
    // load, not capacity, is what must exclude it.
    let mut config = worker_pool(&servers);
    config.members[0].max_in_flight = 3;
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    let held = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(held.member(), "worker-0", "cursor starts at worker-0");

    // Walk the cursor back around to worker-0, releasing each lease so only
    // worker-0 is left carrying load.
    for expected in ["worker-1", "worker-2"] {
        let transient = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
        assert_eq!(transient.member(), expected);
        drop(transient);
    }

    // Rotation now favours worker-0 and load does not.
    let selected = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_ne!(
        selected.member(),
        "worker-0",
        "an idle member must win over one already serving a request"
    );
    drop(held);
}
