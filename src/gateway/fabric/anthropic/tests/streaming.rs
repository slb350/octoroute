//! Anthropic SSE reassembly and stream translation.

use super::{AnthropicSseTranslator, STREAM, rendered};

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
