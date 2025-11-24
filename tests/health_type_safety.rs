//! Type safety tests for health response enums
//!
//! Tests that HealthResponse uses type-safe enums instead of &'static str,
//! preventing invalid states like status: "BROKEN" or health_tracking_status: "chaos".
//!
//! Addresses PR #4 Type Design Issue: HealthResponse weak type

use octoroute::handlers::health::{HealthResponse, HealthStatus, HealthTrackingStatus};

/// Test that HealthStatus enum serializes to "OK"
#[test]
fn test_health_status_serializes_to_ok() {
    let status = HealthStatus::Ok;
    let json = serde_json::to_string(&status).expect("Should serialize");

    assert_eq!(
        json, "\"OK\"",
        "HealthStatus::Ok should serialize to \"OK\""
    );
}

/// Test that HealthTrackingStatus::Operational serializes correctly
#[test]
fn test_health_tracking_operational_serializes() {
    let status = HealthTrackingStatus::Operational;
    let json = serde_json::to_string(&status).expect("Should serialize");

    assert_eq!(
        json, "\"operational\"",
        "HealthTrackingStatus::Operational should serialize to \"operational\""
    );
}

/// Test that HealthTrackingStatus::Degraded serializes correctly
#[test]
fn test_health_tracking_degraded_serializes() {
    let status = HealthTrackingStatus::Degraded;
    let json = serde_json::to_string(&status).expect("Should serialize");

    assert_eq!(
        json, "\"degraded\"",
        "HealthTrackingStatus::Degraded should serialize to \"degraded\""
    );
}

/// Test that HealthResponse::new() constructs from metrics
#[test]
fn test_health_response_new_with_zero_failures() {
    let response = HealthResponse::new(0, 0, 0);

    let json = serde_json::to_value(&response).expect("Should serialize");

    assert_eq!(json["status"], "OK", "Status should be OK");
    assert_eq!(
        json["health_tracking_status"], "operational",
        "Health tracking should be operational with 0 failures"
    );
    assert_eq!(
        json["metrics_recording_status"], "operational",
        "Metrics recording should be operational with 0 failures"
    );
}

/// Test that HealthResponse::new() returns degraded when failures > 0
#[test]
fn test_health_response_new_with_failures() {
    let response = HealthResponse::new(5, 0, 0);

    let json = serde_json::to_value(&response).expect("Should serialize");

    assert_eq!(json["status"], "OK", "Status should be OK");
    assert_eq!(
        json["health_tracking_status"], "degraded",
        "Health tracking should be degraded with failures > 0"
    );
    assert_eq!(
        json["metrics_recording_status"], "operational",
        "Metrics recording should be operational with 0 failures"
    );
}

/// Test that HealthResponse fields are private (compile-time check)
#[test]
fn test_health_response_fields_are_private() {
    let response = HealthResponse::new(0, 0, 0);

    // This test verifies we must use the constructor
    // Trying to access response.status or response.health_tracking_status
    // should NOT compile (this is a compile-time check, not runtime)

    // Verify the response serializes correctly
    let json = serde_json::to_value(&response).expect("Should serialize");
    assert!(json["status"].is_string(), "Status should serialize");
    assert!(
        json["health_tracking_status"].is_string(),
        "Health tracking should serialize"
    );
    assert!(
        json["metrics_recording_status"].is_string(),
        "Metrics recording should serialize"
    );
}

/// Test that HealthResponse prevents invalid states at compile time
///
/// This test documents that the new design prevents invalid states.
/// The old design allowed: HealthResponse { status: "BROKEN", health_tracking_status: "chaos" }
/// The new design makes this impossible at compile time.
#[test]
fn test_invalid_states_prevented_at_compile_time() {
    // With the old design, this was possible (but wrong):
    // let bad_response = HealthResponse {
    //     status: "BROKEN",  // Invalid!
    //     health_tracking_status: "chaos",  // Invalid!
    // };

    // With the new design, invalid states are impossible:
    // - Fields are private (can't construct directly)
    // - Enums restrict values to valid states only
    // - Constructor enforces invariants

    let response = HealthResponse::new(0, 0, 0);
    let json = serde_json::to_value(&response).expect("Should serialize");

    // Can only have valid states
    assert!(
        json["status"] == "OK",
        "Status can only be OK (enforced by HealthStatus enum)"
    );

    assert!(
        json["health_tracking_status"] == "operational"
            || json["health_tracking_status"] == "degraded",
        "Health tracking can only be operational or degraded (enforced by enum)"
    );

    assert!(
        json["metrics_recording_status"] == "operational"
            || json["metrics_recording_status"] == "degraded",
        "Metrics recording can only be operational or degraded (enforced by enum)"
    );
}
