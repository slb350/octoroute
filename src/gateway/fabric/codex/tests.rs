//! Codex CLI adapter tests.

use super::events::*;
use super::*;
const DIAGNOSTIC: &str = r#"{
  "schemaVersion": 1,
  "codexVersion": "0.148.0",
  "checks": {
    "auth.credentials": {
      "details": {
        "stored ChatGPT tokens": "true",
        "stored auth mode": "chatgpt"
      }
    }
  }
}"#;

#[test]
fn child_environment_retains_runtime_paths_but_excludes_secrets() {
    let environment = ChildEnvironment::from_iter([
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("HOME"), OsString::from("/safe/home")),
        (
            OsString::from("CODEX_HOME"),
            OsString::from("/safe/home/.codex"),
        ),
        (
            OsString::from("OPENAI_API_KEY"),
            OsString::from("must-not-leak"),
        ),
        (
            OsString::from("OCTOROUTE_API_KEY"),
            OsString::from("must-not-leak"),
        ),
    ]);
    assert_eq!(environment.get("HOME"), Some("/safe/home"));
    assert_eq!(environment.get("CODEX_HOME"), Some("/safe/home/.codex"));
    assert_eq!(environment.get("OPENAI_API_KEY"), None);
    assert_eq!(environment.get("OCTOROUTE_API_KEY"), None);
}

#[test]
fn diagnostic_accepts_only_chatgpt_managed_auth() {
    assert!(parse_diagnostic(DIAGNOSTIC.as_bytes()).is_ok());
    let api = DIAGNOSTIC
        .replace("\"true\"", "\"false\"")
        .replace("\"chatgpt\"", "\"api\"");
    assert!(matches!(
        parse_diagnostic(api.as_bytes()),
        Err(CodexAdapterError::NotChatGpt)
    ));
}

#[test]
fn event_contract_rejects_internal_tool_activity() {
    let events = concat!(
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\"}}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{}\"}}\n",
        "{\"type\":\"turn.completed\"}\n"
    );
    assert!(matches!(
        parse_events(events.as_bytes()),
        Err(CodexAdapterError::Contract)
    ));
}

#[test]
fn final_codex_json_becomes_an_open_ai_stream() {
    let events = concat!(
        "{\"type\":\"thread.started\"}\n",
        "{\"type\":\"turn.started\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"content\\\":\\\"answer\\\",\\\"reasoning_content\\\":null,\\\"tool_calls\\\":[],\\\"finish_reason\\\":\\\"stop\\\"}\"}}\n",
        "{\"type\":\"turn.completed\"}\n"
    );
    let (reply, usage) = parse_events(events.as_bytes()).expect("Codex reply");
    let rendered = render_open_ai_reply("gpt-test", true, reply, usage).expect("OpenAI stream");
    let rendered = std::str::from_utf8(&rendered).expect("UTF-8");
    assert!(rendered.contains("chat.completion.chunk"), "{rendered}");
    assert!(rendered.contains("data: [DONE]"), "{rendered}");
}

/// `turn.completed` carries token accounting, and cost-tracking clients read it.
#[test]
fn codex_usage_is_reported_in_both_response_shapes() {
    let events = concat!(
        "{\"type\":\"thread.started\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"content\\\":\\\"answer\\\",\\\"reasoning_content\\\":null,\\\"tool_calls\\\":[],\\\"finish_reason\\\":\\\"stop\\\"}\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":41,\"output_tokens\":7}}\n"
    );
    let (reply, usage) = parse_events(events.as_bytes()).expect("Codex reply");
    let rendered = render_open_ai_reply("gpt-test", false, reply, usage).expect("response");
    let rendered: Value = serde_json::from_slice(&rendered).expect("response JSON");
    assert_eq!(rendered["usage"]["prompt_tokens"], 41);
    assert_eq!(rendered["usage"]["completion_tokens"], 7);
    assert_eq!(rendered["usage"]["total_tokens"], 48);
}

/// A Codex release adding an event or item type must not turn every request
/// into a 502.
#[test]
fn unknown_codex_event_and_item_types_are_skipped() {
    let events = concat!(
        "{\"type\":\"thread.started\"}\n",
        "{\"type\":\"future.event\",\"detail\":\"unknown\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"future_item\"}}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"content\\\":\\\"answer\\\",\\\"reasoning_content\\\":null,\\\"tool_calls\\\":[],\\\"finish_reason\\\":\\\"stop\\\"}\"}}\n",
        "{\"type\":\"turn.completed\"}\n"
    );
    let (reply, _) = parse_events(events.as_bytes()).expect("unknown types must be skipped");
    assert_eq!(reply.content.as_deref(), Some("answer"));
}

/// A truncated run is still rejected: skipping unknown events must not
/// weaken the completion requirement.
#[test]
fn a_run_without_turn_completed_is_still_rejected() {
    let events = concat!(
        "{\"type\":\"thread.started\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"content\\\":\\\"answer\\\",\\\"reasoning_content\\\":null,\\\"tool_calls\\\":[],\\\"finish_reason\\\":\\\"stop\\\"}\"}}\n"
    );
    assert!(matches!(
        parse_events(events.as_bytes()),
        Err(CodexAdapterError::Contract)
    ));
}

#[test]
fn nonstream_tool_calls_do_not_include_stream_only_indices() {
    let reply = CodexReply {
        content: None,
        reasoning_content: None,
        tool_calls: vec![CodexToolCall {
            id: "call_1".to_string(),
            name: "lookup".to_string(),
            arguments: "{}".to_string(),
        }],
        finish_reason: "tool_calls".to_string(),
    };
    let rendered = render_open_ai_reply("gpt-test", false, reply, None).expect("OpenAI response");
    let rendered: Value = serde_json::from_slice(&rendered).expect("response JSON");
    let call = &rendered["choices"][0]["message"]["tool_calls"][0];
    assert!(call.get("index").is_none());
    assert_eq!(call["function"]["name"], "lookup");
}

/// `sensitive_name` is reachable, contrary to a plausible reading of the fixed
/// allowlist. The `LC_` arm is an open-ended prefix, not a fixed entry, so it
/// admits any `LC_*` name the environment happens to carry; the sensitive-name
/// check is what stops `LC_AUTH` or `LC_SECRET` reaching the Codex child.
#[test]
fn open_ended_locale_prefix_still_filters_sensitive_names() {
    for name in ["LC_ALL", "LC_TIME", "LC_MESSAGES"] {
        assert!(
            allowed_name(OsStr::new(name)),
            "{name} is an ordinary locale variable"
        );
    }
    for name in [
        "LC_AUTH",
        "LC_SECRET",
        "LC_API_KEY",
        "LC_TOKEN",
        "LC_PASSWORD",
    ] {
        assert!(
            !allowed_name(OsStr::new(name)),
            "{name} matches the LC_ prefix and must still be filtered"
        );
    }
    assert!(!allowed_name(OsStr::new("OCTOROUTE_API_KEY")));
}
