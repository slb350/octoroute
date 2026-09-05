//! Anthropic SSE reassembly and stream translation.

use super::{AnthropicSseTranslator, STREAM, chunk_json, rendered};
use crate::gateway::fabric::unknown_types::{Adapter, Counters};
use serde_json::json;

#[test]
fn sse_event_buffer_accepts_the_exact_limit_and_rejects_one_more_byte() {
    let mut translator = AnthropicSseTranslator::new("k3");
    let exact_limit = vec![b' '; 1024 * 1024];

    assert!(
        translator
            .push(&exact_limit)
            .expect("exact limit")
            .is_empty()
    );
    translator
        .push(b" ")
        .expect_err("one byte past the event limit must be rejected");
}

#[test]
fn recognized_no_output_events_are_not_counted_as_unknown() {
    let counters = Counters::default();
    let mut translator = AnthropicSseTranslator::new("k3");
    for event in [
        json!({"type": "ping"}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "signature_delta", "signature": "opaque"}
        }),
    ] {
        let frame = format!("data: {event}\n\n");
        assert!(
            translator
                .push_with_counters(frame.as_bytes(), &counters)
                .expect("known event")
                .is_empty()
        );
    }
    assert_eq!(counters.count(Adapter::Anthropic), 0);
    assert!(
        translator
            .push_with_counters(b"data: {\"type\":\"future.event\"}\n\n", &counters)
            .expect("unknown event")
            .is_empty()
    );
    assert_eq!(counters.count(Adapter::Anthropic), 1);
    assert_eq!(counters.count(Adapter::Codex), 0);
}

#[test]
fn content_block_starts_preserve_text_and_thinking_payloads() {
    for (block, field, expected) in [
        (json!({"type": "text", "text": "hello"}), "content", "hello"),
        (
            json!({"type": "thinking", "thinking": "inspect"}),
            "reasoning_content",
            "inspect",
        ),
    ] {
        let mut translator = AnthropicSseTranslator::new("k3");
        let event = json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": block
        });
        let output = translator
            .push(format!("data: {event}\n\n").as_bytes())
            .expect("content block start");

        assert_eq!(output.len(), 1, "{field}");
        assert_eq!(
            chunk_json(&output[0])["choices"][0]["delta"][field],
            expected
        );
    }
}

#[test]
fn content_block_deltas_preserve_text_and_thinking_payloads() {
    for (delta, field, expected) in [
        (
            json!({"type": "text_delta", "text": "hello"}),
            "content",
            "hello",
        ),
        (
            json!({"type": "thinking_delta", "thinking": "inspect"}),
            "reasoning_content",
            "inspect",
        ),
    ] {
        let mut translator = AnthropicSseTranslator::new("k3");
        let event = json!({"type": "content_block_delta", "index": 0, "delta": delta});
        let output = translator
            .push(format!("data: {event}\n\n").as_bytes())
            .expect("content block delta");

        assert_eq!(output.len(), 1, "{field}");
        assert_eq!(
            chunk_json(&output[0])["choices"][0]["delta"][field],
            expected
        );
    }
}

/// Reassembly is the point, so the stream is fed in fixed-size byte slices that
/// land inside `data:` lines and between the two newlines of a terminator. One
/// complete event per `push` exercises none of that.
#[test]
fn fragmented_anthropic_sse_is_incrementally_translated() {
    let mut translator = AnthropicSseTranslator::new("k3");
    let mut output = Vec::new();
    let mut buffering_pushes = 0;
    for fragment in STREAM.as_bytes().chunks(13) {
        let translated = translator.push(fragment).expect("translated chunk");
        if translated.is_empty() {
            buffering_pushes += 1;
        }
        output.extend(translated);
    }
    translator.finish().expect("complete stream");
    assert!(
        buffering_pushes > 0,
        "the fragments must not each complete an event"
    );
    let rendered = rendered(&output);
    assert!(rendered.contains("\"content\":\"hi\""));
    assert!(rendered.contains("data: [DONE]"));
    assert_eq!(
        rendered.matches("data: ").count(),
        4,
        "one chunk per translated event plus the terminator"
    );
}

/// A terminator split across two `push` calls: the first newline arrives with
/// one call and the second with the next, which is the exact byte boundary a
/// whole-event reassembler gets wrong.
#[test]
fn an_event_terminator_split_across_pushes_is_reassembled() {
    let mut translator = AnthropicSseTranslator::new("k3");
    for fragment in [
        &b"event: content_block_delta\ndata: {\"type\":\"content_bl"[..],
        &b"ock_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n"[..],
    ] {
        assert!(
            translator.push(fragment).expect("partial event").is_empty(),
            "an incomplete event must not be translated"
        );
    }
    let output = translator.push(b"\n").expect("completed event");
    assert_eq!(output.len(), 1);
    assert!(rendered(&output).contains("\"content\":\"hi\""));
}

/// An event type added after this release must not truncate a committed stream.
#[test]
fn unknown_sse_event_types_are_skipped_without_truncating_the_stream() {
    let mut translator = AnthropicSseTranslator::new("k3");
    let mut output = Vec::new();
    for event in [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"k3\",\"usage\":{\"input_tokens\":7}}}\n\n",
        "event: future_event\ndata: {\"type\":\"future_event\",\"detail\":\"unknown\"}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"future_delta\",\"value\":1}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ] {
        output.extend(translator.push(event.as_bytes()).expect("translated chunk"));
    }
    translator.finish().expect("complete stream");
    let rendered = rendered(&output);
    assert!(rendered.contains("\"content\":\"hi\""));
    assert!(rendered.contains("data: [DONE]"));
}

/// An `error` event stays fatal even though unknown events are skipped.
#[test]
fn anthropic_error_events_remain_fatal() {
    let mut translator = AnthropicSseTranslator::new("k3");
    translator
        .push(b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n")
        .expect_err("an error event must fail the stream");
}

/// A mixed-terminator stream must split at the earliest boundary.
///
/// Both terminators have to be in the buffer at once for this to discriminate:
/// `push` drains the buffer on every call, so feeding one event per call never
/// reaches the state where a later standalone `\n\n` wins over an earlier
/// `\r\n\r\n`. The events are concatenated into a single `push` for that reason.
#[test]
fn mixed_terminator_streams_split_at_the_earliest_boundary() {
    let stream = concat!(
        "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"k3\",\"usage\":{\"input_tokens\":7}}}\r\n\r\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
    );
    let mut translator = AnthropicSseTranslator::new("k3");
    let output = translator
        .push(stream.as_bytes())
        .expect("a mixed-terminator buffer must split at each boundary");
    translator.finish().expect("complete stream");
    let rendered = rendered(&output);
    assert!(rendered.contains("\"content\":\"hi\""));
    assert!(rendered.contains("data: [DONE]"));
}

/// `data: [DONE]` terminates an OpenAI stream. A chunk emitted after it is
/// written past the terminator, where no client will read it.
#[test]
fn events_after_message_stop_are_not_emitted_past_the_terminator() {
    let mut translator = AnthropicSseTranslator::new("k3");
    let mut output = Vec::new();
    output.extend(translator.push(STREAM.as_bytes()).expect("stream"));
    output.extend(
        translator
            .push(
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"LATE\"}}\n\n",
            )
            .expect("a trailing event must not fail a committed stream"),
    );
    // An `error` event is fatal mid-stream; after the terminator the stream is
    // already complete, so it cannot retroactively fail it.
    output.extend(
        translator
            .push(b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n")
            .expect("a trailing error must not fail a committed stream"),
    );
    translator.finish().expect("complete stream");
    let rendered = rendered(&output);
    assert!(!rendered.contains("LATE"), "{rendered}");
    assert!(
        rendered.trim_end().ends_with("data: [DONE]"),
        "the terminator must be the last thing emitted: {rendered}"
    );
}

/// A stream that stops before `message_stop` was truncated, and a client that
/// received no terminator must not be told the answer is complete.
#[test]
fn truncated_streams_are_rejected_by_finish() {
    let mut translator = AnthropicSseTranslator::new("k3");
    translator
        .push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\"}}\n\n")
        .expect("stream");
    translator
        .finish()
        .expect_err("a stream without message_stop must be rejected");

    // A terminated stream with a half-delivered trailing event is truncated too.
    let mut translator = AnthropicSseTranslator::new("k3");
    translator.push(STREAM.as_bytes()).expect("stream");
    translator
        .push(b"event: ping\ndata: {\"typ")
        .expect("partial");
    translator
        .finish()
        .expect_err("a half-delivered trailing event must be rejected");
}

/// Anthropic repeats the kind in the JSON body, but an Anthropic-compatible
/// endpoint may label only the `event:` line. Reading solely the body field
/// fails a stream this adapter can otherwise read.
#[test]
fn the_event_line_names_the_kind_when_the_body_does_not() {
    let mut translator = AnthropicSseTranslator::new("k3");
    let output = translator
        .push(b"event: message_stop\ndata: {}\n\n")
        .expect("event-line kind");
    assert_eq!(rendered(&output), "data: [DONE]\n\n");
    translator.finish().expect("complete stream");
}

/// A skipped block's deltas must be skipped too, not fail the stream.
///
/// `content_block_start` skips a block type it cannot represent, which is how
/// the adapter stays forward compatible. The deltas for that block still
/// arrive, and they arrive *after* the response has committed to the client.
/// Looking their index up in `tool_indices` and failing on the miss turns a
/// complete generation into a truncated stream, which is the one thing a
/// post-commitment path must never do.
#[test]
fn deltas_for_a_skipped_content_block_do_not_truncate_the_stream() {
    const FUTURE_BLOCK: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"k3\",\"usage\":{\"input_tokens\":4}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"server_tool_use\",\"id\":\"srv_1\",\"name\":\"web_search\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"rust\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    let mut translator = AnthropicSseTranslator::new("k3");
    let mut output = Vec::new();
    for fragment in FUTURE_BLOCK.as_bytes().chunks(17) {
        output.extend(
            translator
                .push(fragment)
                .expect("a skipped block's deltas must not fail the stream"),
        );
    }
    output.extend(translator.finish().expect("the stream still completes"));

    let rendered = rendered(&output);
    // The text that followed the skipped block still reaches the client, and the
    // stream terminates properly instead of dying on the second event.
    assert!(
        rendered.contains("done"),
        "content after the skipped block must survive: {rendered}"
    );
    assert!(
        rendered.contains("data: [DONE]"),
        "the stream must terminate normally: {rendered}"
    );
    // The unrepresentable partial JSON is not smuggled through as a tool call.
    assert!(
        !rendered.contains("tool_calls"),
        "a skipped block must not emit tool_calls: {rendered}"
    );
}
