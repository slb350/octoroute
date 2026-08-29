//! Tool declaration, tool-choice, and tool-call translation.

use super::{build_request, chat, config, request, translate};
use serde_json::{Value, json};

/// A tool contract Anthropic cannot be given: `tool_choice` naming an array
/// that is not there, and an empty `tools` array the API rejects outright.
#[test]
fn unusable_tool_declarations_fail_closed() {
    let config = config();
    for tools in [
        json!({"tool_choice": "auto"}),
        json!({"tool_choice": "required"}),
        json!({"tools": []}),
        json!({"tools": [], "tool_choice": "auto"}),
    ] {
        let error = build_request(&config.providers["kimi"], &request(chat(tools.clone())))
            .expect_err("an unusable tool declaration must fail closed");
        assert!(error.is_incompatible(), "{tools} must be incompatible");
    }
}

/// Only `function` tool calls have a `tool_use` equivalent.
#[test]
fn tool_call_types_other_than_function_fail_closed() {
    let config = config();
    for call_type in [json!("custom"), Value::Null] {
        let error = build_request(
            &config.providers["kimi"],
            &request(json!({
                "model": "cloud-sota",
                "messages": [
                    {"role": "user", "content": "go"},
                    {"role": "assistant", "content": null, "tool_calls": [{
                        "id": "call-1",
                        "type": call_type,
                        "function": {"name": "f", "arguments": "{}"}
                    }]}
                ]
            })),
        )
        .expect_err("a non-function tool call must fail closed");
        assert!(error.is_incompatible());
    }
}

/// Each OpenAI `tool_choice` spelling has one Anthropic equivalent, and getting
/// the mapping wrong changes whether the model may or must call a tool.
#[test]
fn every_tool_choice_maps_to_its_anthropic_equivalent() {
    for (choice, expected) in [
        (json!("none"), json!({"type": "none"})),
        (json!("auto"), json!({"type": "auto"})),
        (json!("required"), json!({"type": "any"})),
        (
            json!({"type": "function", "function": {"name": "read_file"}}),
            json!({"type": "tool", "name": "read_file"}),
        ),
    ] {
        let body = translate(json!({
            "model": "cloud-sota",
            "messages": [
                {"role": "user", "content": "go"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call-1", "content": "contents"}
            ],
            "tool_choice": choice,
            "tools": [{
                "type": "function",
                "function": {"name": "read_file", "parameters": {"type": "object"}}
            }]
        }));
        assert_eq!(body["tool_choice"], expected);
        assert_eq!(body["tools"][0]["name"], "read_file");
    }
}

/// A `tool_choice` this adapter does not recognize must not be translated into
/// the nearest thing it does recognize.
#[test]
fn unrecognized_tool_choice_values_fail_closed() {
    let config = config();
    for choice in [
        json!("a_choice_openai_has_not_invented_yet"),
        json!({"type": "custom", "function": {"name": "read_file"}}),
        json!({"function": {"name": "read_file"}}),
        json!(["read_file"]),
    ] {
        let error = build_request(
            &config.providers["kimi"],
            &request(chat(json!({
                "tool_choice": choice,
                "tools": [{
                    "type": "function",
                    "function": {"name": "read_file", "parameters": {"type": "object"}}
                }]
            }))),
        )
        .expect_err("an unrecognized tool_choice must fail closed");
        assert!(error.is_incompatible(), "{choice} must be incompatible");
    }
}

/// `tool_choice: "none"` without a tools array is the state the request is
/// already in, so honoring it cannot change what the caller receives.
#[test]
fn tool_choice_none_without_tools_is_a_no_op() {
    let body = translate(chat(json!({"tool_choice": "none"})));
    assert!(body.get("tools").is_none(), "no tools were declared");
    assert!(body.get("tool_choice").is_none(), "nothing to choose from");
}
