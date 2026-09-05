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
        (
            OsString::from("OPENROUTER_API_KEY"),
            OsString::from("must-not-leak"),
        ),
        (OsString::from("LC_SECRET"), OsString::from("must-not-leak")),
        (OsString::from("RANDOM_VAR"), OsString::from("value")),
    ]);
    assert_eq!(environment.get("PATH"), Some("/usr/bin:/bin"));
    assert_eq!(environment.get("HOME"), Some("/safe/home"));
    assert_eq!(environment.get("CODEX_HOME"), Some("/safe/home/.codex"));
    assert_eq!(environment.get("OPENAI_API_KEY"), None);
    assert_eq!(environment.get("OCTOROUTE_API_KEY"), None);
    assert_eq!(environment.get("OPENROUTER_API_KEY"), None);
    assert_eq!(environment.get("LC_SECRET"), None);
    assert_eq!(environment.get("RANDOM_VAR"), None);
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

/// A tool item proves isolation failed even when the surrounding reply is valid.
#[test]
fn event_contract_rejects_internal_tool_activity() {
    for item in [
        "command_execution",
        "file_change",
        "mcp_tool_call",
        "web_search",
    ] {
        let events = format!(
            "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"{item}\"}}}}\n{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\"}}\n"
        );
        assert!(
            matches!(
                parse_events(events.as_bytes()),
                Err(CodexAdapterError::Contract)
            ),
            "`{item}` proves the Codex sandbox did not hold"
        );
    }

    // Control: the complete reply is valid without the forbidden tool item.
    let clean = format!("{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\"}}\n");
    assert!(parse_events(clean.as_bytes()).is_ok());
}

/// Trailing tool activity still proves isolation failed; completion cannot hide it.
#[test]
fn a_tool_item_after_the_turn_completed_still_fails_closed() {
    for item in [
        "command_execution",
        "file_change",
        "mcp_tool_call",
        "web_search",
    ] {
        let events = format!(
            "{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\"}}\n{{\"type\":\"item.completed\",\"item\":{{\"type\":\"{item}\"}}}}\n"
        );
        assert!(
            matches!(
                parse_events(events.as_bytes()),
                Err(CodexAdapterError::Contract)
            ),
            "`{item}` after the completion proves the Codex sandbox did not hold"
        );
    }
}

/// Future trailing events are allowed; failures and duplicate completions are not.
#[test]
fn trailing_events_after_completion_are_skipped_not_fatal() {
    let events = format!(
        "{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\"}}\n{{\"type\":\"future.trailer\",\"detail\":\"unknown\"}}\n"
    );
    let (reply, _) = parse_events(events.as_bytes()).expect("a trailing event is not fatal");
    assert_eq!(reply.content.as_deref(), Some("answer"));

    // A trailing failure is still fatal, and so is a second completion.
    let failed = format!(
        "{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\"}}\n{{\"type\":\"turn.failed\"}}\n"
    );
    assert!(matches!(
        parse_events(failed.as_bytes()),
        Err(CodexAdapterError::Process)
    ));
    let duplicated = format!(
        "{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\"}}\n{{\"type\":\"turn.completed\"}}\n"
    );
    assert!(matches!(
        parse_events(duplicated.as_bytes()),
        Err(CodexAdapterError::Contract)
    ));
}

/// A later valid completion cannot rescue an earlier out-of-order completion.
#[test]
fn a_completion_before_the_agent_message_is_a_contract_violation() {
    let events = format!(
        "{{\"type\":\"turn.completed\"}}\n{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\"}}\n"
    );
    assert!(matches!(
        parse_events(events.as_bytes()),
        Err(CodexAdapterError::Contract)
    ));
}

/// Reject ordering violations immediately, before a later failure changes the diagnosis.
#[test]
fn an_early_completion_is_rejected_before_any_later_event_is_read() {
    let events = concat!(
        "{\"type\":\"turn.completed\"}\n",
        "{\"type\":\"turn.failed\",\"error\":{\"message\":\"upstream refused\"}}\n"
    );
    assert!(matches!(
        parse_events(events.as_bytes()),
        Err(CodexAdapterError::Contract)
    ));
}

/// Present-but-empty accounting is absent accounting. Reporting zeros would
/// tell a cost-tracking client the turn was free.
#[test]
fn empty_codex_usage_omits_the_key_rather_than_reporting_zeros() {
    let events = format!("{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\",\"usage\":{{}}}}\n");
    let (reply, usage) = parse_events(events.as_bytes()).expect("Codex reply");
    assert!(usage.is_none());
    let rendered = render_open_ai_reply("gpt-test", false, reply, usage).expect("response");
    let rendered: Value = serde_json::from_slice(&rendered).expect("response JSON");
    assert!(
        rendered.get("usage").is_none(),
        "empty usage must be omitted: {rendered}"
    );

    // A half-reported usage object is still reported: the CLI said something.
    let partial = format!(
        "{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":9}}}}\n"
    );
    let (_, usage) = parse_events(partial.as_bytes()).expect("Codex reply");
    assert!(usage.is_some());
}

/// The final CLI message becomes one response; usage must survive either format.
#[test]
fn codex_reply_and_usage_are_preserved_in_both_response_shapes() {
    let events = format!(
        "{{\"type\":\"thread.started\"}}\n{{\"type\":\"turn.started\"}}\n{VALID_AGENT_MESSAGE}{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":41,\"output_tokens\":7}}}}\n"
    );
    for (stream, object, message_key) in [
        (false, "chat.completion", "message"),
        (true, "chat.completion.chunk", "delta"),
    ] {
        let (reply, usage) = parse_events(events.as_bytes()).expect("Codex reply");
        let rendered = render_open_ai_reply("gpt-test", stream, reply, usage).expect("response");
        let rendered = std::str::from_utf8(&rendered).expect("UTF-8 response");
        let json = if stream {
            // Codex produces one final message, not token-by-token output.
            assert_eq!(
                rendered.matches("data: ").count(),
                2,
                "one chunk plus [DONE]"
            );
            rendered
                .strip_prefix("data: ")
                .and_then(|data| data.strip_suffix("\n\ndata: [DONE]\n\n"))
                .expect("one SSE JSON chunk followed by [DONE]")
        } else {
            rendered
        };
        let response: Value = serde_json::from_str(json).expect("OpenAI JSON");
        assert_eq!(response["object"], object);
        assert_eq!(response["model"], "gpt-test");
        assert_eq!(response["choices"][0][message_key]["content"], "answer");
        assert_eq!(response["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            response["usage"],
            json!({
                "prompt_tokens": 41, "completion_tokens": 7, "total_tokens": 48
            })
        );
    }
}

/// Unknown event and item types remain forward-compatible.
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

/// Forward compatibility does not relax the completion requirement.
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

/// The open-ended LC_ prefix must still exclude secret-bearing variable names.
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

/// Service fixtures ignore argv, so verify each sandbox flag and its value here.
#[test]
fn codex_invocation_argv_pins_every_hardening_flag() {
    let args = invocation_args(
        "gpt-test",
        ReasoningEffort::High,
        Path::new("/tmp/instructions.md"),
        Path::new("/tmp/schema.json"),
        Path::new("/tmp/workspace"),
    )
    .expect("argv");
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    for feature in [
        "shell_tool",
        "unified_exec",
        "apps",
        "multi_agent",
        "hooks",
        "memories",
    ] {
        let position = rendered
            .iter()
            .position(|arg| arg == feature)
            .unwrap_or_else(|| panic!("`{feature}` must be disabled"));
        assert_eq!(
            rendered[position - 1],
            "--disable",
            "`{feature}` must be preceded by --disable"
        );
    }

    let sandbox = rendered
        .iter()
        .position(|arg| arg == "--sandbox")
        .expect("--sandbox must be present");
    assert_eq!(rendered[sandbox + 1], "read-only");
    for flag in [
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--json",
    ] {
        assert!(rendered.iter().any(|arg| arg == flag), "{flag} is required");
    }

    let approval = rendered
        .iter()
        .position(|arg| arg == "-a")
        .expect("-a must be present");
    assert_eq!(rendered[approval + 1], "never");
    for override_arg in [
        "project_doc_max_bytes=0",
        "web_search=\"disabled\"",
        "forced_login_method=\"chatgpt\"",
        "model_reasoning_effort=\"high\"",
    ] {
        assert!(
            rendered.iter().any(|arg| arg == override_arg),
            "`{override_arg}` must be passed with -c"
        );
    }

    // The prompt is passed on stdin, never as an argument.
    assert_eq!(rendered.last().map(String::as_str), Some("-"));
    assert!(rendered.iter().any(|arg| arg == "exec"));
}
