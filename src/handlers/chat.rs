//! Chat endpoint handler
//!
//! Handles POST /chat requests with intelligent model routing and streaming responses.

use crate::error::{AppError, AppResult};
use crate::handlers::AppState;
use crate::router::{Importance, RouteMetadata, TaskType};
use axum::{Json, extract::State, response::IntoResponse};
use serde::{Deserialize, Serialize};

/// Chat request from client
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatRequest {
    /// User's message/prompt
    pub message: String,
    /// Optional importance level (defaults to normal)
    #[serde(default)]
    pub importance: Importance,
    /// Optional task type classification
    #[serde(default)]
    pub task_type: TaskType,
}

impl ChatRequest {
    /// Validate the chat request
    pub fn validate(&self) -> AppResult<()> {
        if self.message.trim().is_empty() {
            return Err(AppError::Validation("message cannot be empty".to_string()));
        }
        Ok(())
    }

    /// Convert request to RouteMetadata for routing decisions
    pub fn to_metadata(&self) -> RouteMetadata {
        let token_estimate = RouteMetadata::estimate_tokens(&self.message);
        RouteMetadata {
            token_estimate,
            importance: self.importance,
            task_type: self.task_type,
        }
    }
}

/// Chat response to client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Model's response content
    pub content: String,
    /// Which model tier was used
    pub model_tier: String,
    /// Which specific endpoint was used
    pub model_name: String,
}

/// POST /chat handler
pub async fn handler(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate request
    request.validate()?;

    // Convert to metadata for routing
    let metadata = request.to_metadata();

    // Use rule-based router to determine target tier, with fallback to default
    let target = state.router().route(&metadata).unwrap_or_else(|| {
        // When no rule matches, fall back to balanced tier as a sensible default
        // This handles common cases like simple questions that don't match specific rules
        use crate::router::TargetModel;
        TargetModel::Balanced
    });

    // Select specific endpoint from the target tier
    let endpoint = state
        .selector()
        .select(target)
        .ok_or_else(|| {
            AppError::RoutingFailed(format!("No available endpoints for tier {:?}", target))
        })?
        .clone();

    // Build AgentOptions from selected endpoint
    let options = open_agent::AgentOptions::builder()
        .model(&endpoint.name)
        .base_url(&endpoint.base_url)
        .max_tokens(endpoint.max_tokens as u32)
        .temperature(endpoint.temperature as f32)
        .build()
        .map_err(|e| AppError::Internal(format!("Failed to build AgentOptions: {}", e)))?;

    // Query the model using the standalone function (avoids !Sync issues)
    use futures::StreamExt;
    let mut stream = open_agent::query(&request.message, &options)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to query model: {}", e)))?;

    // Collect response from stream
    let mut response_text = String::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(block) => {
                use open_agent::ContentBlock;
                if let ContentBlock::Text(text_block) = block {
                    response_text.push_str(&text_block.text);
                }
            }
            Err(e) => {
                return Err(AppError::Internal(format!("Stream error: {}", e)));
            }
        }
    }

    // Build response
    let response = ChatResponse {
        content: response_text,
        model_tier: format!("{:?}", target).to_lowercase(),
        model_name: endpoint.name.clone(),
    };

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_request_deserializes() {
        let json = r#"{"message": "Hello!"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.message, "Hello!");
        assert_eq!(req.importance, Importance::Normal); // default
        assert_eq!(req.task_type, TaskType::QuestionAnswer); // default
    }

    #[test]
    fn test_chat_request_with_importance() {
        let json = r#"{"message": "Urgent!", "importance": "high"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.message, "Urgent!");
        assert_eq!(req.importance, Importance::High);
    }

    #[test]
    fn test_chat_request_with_task_type() {
        let json = r#"{"message": "Write code", "task_type": "code"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.task_type, TaskType::Code);
    }

    #[test]
    fn test_chat_request_validate_empty_message() {
        let req = ChatRequest {
            message: "".to_string(),
            importance: Importance::Normal,
            task_type: TaskType::QuestionAnswer,
        };

        let result = req.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_chat_request_validate_valid_message() {
        let req = ChatRequest {
            message: "Hello, world!".to_string(),
            importance: Importance::Normal,
            task_type: TaskType::QuestionAnswer,
        };

        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_chat_request_to_metadata() {
        let req = ChatRequest {
            message: "What is 2+2?".to_string(),
            importance: Importance::Low,
            task_type: TaskType::CasualChat,
        };

        let meta = req.to_metadata();
        assert_eq!(meta.importance, Importance::Low);
        assert_eq!(meta.task_type, TaskType::CasualChat);
        assert!(meta.token_estimate > 0);
    }

    #[test]
    fn test_chat_response_serializes() {
        let resp = ChatResponse {
            content: "4".to_string(),
            model_tier: "fast".to_string(),
            model_name: "fast-1".to_string(),
        };

        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"content\":\"4\""));
        assert!(json.contains("\"model_tier\":\"fast\""));
    }
}
