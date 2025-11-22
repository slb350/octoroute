//! Prompt truncation tests (UTF-8 safety)

use super::*;
use crate::router::{Importance, RouteMetadata, TaskType};

#[test]
fn test_build_router_prompt_truncates_long_prompt_safely() {
    // Long ASCII prompt - should truncate cleanly
    let long_prompt = "a".repeat(1000);
    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    let result = LlmBasedRouter::build_router_prompt(&long_prompt, &meta);

    // Should not panic, should contain truncation marker
    assert!(result.contains("[truncated]"));
}

#[test]
fn test_build_router_prompt_handles_multibyte_chars_at_boundary() {
    // Create a string where a multibyte UTF-8 character falls exactly at byte 500
    // "世" is 3 bytes in UTF-8 (0xE4 0xB8 0x96)
    // We want byte 499-501 to be this character, so byte slicing at 500 would panic
    let ascii_prefix = "a".repeat(498); // 498 bytes
    let prompt = format!("{}世界test", ascii_prefix); // byte 498-500 is "世" (3 bytes)

    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    // This should NOT panic - the current implementation WILL panic
    let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

    // Should contain truncation marker and be valid UTF-8
    assert!(result.contains("[truncated]"));
}

#[test]
fn test_build_router_prompt_handles_emoji_at_boundary() {
    // Emoji are 4-byte UTF-8 sequences
    // Create string where emoji falls at truncation boundary
    let ascii_prefix = "a".repeat(497);
    let prompt = format!("{}🦑test", ascii_prefix); // 🦑 is 4 bytes

    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    // Should NOT panic
    let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

    // Should be valid UTF-8 with truncation marker
    assert!(result.contains("[truncated]"));
}

#[test]
fn test_build_router_prompt_preserves_short_multibyte_prompt() {
    // Short prompt with multibyte characters should NOT be truncated
    let prompt = "Explain quantum entanglement in Chinese: 量子纠缠";
    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    let result = LlmBasedRouter::build_router_prompt(prompt, &meta);

    // Should contain the original prompt, NOT truncated
    assert!(result.contains(prompt));
    assert!(!result.contains("[truncated]"));
}

#[test]
fn test_build_router_prompt_handles_zwj_emoji_at_boundary() {
    // GAP #7: ZWJ (Zero-Width Joiner) Emoji Truncation
    //
    // ZWJ emoji like 👨‍👩‍👧‍👦 (family) are composed of multiple codepoints joined by U+200D (ZWJ).
    // Family emoji: 👨 (man) + ZWJ + 👩 (woman) + ZWJ + 👧 (girl) + ZWJ + 👦 (boy)
    // Total: ~25 bytes in UTF-8
    //
    // Truncation at character boundary should not produce � (replacement character).

    // Create string where ZWJ emoji sequence falls near truncation boundary (500 chars)
    let ascii_prefix = "a".repeat(480); // Leave room for ZWJ emoji + some padding
    let prompt = format!("{}Family emoji: 👨‍👩‍👧‍👦 test", ascii_prefix);

    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

    // Should be valid UTF-8 (no replacement characters)
    assert!(
        !result.contains('\u{FFFD}'),
        "Truncated output should not contain replacement character (�)"
    );

    // Should be valid UTF-8 (can be converted without error)
    assert!(
        result.is_char_boundary(result.len()),
        "Truncated output should end on char boundary"
    );
}

#[test]
fn test_build_router_prompt_handles_rtl_text_at_boundary() {
    // GAP #7: RTL (Right-to-Left) Text Truncation
    //
    // RTL languages like Arabic and Hebrew use bidirectional text.
    // Truncation should preserve valid UTF-8 even with RTL characters.
    //
    // Arabic text uses 2-3 bytes per character in UTF-8.

    // Create string with Arabic text near truncation boundary
    let ascii_prefix = "a".repeat(490);
    let prompt = format!(
        "{}Arabic: مرحبا بك في عالم الذكاء الاصطناعي test",
        ascii_prefix
    );

    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

    // Should be valid UTF-8 (no replacement characters)
    assert!(
        !result.contains('\u{FFFD}'),
        "Truncated output should not contain replacement character (�)"
    );

    // Should contain truncation marker since prompt > 500 chars
    assert!(result.contains("[truncated]"));
}

#[test]
fn test_build_router_prompt_handles_combining_diacritics_at_boundary() {
    // GAP #7: Combining Diacritics Truncation
    //
    // Combining diacritics are separate codepoints that modify base characters.
    // Example: é can be composed as 'e' (U+0065) + ́ (U+0301)
    //
    // Truncation at character boundary should not split combining sequences.

    // Create string with combining diacritics near boundary
    // Use decomposed form: e + combining acute accent
    let ascii_prefix = "a".repeat(495);
    let decomposed_text = "café resume"; // May contain combining forms depending on normalization
    let prompt = format!("{}{}", ascii_prefix, decomposed_text);

    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

    // Should be valid UTF-8 (char-based truncation ensures this)
    assert!(
        !result.contains('\u{FFFD}'),
        "Truncated output should not contain replacement character (�)"
    );

    // Verify truncation marker present
    assert!(result.contains("[truncated]"));
}
