//! Message, role, and content-block translation.

use super::{build_request, config, request, translate};
use serde_json::{Value, json};

#[test]
fn open_ai_messages_tools_and_reasoning_translate_explicitly() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "system", "content": "Be concise."},
            {"role": "user", "content": "Read the file."},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"README.md\"}"}
                }]
            },
            {"role": "tool", "tool_call_id": "call-1", "content": "contents"}
        ],
        "stream": true,
        "max_completion_tokens": 8192,
        "reasoning_effort": "high",
        "tools": [{
            "type": "function",
            "function": {
                "name": "read_file",
                "parameters": {"type": "object", "properties": {}}
            }
        }]
    }));

    assert_eq!(body["model"], "k3");
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"][0]["text"], "Be concise.");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["text"], "Read the file.");
    // The parsed `input` is the assertion that matters: `arguments` is a JSON
    // string on the OpenAI side and a JSON object on the Anthropic side, so a
    // translation that forwarded the string would still satisfy a type-only
    // check.
    assert_eq!(
        body["messages"][1]["content"][0],
        json!({
            "type": "tool_use",
            "id": "call-1",
            "name": "read_file",
            "input": {"path": "README.md"}
        })
    );
    assert_eq!(
        body["messages"][2]["content"][0],
        json!({"type": "tool_result", "tool_use_id": "call-1", "content": "contents"})
    );
    assert_eq!(
        body["tools"][0],
        json!({
            "name": "read_file",
            "input_schema": {"type": "object", "properties": {}}
        })
    );
    // `high` effort against 8192 total tokens is capped at half the budget.
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 4096})
    );
}

/// Anthropic carries system text in a dedicated field, so a mid-conversation
/// instruction cannot be represented in place and must not be hoisted.
#[test]
fn system_message_after_conversation_content_fails_closed() {
    let config = config();
    let error = build_request(
        &config.providers["kimi"],
        &request(json!({
            "model": "cloud-sota",
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "system", "content": "LATE"},
                {"role": "user", "content": "second"}
            ]
        })),
    )
    .expect_err("a late system message must fail closed");
    assert!(error.is_incompatible());
}

#[test]
fn leading_system_messages_are_still_accepted() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "system", "content": "one"},
            {"role": "developer", "content": "two"},
            {"role": "user", "content": "answer"}
        ]
    }));
    assert_eq!(body["system"][0]["text"], "one");
    assert_eq!(body["system"][1]["text"], "two");
    assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
}

/// Anthropic rejects empty text, so forwarding it buys a committed 400.
#[test]
fn empty_content_fails_closed() {
    let config = config();
    for content in [json!(""), json!([]), json!([{"type": "text", "text": ""}])] {
        let error = build_request(
            &config.providers["kimi"],
            &request(json!({
                "model": "cloud-sota",
                "messages": [{"role": "user", "content": content}]
            })),
        )
        .expect_err("empty content must fail closed");
        assert!(error.is_incompatible());
    }
}

/// Anthropic refuses an empty tool result, while a non-empty result is part of
/// the supported tool-history shape. The distinction must survive translation.
#[test]
fn an_empty_string_tool_result_is_distinct_from_a_non_empty_result() {
    let config = config();
    let request_with_result = |content| {
        request(json!({
            "model": "cloud-sota",
            "messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "f", "arguments": "{}"}
                }]},
                {"role": "tool", "tool_call_id": "call-1", "content": content}
            ]
        }))
    };

    let error = build_request(&config.providers["kimi"], &request_with_result(json!("")))
        .expect_err("an empty tool result must fail closed");
    assert!(error.is_incompatible());

    let translated = build_request(
        &config.providers["kimi"],
        &request_with_result(json!("result")),
    )
    .expect("a non-empty tool result is representable");
    let translated: Value =
        serde_json::from_slice(&translated.body).expect("translated request JSON");
    assert_eq!(translated["messages"][2]["content"][0]["content"], "result");
}

/// The empty-content rule must not break the shape every tool-calling client
/// emits: an assistant turn whose only content is its tool calls.
#[test]
fn a_tool_calling_assistant_turn_may_carry_no_text() {
    for content in [Value::Null, json!(""), json!([])] {
        let body = translate(json!({
            "model": "cloud-sota",
            "messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": content, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "f", "arguments": "{}"}
                }]}
            ]
        }));
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(
            body["messages"][1]["content"]
                .as_array()
                .expect("blocks")
                .len(),
            1
        );
    }
}

/// Anthropic requires the conversation to open on a `user` turn.
#[test]
fn a_conversation_must_open_on_a_user_turn() {
    let config = config();
    let error = build_request(
        &config.providers["kimi"],
        &request(json!({
            "model": "cloud-sota",
            "messages": [
                {"role": "assistant", "content": "I was mid-thought"},
                {"role": "user", "content": "go on"}
            ]
        })),
    )
    .expect_err("an assistant-first conversation must fail closed");
    assert!(error.is_incompatible());
}

/// The standard tool-calling loop appends `response.choices[0].message` back
/// into the history verbatim, so an OpenAI assistant *response* message has to
/// translate. It always carries `refusal` and `annotations`.
#[test]
fn an_open_ai_response_message_fed_back_translates() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": "hi", "refusal": null, "annotations": []},
            {"role": "user", "content": "again"}
        ]
    }));
    assert_eq!(body["messages"][1]["role"], "assistant");
    assert_eq!(
        body["messages"][1]["content"],
        json!([{"type": "text", "text": "hi"}])
    );
}

/// A streaming accumulator reassembles `tool_calls` entries with the `index`
/// they arrived under, and a client feeds that straight back in.
#[test]
fn a_reassembled_tool_call_keeps_its_index() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": null, "annotations": [], "tool_calls": [{
                "index": 0,
                "id": "call-1",
                "type": "function",
                "function": {"name": "f", "arguments": "{}"}
            }]}
        ]
    }));
    assert_eq!(
        body["messages"][1]["content"][0],
        json!({"type": "tool_use", "id": "call-1", "name": "f", "input": {}})
    );
}

/// `name` differentiates participants on every OpenAI message role. Anthropic
/// has no per-message author field, so it is dropped rather than refused.
#[test]
fn a_participant_name_is_accepted_on_every_role() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "system", "content": "be brief", "name": "policy"},
            {"role": "user", "content": "go", "name": "alice"},
            {"role": "assistant", "content": null, "name": "bob", "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {"name": "f", "arguments": "{}"}
            }]},
            {"role": "tool", "tool_call_id": "call-1", "content": "out", "name": "f"}
        ]
    }));
    assert_eq!(body["system"][0]["text"], "be brief");
    assert_eq!(body["messages"][0]["content"][0]["text"], "go");
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
}

/// Array-shaped content is the other spelling of every role's content, and it
/// has to survive translation on the user turn, the assistant turn, and the
/// tool result alike.
#[test]
fn array_content_translates_on_every_role() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "go"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "calling"}], "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {"name": "f", "arguments": "{}"}
            }]},
            {"role": "tool", "tool_call_id": "call-1", "content": [{"type": "text", "text": "out"}]}
        ]
    }));
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type": "text", "text": "go"}])
    );
    assert_eq!(
        body["messages"][1]["content"][0],
        json!({"type": "text", "text": "calling"})
    );
    assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
    assert_eq!(
        body["messages"][2]["content"][0]["content"],
        json!([{"type": "text", "text": "out"}])
    );
}

/// A non-null `refusal` or `audio` is assistant content with no Anthropic
/// representation, so it fails closed rather than being dropped silently. The
/// `null` forms are what a text round-trip actually carries, and they pass.
#[test]
fn assistant_content_without_a_mapping_fails_closed() {
    let config = config();
    for (label, extra) in [
        ("refusal", json!({"refusal": "I cannot help with that."})),
        ("audio", json!({"audio": {"id": "audio-1"}})),
        (
            "function_call",
            json!({"function_call": {"name": "f", "arguments": "{}"}}),
        ),
    ] {
        let mut message = json!({"role": "assistant", "content": "hi"});
        message
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        let error = build_request(
            &config.providers["kimi"],
            &request(json!({
                "model": "cloud-sota",
                "messages": [{"role": "user", "content": "go"}, message]
            })),
        )
        .expect_err(label);
        assert!(error.is_incompatible(), "{label} must be incompatible");
    }
}
