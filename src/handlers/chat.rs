//! Chat endpoint handler
//!
//! Handles POST /chat requests with intelligent model routing.

use crate::config::ModelEndpoint;
use crate::error::AppError;
use crate::handlers::AppState;
use crate::middleware::RequestId;
use crate::router::{Importance, RouteMetadata, RoutingStrategy, TargetModel, TaskType};
use crate::shared::query::{QueryConfig, execute_query_with_retry, record_routing_metrics};
use axum::{Extension, Json, extract::State, response::IntoResponse};
use serde::{Deserialize, Deserializer, Serialize};

/// Maximum allowed message length in characters (100K chars)
const MAX_MESSAGE_LENGTH: usize = 100_000;

/// Chat request from client
///
/// Validation is enforced during deserialization - invalid instances cannot exist.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    message: String,
    importance: Importance,
    task_type: TaskType,
}

impl ChatRequest {
    /// Get the message
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Get the importance level
    pub fn importance(&self) -> Importance {
        self.importance
    }

    /// Get the task type
    pub fn task_type(&self) -> TaskType {
        self.task_type
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

/// Custom Deserialize implementation that validates during deserialization
impl<'de> Deserialize<'de> for ChatRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawChatRequest {
            message: String,
            #[serde(default)]
            importance: Importance,
            #[serde(default)]
            task_type: TaskType,
        }

        let raw = RawChatRequest::deserialize(deserializer)?;

        // Validate message is not empty or whitespace-only
        if raw.message.trim().is_empty() {
            return Err(serde::de::Error::custom(
                "message cannot be empty or contain only whitespace",
            ));
        }

        // Validate message length (count Unicode characters, not bytes)
        let char_count = raw.message.chars().count();
        if char_count > MAX_MESSAGE_LENGTH {
            return Err(serde::de::Error::custom(format!(
                "message exceeds maximum length of {} characters (got {})",
                MAX_MESSAGE_LENGTH, char_count
            )));
        }

        Ok(ChatRequest {
            message: raw.message,
            importance: raw.importance,
            task_type: raw.task_type,
        })
    }
}

/// Model tier classification for API responses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Fast,
    Balanced,
    Deep,
}

impl From<crate::router::TargetModel> for ModelTier {
    fn from(target: crate::router::TargetModel) -> Self {
        match target {
            crate::router::TargetModel::Fast => ModelTier::Fast,
            crate::router::TargetModel::Balanced => ModelTier::Balanced,
            crate::router::TargetModel::Deep => ModelTier::Deep,
        }
    }
}

/// Chat response to client
///
/// Fields are private to enforce construction through the validated `new()` constructor.
/// This ensures `model_name` always matches an actual endpoint from configuration.
///
/// ## Warnings
///
/// The `warnings` field surfaces non-fatal issues (e.g., health tracking failures)
/// to users. Warnings are omitted from JSON if empty.
#[derive(Debug, Clone, Serialize)]
pub struct ChatResponse {
    /// Model's response content
    content: String,
    /// Which model tier was used
    model_tier: ModelTier,
    /// Which specific endpoint was used
    model_name: String,
    /// Which routing strategy made the decision (Rule or Llm)
    routing_strategy: RoutingStrategy,
    /// Non-fatal warnings encountered during routing (omitted if empty)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

impl ChatResponse {
    /// Create a new ChatResponse with guaranteed consistency between endpoint and model_name
    ///
    /// Use this constructor in production code to ensure `model_name` always matches
    /// an actual endpoint from the configuration.
    ///
    /// # Arguments
    /// * `content` - The model's response text
    /// * `endpoint` - The endpoint that generated the response (guarantees valid model_name)
    /// * `tier` - The tier used for routing (fast, balanced, deep)
    /// * `routing_strategy` - Which routing strategy was used (Rule or Llm)
    pub fn new(
        content: String,
        endpoint: &ModelEndpoint,
        tier: TargetModel,
        routing_strategy: RoutingStrategy,
    ) -> Self {
        Self {
            content,
            model_tier: tier.into(),
            model_name: endpoint.name().to_string(),
            routing_strategy,
            warnings: Vec::new(),
        }
    }

    /// Create a new ChatResponse with warnings
    ///
    /// Like `new()` but includes warnings collected during routing (e.g., health tracking failures).
    ///
    /// # Arguments
    /// * `content` - The model's response text
    /// * `endpoint` - The endpoint that generated the response (guarantees valid model_name)
    /// * `tier` - The tier used for routing (fast, balanced, deep)
    /// * `routing_strategy` - Which routing strategy was used (Rule or Llm)
    /// * `warnings` - Non-fatal warnings to surface to the user
    pub fn new_with_warnings(
        content: String,
        endpoint: &ModelEndpoint,
        tier: TargetModel,
        routing_strategy: RoutingStrategy,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            content,
            model_tier: tier.into(),
            model_name: endpoint.name().to_string(),
            routing_strategy,
            warnings,
        }
    }

    /// Get the response content
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Get the model tier used
    pub fn model_tier(&self) -> ModelTier {
        self.model_tier
    }

    /// Get the model name (endpoint) used
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Get the routing strategy used
    pub fn routing_strategy(&self) -> RoutingStrategy {
        self.routing_strategy
    }

    /// Get the warnings collected during routing
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// Custom Deserialize implementation for ChatResponse that validates fields
///
/// Defense-in-depth: Prevents creation of invalid ChatResponse instances via deserialization,
/// even though ChatResponse is never deserialized from user input in current code.
impl<'de> Deserialize<'de> for ChatResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawChatResponse {
            content: String,
            model_tier: ModelTier,
            model_name: String,
            routing_strategy: RoutingStrategy,
            #[serde(default)]
            warnings: Vec<String>,
        }

        let raw = RawChatResponse::deserialize(deserializer)?;

        // Validate content is not empty
        if raw.content.trim().is_empty() {
            return Err(serde::de::Error::custom("content cannot be empty"));
        }

        // Validate model_name is not empty
        if raw.model_name.trim().is_empty() {
            return Err(serde::de::Error::custom("model_name cannot be empty"));
        }

        Ok(ChatResponse {
            content: raw.content,
            model_tier: raw.model_tier,
            model_name: raw.model_name,
            routing_strategy: raw.routing_strategy,
            warnings: raw.warnings,
        })
    }
}

/// POST /chat handler
///
/// # Async Latency Characteristics
///
/// This handler is async with variable latency depending on routing strategy:
///
/// - **Rule-based routing** (~1ms): Fast, deterministic routing with immediate endpoint selection
/// - **LLM-based routing** (~100-500ms): Intelligent routing using 30B model for decision-making
/// - **Model query execution** (variable): Depends on selected model tier and prompt complexity
///
/// ## Worst-Case Latency
///
/// With MAX_RETRIES=3 and 30s timeout per attempt:
/// - **Total maximum latency**: 90 seconds (3 attempts × 30s timeout)
/// - **Typical latency**: 1-5 seconds for successful requests
///
/// ## Blocking Points
///
/// - Routing decision (rule or LLM-based)
/// - Endpoint selection with health checking
/// - Model query with streaming response
/// - Health state updates (async lock acquisition)
pub async fn handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<ChatRequest>,
) -> Result<impl IntoResponse, AppError> {
    tracing::debug!(
        request_id = %request_id,
        message_length = request.message().len(),
        importance = ?request.importance(),
        task_type = ?request.task_type(),
        "Received chat request"
    );

    // Convert to metadata for routing
    let metadata = request.to_metadata();

    // Use router to determine target tier
    let routing_start = std::time::Instant::now();
    let decision = state
        .router()
        .route(request.message(), &metadata, state.selector())
        .await?;
    let routing_duration_ms = routing_start.elapsed().as_secs_f64() * 1000.0;

    tracing::info!(
        request_id = %request_id,
        target_tier = ?decision.target(),
        routing_strategy = ?decision.strategy(),
        token_estimate = metadata.token_estimate,
        routing_duration_ms = %routing_duration_ms,
        "Routing decision made"
    );

    // Record routing metrics
    record_routing_metrics(&state, &decision, routing_duration_ms, request_id);

    // Execute query with retry logic (uses shared module)
    // Legacy chat endpoint doesn't support sampling parameters - use endpoint defaults
    let config = QueryConfig::default();
    let result = execute_query_with_retry(
        &state,
        &decision,
        request.message(),
        request_id,
        &config,
        None,
    )
    .await?;

    // Build response
    let response = if result.warnings.is_empty() {
        ChatResponse::new(
            result.content,
            &result.endpoint,
            result.tier,
            result.strategy,
        )
    } else {
        ChatResponse::new_with_warnings(
            result.content,
            &result.endpoint,
            result.tier,
            result.strategy,
            result.warnings,
        )
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

        assert_eq!(req.message(), "Hello!");
        assert_eq!(req.importance(), Importance::Normal); // default
        assert_eq!(req.task_type(), TaskType::QuestionAnswer); // default
    }

    #[test]
    fn test_chat_request_with_importance() {
        let json = r#"{"message": "Urgent!", "importance": "high"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.message(), "Urgent!");
        assert_eq!(req.importance(), Importance::High);
    }

    #[test]
    fn test_chat_request_with_task_type() {
        let json = r#"{"message": "Write code", "task_type": "code"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.task_type(), TaskType::Code);
    }

    #[test]
    fn test_chat_request_rejects_empty_message() {
        let json = r#"{"message": ""}"#;
        let result = serde_json::from_str::<ChatRequest>(json);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty") || err_msg.contains("whitespace"),
            "error message should mention empty or whitespace, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chat_request_rejects_whitespace_only_message() {
        let json = r#"{"message": "   \n\t  "}"#;
        let result = serde_json::from_str::<ChatRequest>(json);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty") || err_msg.contains("whitespace"),
            "error message should mention empty or whitespace, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chat_request_rejects_message_too_long() {
        let long_message = "a".repeat(100_001); // Exceeds MAX_MESSAGE_LENGTH (characters)
        let json = format!(r#"{{"message": "{}"}}"#, long_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds maximum length") || err_msg.contains("100000"),
            "error message should mention exceeds maximum length, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chat_request_accepts_emoji_at_char_limit() {
        // Emoji are 4 bytes each in UTF-8, but should count as 1 character
        // 100,000 emojis = 400,000 bytes but only 100,000 characters
        let emoji_message = "👋".repeat(100_000);
        let json = format!(r#"{{"message": "{}"}}"#, emoji_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_ok(),
            "100K emoji chars (400K bytes) should be accepted. Error: {:?}",
            result.err()
        );
        let req = result.unwrap();
        assert_eq!(req.message().chars().count(), 100_000);
    }

    #[test]
    fn test_chat_request_rejects_emoji_over_char_limit() {
        // 100,001 emojis = 400,004 bytes but 100,001 characters
        let emoji_message = "👋".repeat(100_001);
        let json = format!(r#"{{"message": "{}"}}"#, emoji_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_err(),
            "100,001 emoji chars should be rejected regardless of byte count"
        );
    }

    #[test]
    fn test_chat_request_accepts_cjk_at_char_limit() {
        // CJK characters are 3 bytes each in UTF-8, but should count as 1 character
        // 100,000 Chinese chars = 300,000 bytes but only 100,000 characters
        let cjk_message = "你".repeat(100_000);
        let json = format!(r#"{{"message": "{}"}}"#, cjk_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_ok(),
            "100K CJK chars (300K bytes) should be accepted. Error: {:?}",
            result.err()
        );
        let req = result.unwrap();
        assert_eq!(req.message().chars().count(), 100_000);
    }

    #[test]
    fn test_chat_request_rejects_cjk_over_char_limit() {
        // 100,001 Chinese characters = 300,003 bytes but 100,001 characters
        let cjk_message = "你".repeat(100_001);
        let json = format!(r#"{{"message": "{}"}}"#, cjk_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_err(),
            "100,001 CJK chars should be rejected regardless of byte count"
        );
    }

    #[test]
    fn test_chat_request_accepts_valid_message() {
        let json = r#"{"message": "Hello, world!"}"#;
        let result = serde_json::from_str::<ChatRequest>(json);

        assert!(result.is_ok());
        let req = result.unwrap();
        assert_eq!(req.message(), "Hello, world!");
    }

    #[test]
    fn test_chat_request_to_metadata() {
        let json =
            r#"{"message": "What is 2+2?", "importance": "low", "task_type": "casual_chat"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        let meta = req.to_metadata();
        assert_eq!(meta.importance, Importance::Low);
        assert_eq!(meta.task_type, TaskType::CasualChat);
        assert!(meta.token_estimate > 0);
    }

    #[test]
    fn test_chat_response_serializes() {
        // Use constructor instead of struct literal (fields are now private)
        let toml = r#"
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1
"#;
        let endpoint: ModelEndpoint = toml::from_str(toml).expect("should parse endpoint");

        let resp = ChatResponse::new(
            "4".to_string(),
            &endpoint,
            TargetModel::Fast,
            RoutingStrategy::Rule,
        );

        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"content\":\"4\""));
        assert!(json.contains("\"model_tier\":\"fast\""));
        assert!(json.contains("\"routing_strategy\":\"rule\""));
    }

    #[test]
    fn test_chat_response_constructor_and_accessors() {
        // Create a mock endpoint using TOML deserialization
        let toml = r#"
name = "test-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1
"#;
        let endpoint: ModelEndpoint = toml::from_str(toml).expect("should parse endpoint");

        // Create ChatResponse using constructor
        let response = ChatResponse::new(
            "Test response".to_string(),
            &endpoint,
            TargetModel::Fast,
            RoutingStrategy::Rule,
        );

        // Verify accessors work
        assert_eq!(response.content(), "Test response");
        assert_eq!(response.model_tier(), ModelTier::Fast);
        assert_eq!(response.model_name(), "test-model");
        assert_eq!(response.routing_strategy(), RoutingStrategy::Rule);
    }

    #[test]
    fn test_chat_response_serde_with_constructor() {
        // Create a mock endpoint
        let toml = r#"
name = "test-balanced"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1
"#;
        let endpoint: ModelEndpoint = toml::from_str(toml).expect("should parse endpoint");

        let response = ChatResponse::new(
            "Serialization test".to_string(),
            &endpoint,
            TargetModel::Balanced,
            RoutingStrategy::Llm,
        );

        // Verify serialization works (will work even with private fields)
        let json = serde_json::to_string(&response).expect("should serialize");
        assert!(json.contains("Serialization test"));
        assert!(json.contains("balanced"));
        assert!(json.contains("test-balanced"));
        assert!(json.contains("llm"));

        // Verify deserialization works (serde works with private fields)
        let deserialized: ChatResponse = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized.content(), "Serialization test");
        assert_eq!(deserialized.model_tier(), ModelTier::Balanced);
        assert_eq!(deserialized.model_name(), "test-balanced");
        assert_eq!(deserialized.routing_strategy(), RoutingStrategy::Llm);
    }

    #[test]
    fn test_chatresponse_deserialize_rejects_empty_content() {
        // GAP #7: ChatResponse custom Deserialize validation
        //
        // ChatResponse has a custom Deserialize implementation that validates
        // content and model_name are non-empty (lines 181-211).
        //
        // This is defense-in-depth: ChatResponse is never actually deserialized
        // from user input (only serialized for output), but the validation
        // prevents future footguns if code changes.
        //
        // Verifies: Empty content is rejected during deserialization

        let json = r#"{
            "content": "",
            "model_tier": "balanced",
            "model_name": "test-model",
            "routing_strategy": "rule"
        }"#;

        let result = serde_json::from_str::<ChatResponse>(json);
        assert!(
            result.is_err(),
            "ChatResponse with empty content should fail deserialization"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("content") && err_msg.contains("empty"),
            "Error should mention 'content' and 'empty', got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chatresponse_deserialize_rejects_whitespace_only_content() {
        // Verify that whitespace-only content is also rejected (trim() check)

        let json = r#"{
            "content": "   \n\t  ",
            "model_tier": "balanced",
            "model_name": "test-model",
            "routing_strategy": "rule"
        }"#;

        let result = serde_json::from_str::<ChatResponse>(json);
        assert!(
            result.is_err(),
            "ChatResponse with whitespace-only content should fail deserialization"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("content") && err_msg.contains("empty"),
            "Error should mention 'content' and 'empty', got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chatresponse_deserialize_rejects_empty_model_name() {
        // GAP #8: ChatResponse model_name validation
        //
        // Verifies: Empty model_name is rejected during deserialization

        let json = r#"{
            "content": "Some response",
            "model_tier": "balanced",
            "model_name": "",
            "routing_strategy": "rule"
        }"#;

        let result = serde_json::from_str::<ChatResponse>(json);
        assert!(
            result.is_err(),
            "ChatResponse with empty model_name should fail deserialization"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("model_name") && err_msg.contains("empty"),
            "Error should mention 'model_name' and 'empty', got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chatresponse_deserialize_rejects_whitespace_only_model_name() {
        // Verify that whitespace-only model_name is also rejected

        let json = r#"{
            "content": "Some response",
            "model_tier": "balanced",
            "model_name": "  \t\n  ",
            "routing_strategy": "rule"
        }"#;

        let result = serde_json::from_str::<ChatResponse>(json);
        assert!(
            result.is_err(),
            "ChatResponse with whitespace-only model_name should fail deserialization"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("model_name") && err_msg.contains("empty"),
            "Error should mention 'model_name' and 'empty', got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chatresponse_deserialize_accepts_valid_response() {
        // Verify that valid responses are accepted

        let json = r#"{
            "content": "Valid response content",
            "model_tier": "deep",
            "model_name": "gpt-oss-120b",
            "routing_strategy": "llm"
        }"#;

        let result = serde_json::from_str::<ChatResponse>(json);
        assert!(
            result.is_ok(),
            "ChatResponse with valid fields should deserialize successfully. Error: {:?}",
            result.err()
        );

        let response = result.unwrap();
        assert_eq!(response.content(), "Valid response content");
        assert_eq!(response.model_tier(), ModelTier::Deep);
        assert_eq!(response.model_name(), "gpt-oss-120b");
        assert_eq!(response.routing_strategy(), RoutingStrategy::Llm);
    }
}
