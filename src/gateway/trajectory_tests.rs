use super::{
    request::GatewayRequest,
    test_support::{gateway_request, trajectory_tool_call, trajectory_tool_result},
    trajectory::TrajectorySignals,
};
use serde_json::json;

fn request(messages: serde_json::Value) -> GatewayRequest {
    gateway_request(json!({"model": "auto", "messages": messages}))
}

#[test]
fn extracts_closed_signals_from_paired_typed_tool_results() {
    let request = request(json!([
        trajectory_tool_call("call-1"),
        trajectory_tool_result(
            "call-1",
            json!({
                "outcome": "failure",
                "error_severity": "hard",
                "environment": "production",
                "test_status": "failed",
                "context_compacted": true
            })
        ),
        trajectory_tool_call("call-2"),
        trajectory_tool_result(
            "call-2",
            json!({
                "outcome": "success",
                "error_severity": "none",
                "environment": "production",
                "test_status": "passed",
                "context_compacted": false
            })
        ),
        {"role": "user", "content": "Continue the task."}
    ]));

    let signals = TrajectorySignals::extract(&request).expect("verified trajectory");
    assert_eq!(signals.error_severity(), "hard");
    assert_eq!(signals.clean_streak(), 1);
    assert_eq!(signals.environment(), "production");
    assert_eq!(signals.test_status(), "passed");
    assert!(signals.context_compacted());
    assert_eq!(
        signals.to_prompt_json(),
        r#"{"error_severity":"hard","clean_streak":1,"environment":"production","test_status":"passed","context_compacted":true}"#
    );
}

#[test]
fn abstains_for_untyped_unpaired_or_inconsistent_tool_history() {
    for messages in [
        json!([
            trajectory_tool_call("call-1"),
            {"role": "tool", "tool_call_id": "call-1", "content": "ordinary result"},
            {"role": "user", "content": "Continue."}
        ]),
        json!([
            trajectory_tool_call("call-1"),
            trajectory_tool_result(
                "call-1",
                json!({
                    "outcome": "success",
                    "error_severity": "none",
                    "environment": "development",
                    "test_status": "passed",
                    "context_compacted": false
                })
            ),
            trajectory_tool_call("call-2"),
            {"role": "user", "content": "Continue."}
        ]),
        json!([
            trajectory_tool_result(
                "missing-call",
                json!({
                    "outcome": "failure",
                    "error_severity": "hard",
                    "environment": "production",
                    "test_status": "failed",
                    "context_compacted": false
                })
            ),
            {"role": "user", "content": "Continue."}
        ]),
        json!([
            trajectory_tool_call("call-1"),
            trajectory_tool_result(
                "call-1",
                json!({
                    "outcome": "success",
                    "error_severity": "hard",
                    "environment": "development",
                    "test_status": "not_run",
                    "context_compacted": false
                })
            ),
            {"role": "user", "content": "Continue."}
        ]),
        json!([
            trajectory_tool_call(""),
            trajectory_tool_result(
                "",
                json!({
                    "outcome": "success",
                    "error_severity": "none",
                    "environment": "development",
                    "test_status": "passed",
                    "context_compacted": false
                })
            ),
            {"role": "user", "content": "Continue."}
        ]),
    ] {
        assert!(TrajectorySignals::extract(&request(messages)).is_none());
    }

    let without_tools = request(json!([{"role": "user", "content": "Fresh task."}]));
    assert!(TrajectorySignals::extract(&without_tools).is_none());
}
