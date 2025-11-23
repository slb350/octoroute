//! Integration tests for ChatRequest validation
//!
//! Tests the custom Deserialize implementation for ChatRequest that validates:
//! - Empty messages are rejected
//! - Whitespace-only messages are rejected
//! - Messages over 100K characters are rejected
//! - Unicode character counting (not byte counting)
//!
//! Addresses PR #4 Critical Issue #5: ChatRequest Validation Missing Dedicated Tests
//!
//! ## Security Impact
//!
//! These validations protect against DoS attacks:
//! - Empty/whitespace messages waste compute resources
//! - Extremely long messages cause memory exhaustion
//!
//! Without tests, these security boundaries are at risk during refactoring.

// Since ChatRequest fields are private and it has a custom Deserialize implementation,
// we test via JSON deserialization which exercises the actual validation logic
type ChatRequest = octoroute::handlers::chat::ChatRequest;

/// Test that empty messages are rejected
///
/// **RED PHASE**: This test should fail if validation is removed
/// **Security**: Prevents DoS via empty message requests
#[test]
fn test_chat_request_rejects_empty_message() {
    let json = r#"{"message": ""}"#;
    let result: Result<ChatRequest, _> = serde_json::from_str(json);

    assert!(
        result.is_err(),
        "Empty message should be rejected by validation"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("empty") || err.to_string().contains("whitespace"),
        "Error message should mention empty or whitespace: {}",
        err
    );
}

/// Test that whitespace-only messages are rejected
///
/// **RED PHASE**: This test should fail if trim() check is removed
/// **Security**: Prevents DoS via whitespace-only requests
#[test]
fn test_chat_request_rejects_whitespace_only_message() {
    let test_cases = vec![
        r#"{"message": " "}"#,      // Single space
        r#"{"message": "   "}"#,    // Multiple spaces
        r#"{"message": "\t"}"#,     // Tab
        r#"{"message": "\n"}"#,     // Newline
        r#"{"message": " \t\n "}"#, // Mixed whitespace
    ];

    for json in test_cases {
        let result: Result<ChatRequest, _> = serde_json::from_str(json);

        assert!(
            result.is_err(),
            "Whitespace-only message should be rejected: {}",
            json
        );

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("empty") || err.to_string().contains("whitespace"),
            "Error message should mention empty or whitespace for input: {}",
            json
        );
    }
}

/// Test that messages over 100K characters are rejected
///
/// **RED PHASE**: This test should fail if MAX_MESSAGE_LENGTH check is removed
/// **Security**: Prevents DoS via extremely long messages causing memory exhaustion
#[test]
fn test_chat_request_rejects_over_100k_chars() {
    // Create a message with 100,001 ASCII characters
    let long_message = "a".repeat(100_001);
    let json = format!(r#"{{"message": "{}"}}"#, long_message);
    let result: Result<ChatRequest, _> = serde_json::from_str(&json);

    assert!(
        result.is_err(),
        "Message with 100,001 characters should be rejected"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("maximum length") || err.to_string().contains("100000"),
        "Error message should mention maximum length: {}",
        err
    );
}

/// Test that messages at exactly 100K characters are accepted
///
/// **GREEN PHASE**: This verifies the boundary condition
#[test]
fn test_chat_request_accepts_exactly_100k_chars() {
    // Create a message with exactly 100,000 ASCII characters
    let message = "a".repeat(100_000);
    let json = format!(r#"{{"message": "{}"}}"#, message);
    let result: Result<ChatRequest, _> = serde_json::from_str(&json);

    assert!(
        result.is_ok(),
        "Message with exactly 100,000 characters should be accepted"
    );
}

/// Test that messages just under 100K characters are accepted
///
/// **GREEN PHASE**: This verifies messages under the limit are accepted
#[test]
fn test_chat_request_accepts_under_100k_chars() {
    // Create a message with 99,999 ASCII characters
    let message = "a".repeat(99_999);
    let json = format!(r#"{{"message": "{}"}}"#, message);
    let result: Result<ChatRequest, _> = serde_json::from_str(&json);

    assert!(
        result.is_ok(),
        "Message with 99,999 characters should be accepted"
    );
}

/// Test Unicode character counting (not byte counting)
///
/// **RED PHASE**: This test verifies that multi-byte characters are counted correctly
/// **Security**: Emoji and other Unicode characters count as 1 character, not multiple bytes
#[test]
fn test_chat_request_unicode_boundary() {
    // Emoji are multi-byte but count as 1 character
    // "😀" is 4 bytes but 1 character
    let emoji_message = "😀".repeat(50_000);
    let char_count = emoji_message.chars().count();
    let byte_count = emoji_message.len();

    assert_eq!(char_count, 50_000, "Should have 50,000 characters");
    assert_eq!(
        byte_count, 200_000,
        "Should have 200,000 bytes (4 bytes per emoji)"
    );

    let json = format!(r#"{{"message": "{}"}}"#, emoji_message);
    let result: Result<ChatRequest, _> = serde_json::from_str(&json);

    assert!(
        result.is_ok(),
        "Message with 50K emoji (50K chars, 200KB bytes) should be accepted"
    );
}

/// Test that Unicode messages over 100K characters are rejected
///
/// **RED PHASE**: This verifies that character counting works for Unicode
#[test]
fn test_chat_request_rejects_unicode_over_100k_chars() {
    // Create a message with 100,001 emoji (100,001 chars, ~400KB bytes)
    let emoji_message = "😀".repeat(100_001);
    let char_count = emoji_message.chars().count();

    assert_eq!(char_count, 100_001, "Should have 100,001 characters");

    let json = format!(r#"{{"message": "{}"}}"#, emoji_message);
    let result: Result<ChatRequest, _> = serde_json::from_str(&json);

    assert!(
        result.is_err(),
        "Message with 100,001 emoji should be rejected (counts characters, not bytes)"
    );
}

/// Test that valid messages with normal content are accepted
///
/// **GREEN PHASE**: This verifies normal operation
#[test]
fn test_chat_request_accepts_valid_message() {
    let json = r#"{"message": "Hello, world!"}"#;
    let result: Result<ChatRequest, _> = serde_json::from_str(json);

    assert!(result.is_ok(), "Valid message should be accepted");

    let req = result.unwrap();
    assert_eq!(req.message(), "Hello, world!");
}

/// Test that messages with leading/trailing whitespace are accepted
/// (only rejected if ENTIRELY whitespace)
///
/// **GREEN PHASE**: This verifies that trim() is only used for validation, not modification
#[test]
fn test_chat_request_preserves_whitespace_in_valid_message() {
    let json = r#"{"message": "  Hello, world!  "}"#;
    let result: Result<ChatRequest, _> = serde_json::from_str(json);

    assert!(
        result.is_ok(),
        "Message with leading/trailing whitespace should be accepted"
    );

    let req = result.unwrap();
    assert_eq!(
        req.message(),
        "  Hello, world!  ",
        "Whitespace should be preserved in valid messages"
    );
}

/// Test that importance and task_type fields have defaults
///
/// **GREEN PHASE**: This verifies the #[serde(default)] attributes
#[test]
fn test_chat_request_has_default_fields() {
    let json = r#"{"message": "test"}"#;
    let result: Result<ChatRequest, _> = serde_json::from_str(json);

    assert!(
        result.is_ok(),
        "Message without importance/task_type should use defaults"
    );
}
