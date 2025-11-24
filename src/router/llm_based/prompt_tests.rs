//! Tests for build_router_prompt function

use super::*;
use crate::router::{Importance, RouteMetadata, TaskType};

#[test]
fn test_build_router_prompt_contains_user_prompt() {
    let user_prompt = "Explain quantum entanglement";
    let meta = RouteMetadata {
        token_estimate: 500,
        importance: Importance::Normal,
        task_type: TaskType::QuestionAnswer,
    };

    let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);
    assert!(prompt.contains(user_prompt));
}

#[test]
fn test_build_router_prompt_contains_metadata() {
    let user_prompt = "Hello";
    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::High,
        task_type: TaskType::CasualChat,
    };

    let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

    // Check that metadata is included
    assert!(prompt.contains("100")); // token_estimate
    assert!(prompt.contains("High")); // importance
    assert!(prompt.contains("CasualChat")); // task_type
}

#[test]
fn test_build_router_prompt_contains_model_options() {
    let user_prompt = "Test";
    let meta = RouteMetadata {
        token_estimate: 50,
        importance: Importance::Normal,
        task_type: TaskType::QuestionAnswer,
    };

    let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

    // Check that all three model options are mentioned
    assert!(prompt.contains("FAST"));
    assert!(prompt.contains("BALANCED"));
    assert!(prompt.contains("DEEP"));
}

#[test]
fn test_build_router_prompt_contains_instructions() {
    let user_prompt = "Test";
    let meta = RouteMetadata {
        token_estimate: 50,
        importance: Importance::Normal,
        task_type: TaskType::QuestionAnswer,
    };

    let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

    // Check that it contains instruction to respond with ONLY one of the options
    assert!(prompt.to_uppercase().contains("ONLY") || prompt.to_uppercase().contains("RESPOND"));
}

#[test]
fn test_build_router_prompt_formatting() {
    let user_prompt = "Write a function to calculate fibonacci";
    let meta = RouteMetadata {
        token_estimate: 250,
        importance: Importance::Normal,
        task_type: TaskType::Code,
    };

    let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

    // Verify it's not empty and has reasonable length
    assert!(!prompt.is_empty());
    assert!(prompt.len() > 100); // Should be a substantial prompt

    // Verify key sections are present
    assert!(prompt.contains("router"));
    assert!(prompt.contains("User request:") || prompt.contains("User:"));
    assert!(prompt.contains("Metadata:") || prompt.contains("metadata"));
}
