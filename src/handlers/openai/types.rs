//! OpenAI-compatible request and response types
//!
//! These types follow the OpenAI Chat Completions API specification.
//! Validation is enforced during deserialization - invalid instances cannot exist.

use crate::router::{Importance, RouteMetadata, TargetModel, TaskType};
use serde::{Deserialize, Deserializer, Serialize};

/// Maximum allowed total content length across all messages (500K chars)
const MAX_TOTAL_CONTENT_LENGTH: usize = 500_000;
/// Maximum number of messages allowed
const MAX_MESSAGES: usize = 100;

// =============================================================================
// OpenAI API Object Type Constants
// =============================================================================

/// Object type for non-streaming chat completion responses
pub const OBJECT_CHAT_COMPLETION: &str = "chat.completion";
/// Object type for streaming chat completion chunks
pub const OBJECT_CHAT_COMPLETION_CHUNK: &str = "chat.completion.chunk";
/// Object type for list responses (e.g., model list)
pub const OBJECT_LIST: &str = "list";
/// Object type for individual model entries
pub const OBJECT_MODEL: &str = "model";

// =============================================================================
// Shared Validation Logic
// =============================================================================

/// Validate ChatCompletionRequest fields
///
/// This is the single source of truth for request validation, used by both
/// the builder and serde deserializer to ensure consistent validation rules.
fn validate_request_fields(
    messages: &[ChatMessage],
    temperature: Option<f64>,
    top_p: Option<f64>,
    presence_penalty: Option<f64>,
    frequency_penalty: Option<f64>,
    max_tokens: Option<u32>,
) -> Result<(), String> {
    // Validation 1: Messages array not empty
    if messages.is_empty() {
        return Err("messages array cannot be empty".to_string());
    }

    // Validation 2: Message count limit
    if messages.len() > MAX_MESSAGES {
        return Err(format!(
            "messages array cannot exceed {} messages (got {})",
            MAX_MESSAGES,
            messages.len()
        ));
    }

    // Validation 3: Total content length
    let total_length: usize = messages.iter().map(|m| m.content_length()).sum();
    if total_length > MAX_TOTAL_CONTENT_LENGTH {
        return Err(format!(
            "total content length exceeds {} characters (got {})",
            MAX_TOTAL_CONTENT_LENGTH, total_length
        ));
    }

    // Validation 4: Temperature range [0.0, 2.0]
    if let Some(temp) = temperature {
        if temp.is_nan() || temp.is_infinite() {
            return Err("temperature must be a finite number".to_string());
        }
        if !(0.0..=2.0).contains(&temp) {
            return Err("temperature must be between 0.0 and 2.0".to_string());
        }
    }

    // Validation 5: top_p range (0.0, 1.0]
    if let Some(top_p) = top_p {
        if top_p.is_nan() || top_p.is_infinite() {
            return Err("top_p must be a finite number".to_string());
        }
        if top_p <= 0.0 || top_p > 1.0 {
            return Err("top_p must be between 0.0 (exclusive) and 1.0 (inclusive)".to_string());
        }
    }

    // Validation 6: presence_penalty range [-2.0, 2.0]
    if let Some(pp) = presence_penalty {
        if pp.is_nan() || pp.is_infinite() {
            return Err("presence_penalty must be a finite number".to_string());
        }
        if !((-2.0)..=2.0).contains(&pp) {
            return Err("presence_penalty must be between -2.0 and 2.0".to_string());
        }
    }

    // Validation 7: frequency_penalty range [-2.0, 2.0]
    if let Some(fp) = frequency_penalty {
        if fp.is_nan() || fp.is_infinite() {
            return Err("frequency_penalty must be a finite number".to_string());
        }
        if !((-2.0)..=2.0).contains(&fp) {
            return Err("frequency_penalty must be between -2.0 and 2.0".to_string());
        }
    }

    // Validation 8: max_tokens > 0
    if let Some(max) = max_tokens
        && max == 0
    {
        return Err("max_tokens must be greater than 0".to_string());
    }

    Ok(())
}

// =============================================================================
// Model Choice - Maps OpenAI `model` field to Octoroute tiers
// =============================================================================

/// Model selection for routing
///
/// Maps OpenAI `model` field to Octoroute's tier system.
/// Supports tier-based selection and pass-through model names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelChoice {
    /// Auto-route based on request analysis (default)
    #[default]
    Auto,
    /// Route to Fast tier (smaller, faster models)
    Fast,
    /// Route to Balanced tier (medium-capability models)
    Balanced,
    /// Route to Deep tier (largest, highest-capability models)
    Deep,
    /// Pass-through: specific model name (bypasses routing)
    Specific(String),
}

impl<'de> Deserialize<'de> for ModelChoice {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.to_lowercase().as_str() {
            "auto" => ModelChoice::Auto,
            "fast" => ModelChoice::Fast,
            "balanced" => ModelChoice::Balanced,
            "deep" => ModelChoice::Deep,
            _ => {
                // Validate that specific model names are non-empty
                if s.trim().is_empty() {
                    return Err(serde::de::Error::custom("model name cannot be empty"));
                }
                ModelChoice::Specific(s)
            }
        })
    }
}

impl Serialize for ModelChoice {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ModelChoice::Auto => serializer.serialize_str("auto"),
            ModelChoice::Fast => serializer.serialize_str("fast"),
            ModelChoice::Balanced => serializer.serialize_str("balanced"),
            ModelChoice::Deep => serializer.serialize_str("deep"),
            ModelChoice::Specific(name) => serializer.serialize_str(name),
        }
    }
}

impl ModelChoice {
    /// Create a validated Specific variant
    ///
    /// Use this constructor instead of directly constructing `ModelChoice::Specific`
    /// to ensure the model name is validated (non-empty, not whitespace-only).
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty or whitespace-only.
    ///
    /// # Examples
    ///
    /// ```
    /// use octoroute::handlers::openai::types::ModelChoice;
    ///
    /// let valid = ModelChoice::try_specific("qwen3-8b").unwrap();
    /// assert!(valid.is_specific());
    ///
    /// let invalid = ModelChoice::try_specific("");
    /// assert!(invalid.is_err());
    /// ```
    pub fn try_specific(name: impl Into<String>) -> Result<Self, &'static str> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("model name cannot be empty or whitespace-only");
        }
        Ok(ModelChoice::Specific(name))
    }

    /// Convert to TargetModel if tier-based
    ///
    /// Returns `None` for Auto (requires routing) and Specific (bypass routing)
    pub fn to_target_model(&self) -> Option<TargetModel> {
        match self {
            ModelChoice::Fast => Some(TargetModel::Fast),
            ModelChoice::Balanced => Some(TargetModel::Balanced),
            ModelChoice::Deep => Some(TargetModel::Deep),
            ModelChoice::Auto | ModelChoice::Specific(_) => None,
        }
    }

    /// Check if auto-routing should be used
    pub fn requires_routing(&self) -> bool {
        matches!(self, ModelChoice::Auto)
    }

    /// Check if this is a specific model name (bypass routing)
    pub fn is_specific(&self) -> bool {
        matches!(self, ModelChoice::Specific(_))
    }

    /// Get the specific model name if this is a Specific variant
    pub fn specific_name(&self) -> Option<&str> {
        match self {
            ModelChoice::Specific(name) => Some(name),
            _ => None,
        }
    }
}

// =============================================================================
// Message Types
// =============================================================================

/// Message role in the conversation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// A single message in the conversation
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    role: MessageRole,
    content: String,
}

impl ChatMessage {
    /// Create a new message with validation
    ///
    /// # Errors
    /// Returns an error if:
    /// - Content is empty or whitespace-only for User or System roles
    ///
    /// Assistant messages can have empty content (for function calls, partial responses).
    pub fn try_new(role: MessageRole, content: impl Into<String>) -> Result<Self, &'static str> {
        let content = content.into();
        if content.trim().is_empty() && role != MessageRole::Assistant {
            return Err("content cannot be empty for user/system messages");
        }
        Ok(Self { role, content })
    }

    /// Get the role
    pub fn role(&self) -> MessageRole {
        self.role
    }

    /// Get the content
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Get content length in characters (Unicode-aware)
    pub fn content_length(&self) -> usize {
        self.content.chars().count()
    }
}

impl<'de> Deserialize<'de> for ChatMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawMessage {
            role: MessageRole,
            content: String,
        }

        let raw = RawMessage::deserialize(deserializer)?;

        // Content can be empty for assistant messages (partial responses)
        // but user/system messages should have content
        if raw.content.trim().is_empty() && raw.role != MessageRole::Assistant {
            return Err(serde::de::Error::custom(format!(
                "{:?} message content cannot be empty",
                raw.role
            )));
        }

        Ok(ChatMessage {
            role: raw.role,
            content: raw.content,
        })
    }
}

// =============================================================================
// Chat Completion Request
// =============================================================================

/// OpenAI-compatible chat completion request
///
/// Validation is enforced during deserialization - invalid instances cannot exist.
/// Use [`ChatCompletionRequest::builder()`] for programmatic construction in tests.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    model: ModelChoice,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

/// Builder for constructing [`ChatCompletionRequest`] programmatically
///
/// Provides a fluent API for building requests, particularly useful in tests.
/// Performs the same validation as JSON deserialization.
///
/// # Examples
///
/// ```
/// use octoroute::handlers::openai::types::{ChatCompletionRequest, ModelChoice};
///
/// let request = ChatCompletionRequest::builder()
///     .model(ModelChoice::Fast)
///     .system_message("You are helpful.")
///     .user_message("Hello!")
///     .temperature(0.7)
///     .build()
///     .expect("valid request");
/// ```
#[derive(Debug, Default)]
pub struct ChatCompletionRequestBuilder {
    model: ModelChoice,
    messages: Vec<ChatMessage>,
    stream: bool,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    top_p: Option<f64>,
    presence_penalty: Option<f64>,
    frequency_penalty: Option<f64>,
    user: Option<String>,
}

impl ChatCompletionRequestBuilder {
    /// Create a new builder with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the model choice for routing
    pub fn model(mut self, model: ModelChoice) -> Self {
        self.model = model;
        self
    }

    /// Add a single message to the request
    pub fn message(mut self, message: ChatMessage) -> Self {
        self.messages.push(message);
        self
    }

    /// Replace all messages with the provided vector
    pub fn messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self
    }

    /// Add a system message (convenience method)
    ///
    /// # Panics
    /// Panics if content is empty (use `message()` for error handling)
    pub fn system_message(self, content: impl Into<String>) -> Self {
        let msg = ChatMessage::try_new(MessageRole::System, content)
            .expect("system message content must not be empty");
        self.message(msg)
    }

    /// Add a user message (convenience method)
    ///
    /// # Panics
    /// Panics if content is empty (use `message()` for error handling)
    pub fn user_message(self, content: impl Into<String>) -> Self {
        let msg = ChatMessage::try_new(MessageRole::User, content)
            .expect("user message content must not be empty");
        self.message(msg)
    }

    /// Add an assistant message (convenience method)
    pub fn assistant_message(self, content: impl Into<String>) -> Self {
        let msg = ChatMessage::try_new(MessageRole::Assistant, content)
            .expect("assistant message creation should not fail");
        self.message(msg)
    }

    /// Enable or disable streaming
    pub fn stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Set the temperature (0.0 to 2.0)
    pub fn temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Set the maximum tokens
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set top_p (nucleus sampling, 0.0 exclusive to 1.0 inclusive)
    pub fn top_p(mut self, top_p: f64) -> Self {
        self.top_p = Some(top_p);
        self
    }

    /// Set presence penalty (-2.0 to 2.0)
    pub fn presence_penalty(mut self, presence_penalty: f64) -> Self {
        self.presence_penalty = Some(presence_penalty);
        self
    }

    /// Set frequency penalty (-2.0 to 2.0)
    pub fn frequency_penalty(mut self, frequency_penalty: f64) -> Self {
        self.frequency_penalty = Some(frequency_penalty);
        self
    }

    /// Set the user identifier
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Build the request, performing all validation
    ///
    /// # Errors
    /// Returns an error string if validation fails (same rules as JSON deserialization)
    pub fn build(self) -> Result<ChatCompletionRequest, String> {
        // Use shared validation logic
        validate_request_fields(
            &self.messages,
            self.temperature,
            self.top_p,
            self.presence_penalty,
            self.frequency_penalty,
            self.max_tokens,
        )?;

        Ok(ChatCompletionRequest {
            model: self.model,
            messages: self.messages,
            stream: self.stream,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            top_p: self.top_p,
            presence_penalty: self.presence_penalty,
            frequency_penalty: self.frequency_penalty,
            user: self.user,
        })
    }
}

impl ChatCompletionRequest {
    /// Create a new builder for constructing a request programmatically
    ///
    /// This is the recommended way to construct requests in tests.
    pub fn builder() -> ChatCompletionRequestBuilder {
        ChatCompletionRequestBuilder::new()
    }

    /// Get the model choice
    pub fn model(&self) -> &ModelChoice {
        &self.model
    }

    /// Get the messages
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Check if streaming is enabled
    pub fn stream(&self) -> bool {
        self.stream
    }

    /// Get temperature if set
    pub fn temperature(&self) -> Option<f64> {
        self.temperature
    }

    /// Get max_tokens if set
    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }

    /// Convert messages to a single prompt string for routing
    ///
    /// Combines all messages into a format suitable for routing analysis.
    pub fn to_prompt_string(&self) -> String {
        self.messages
            .iter()
            .map(|m| format!("{:?}: {}", m.role(), m.content()))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Get just the last user message content (for simpler routing)
    pub fn last_user_content(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role() == MessageRole::User)
            .map(|m| m.content())
    }

    /// Convert to RouteMetadata for routing decisions
    ///
    /// Uses auto-detection for task type based on message content.
    pub fn to_route_metadata(&self) -> RouteMetadata {
        let total_chars: usize = self.messages.iter().map(|m| m.content_length()).sum();
        let token_estimate = total_chars / 4; // Simple heuristic

        let task_type = self.infer_task_type();

        RouteMetadata::new(token_estimate)
            .with_importance(Importance::Normal)
            .with_task_type(task_type)
    }

    /// Infer task type from message content
    fn infer_task_type(&self) -> TaskType {
        let last_user_content = self.last_user_content().unwrap_or("").to_lowercase();

        // Code detection
        if last_user_content.contains("code")
            || last_user_content.contains("function")
            || last_user_content.contains("implement")
            || last_user_content.contains("```")
            || last_user_content.contains("programming")
            || last_user_content.contains("debug")
        {
            return TaskType::Code;
        }

        // Analysis detection
        if last_user_content.contains("analyze")
            || last_user_content.contains("analysis")
            || last_user_content.contains("compare")
            || last_user_content.contains("evaluate")
        {
            return TaskType::DeepAnalysis;
        }

        // Creative writing detection
        if last_user_content.contains("write a story")
            || last_user_content.contains("creative")
            || last_user_content.contains("poem")
            || last_user_content.contains("fiction")
        {
            return TaskType::CreativeWriting;
        }

        // Summary detection
        if last_user_content.contains("summarize")
            || last_user_content.contains("summary")
            || last_user_content.contains("tldr")
        {
            return TaskType::DocumentSummary;
        }

        // Casual chat detection
        if last_user_content.contains("hello")
            || last_user_content.contains("hi ")
            || last_user_content.contains("hey ")
            || last_user_content.starts_with("how are")
        {
            return TaskType::CasualChat;
        }

        // Default to question/answer
        TaskType::QuestionAnswer
    }
}

impl<'de> Deserialize<'de> for ChatCompletionRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawRequest {
            model: ModelChoice,
            messages: Vec<ChatMessage>,
            #[serde(default)]
            stream: bool,
            temperature: Option<f64>,
            max_tokens: Option<u32>,
            top_p: Option<f64>,
            presence_penalty: Option<f64>,
            frequency_penalty: Option<f64>,
            user: Option<String>,
        }

        let raw = RawRequest::deserialize(deserializer)?;

        // Use shared validation logic, converting String error to serde error
        validate_request_fields(
            &raw.messages,
            raw.temperature,
            raw.top_p,
            raw.presence_penalty,
            raw.frequency_penalty,
            raw.max_tokens,
        )
        .map_err(serde::de::Error::custom)?;

        Ok(ChatCompletionRequest {
            model: raw.model,
            messages: raw.messages,
            stream: raw.stream,
            temperature: raw.temperature,
            max_tokens: raw.max_tokens,
            top_p: raw.top_p,
            presence_penalty: raw.presence_penalty,
            frequency_penalty: raw.frequency_penalty,
            user: raw.user,
        })
    }
}

// =============================================================================
// Chat Completion Response (Non-Streaming)
// =============================================================================

/// Finish reason for a completion
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
}

/// Usage statistics for a chat completion response.
///
/// Fields are private to enforce the invariant that `total_tokens` always
/// equals `prompt_tokens + completion_tokens`. Use `new()` or `estimate()`
/// constructors, which guarantee this invariant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

impl Usage {
    /// Create usage stats from token counts.
    ///
    /// Automatically calculates `total_tokens` as `prompt_tokens + completion_tokens`.
    #[inline]
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }

    /// Create usage stats by estimating tokens from character count.
    ///
    /// Uses a ~4 chars/token heuristic which is typical for English text.
    /// May under/overestimate for non-English, code, or whitespace-heavy content.
    pub fn estimate(prompt_chars: usize, completion_chars: usize) -> Self {
        let prompt_tokens = (prompt_chars / 4) as u32;
        let completion_tokens = (completion_chars / 4) as u32;
        Self::new(prompt_tokens, completion_tokens)
    }

    /// Returns the number of tokens in the prompt.
    #[inline]
    pub fn prompt_tokens(&self) -> u32 {
        self.prompt_tokens
    }

    /// Returns the number of tokens in the completion.
    #[inline]
    pub fn completion_tokens(&self) -> u32 {
        self.completion_tokens
    }

    /// Returns the total number of tokens (prompt + completion).
    #[inline]
    pub fn total_tokens(&self) -> u32 {
        self.total_tokens
    }
}

/// Assistant message in response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub role: MessageRole,
    pub content: String,
}

impl AssistantMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }
}

/// A single choice in the response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: FinishReason,
}

/// OpenAI-compatible chat completion response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletion {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

impl ChatCompletion {
    /// Create a new chat completion response
    ///
    /// # Arguments
    /// * `content` - The assistant's response content
    /// * `model_name` - Name of the model that generated the response
    /// * `prompt_chars` - Number of characters in the prompt (for usage estimation)
    /// * `created` - Unix timestamp when the completion was created (use `current_timestamp()` helper)
    pub fn new(content: String, model_name: String, prompt_chars: usize, created: i64) -> Self {
        let completion_chars = content.chars().count();
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());

        Self {
            id,
            object: OBJECT_CHAT_COMPLETION.to_string(),
            created,
            model: model_name,
            choices: vec![Choice {
                index: 0,
                message: AssistantMessage::new(content),
                finish_reason: FinishReason::Stop,
            }],
            usage: Usage::estimate(prompt_chars, completion_chars),
        }
    }
}

/// Get the current Unix timestamp for response creation.
///
/// Returns the current time as seconds since UNIX epoch. If the system clock
/// is misconfigured (before UNIX epoch), returns 0 and logs a warning.
///
/// # Arguments
/// * `metrics` - Optional metrics to track clock errors for observability
/// * `request_id` - Optional request ID for log correlation
///
/// # Note
/// Clock errors are rare but indicate serious system misconfiguration.
/// The metric `octoroute_clock_errors_total` should trigger alerts.
pub fn current_timestamp(
    metrics: Option<&crate::metrics::Metrics>,
    request_id: Option<&crate::middleware::RequestId>,
) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_else(|e| {
            if let Some(rid) = request_id {
                tracing::warn!(
                    request_id = %rid,
                    error = %e,
                    "System clock appears to be before UNIX epoch - using 0 as timestamp"
                );
            } else {
                tracing::warn!(
                    error = %e,
                    "System clock appears to be before UNIX epoch - using 0 as timestamp"
                );
            }
            if let Some(m) = metrics {
                m.clock_error();
            }
            0
        })
}

// =============================================================================
// Chat Completion Chunk (Streaming)
// =============================================================================

/// Delta content in a streaming chunk
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// A single choice in a streaming chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
}

/// OpenAI-compatible streaming chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

impl ChatCompletionChunk {
    /// Create an initial chunk with role announcement
    pub fn initial(id: &str, model: &str, created: i64) -> Self {
        Self {
            id: id.to_string(),
            object: OBJECT_CHAT_COMPLETION_CHUNK.to_string(),
            created,
            model: model.to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: Some("assistant".to_string()),
                    content: None,
                },
                finish_reason: None,
            }],
        }
    }

    /// Create a content chunk
    pub fn content(id: &str, model: &str, created: i64, content: &str) -> Self {
        Self {
            id: id.to_string(),
            object: OBJECT_CHAT_COMPLETION_CHUNK.to_string(),
            created,
            model: model.to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: Some(content.to_string()),
                },
                finish_reason: None,
            }],
        }
    }

    /// Create a final chunk with finish reason
    pub fn finish(id: &str, model: &str, created: i64) -> Self {
        Self {
            id: id.to_string(),
            object: OBJECT_CHAT_COMPLETION_CHUNK.to_string(),
            created,
            model: model.to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta::default(),
                finish_reason: Some(FinishReason::Stop),
            }],
        }
    }
}

// =============================================================================
// Models List Response
// =============================================================================

/// A model object for the models list endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelObject {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
}

impl ModelObject {
    /// Create a new model object
    pub fn new(id: impl Into<String>, owned_by: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            object: OBJECT_MODEL.to_string(),
            created: 0, // OpenAI uses 0 for many models
            owned_by: owned_by.into(),
        }
    }
}

/// Response for GET /v1/models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsListResponse {
    pub object: String,
    pub data: Vec<ModelObject>,
}

impl ModelsListResponse {
    /// Create a models list response
    pub fn new(models: Vec<ModelObject>) -> Self {
        Self {
            object: OBJECT_LIST.to_string(),
            data: models,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // ModelChoice Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_model_choice_deserialize_auto() {
        let json = r#""auto""#;
        let model: ModelChoice = serde_json::from_str(json).unwrap();
        assert_eq!(model, ModelChoice::Auto);
    }

    #[test]
    fn test_model_choice_deserialize_tiers() {
        assert_eq!(
            serde_json::from_str::<ModelChoice>(r#""fast""#).unwrap(),
            ModelChoice::Fast
        );
        assert_eq!(
            serde_json::from_str::<ModelChoice>(r#""balanced""#).unwrap(),
            ModelChoice::Balanced
        );
        assert_eq!(
            serde_json::from_str::<ModelChoice>(r#""deep""#).unwrap(),
            ModelChoice::Deep
        );
    }

    #[test]
    fn test_model_choice_deserialize_case_insensitive() {
        assert_eq!(
            serde_json::from_str::<ModelChoice>(r#""AUTO""#).unwrap(),
            ModelChoice::Auto
        );
        assert_eq!(
            serde_json::from_str::<ModelChoice>(r#""Fast""#).unwrap(),
            ModelChoice::Fast
        );
        assert_eq!(
            serde_json::from_str::<ModelChoice>(r#""BALANCED""#).unwrap(),
            ModelChoice::Balanced
        );
    }

    #[test]
    fn test_model_choice_deserialize_specific() {
        let json = r#""gpt-4o-mini""#;
        let model: ModelChoice = serde_json::from_str(json).unwrap();
        assert_eq!(model, ModelChoice::Specific("gpt-4o-mini".to_string()));
    }

    #[test]
    fn test_model_choice_to_target_model() {
        assert_eq!(ModelChoice::Fast.to_target_model(), Some(TargetModel::Fast));
        assert_eq!(
            ModelChoice::Balanced.to_target_model(),
            Some(TargetModel::Balanced)
        );
        assert_eq!(ModelChoice::Deep.to_target_model(), Some(TargetModel::Deep));
        assert_eq!(ModelChoice::Auto.to_target_model(), None);
        assert_eq!(
            ModelChoice::Specific("test".to_string()).to_target_model(),
            None
        );
    }

    #[test]
    fn test_model_choice_requires_routing() {
        assert!(ModelChoice::Auto.requires_routing());
        assert!(!ModelChoice::Fast.requires_routing());
        assert!(!ModelChoice::Specific("test".to_string()).requires_routing());
    }

    #[test]
    fn test_model_choice_is_specific() {
        assert!(ModelChoice::Specific("test".to_string()).is_specific());
        assert!(!ModelChoice::Auto.is_specific());
        assert!(!ModelChoice::Fast.is_specific());
    }

    #[test]
    fn test_model_choice_rejects_empty_string() {
        let json = r#""""#;
        let result = serde_json::from_str::<ModelChoice>(json);
        assert!(result.is_err(), "Empty model name should be rejected");
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_model_choice_rejects_whitespace_only() {
        let json = r#""   ""#;
        let result = serde_json::from_str::<ModelChoice>(json);
        assert!(
            result.is_err(),
            "Whitespace-only model name should be rejected"
        );
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_model_choice_try_specific_valid() {
        let result = ModelChoice::try_specific("qwen3-8b");
        assert!(result.is_ok());
        let model = result.unwrap();
        assert!(model.is_specific());
        assert_eq!(model.specific_name(), Some("qwen3-8b"));
    }

    #[test]
    fn test_model_choice_try_specific_rejects_empty() {
        let result = ModelChoice::try_specific("");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot be empty"));
    }

    #[test]
    fn test_model_choice_try_specific_rejects_whitespace() {
        let result = ModelChoice::try_specific("   ");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("whitespace"));
    }

    #[test]
    fn test_model_choice_try_specific_accepts_with_spaces() {
        // Model names with internal spaces are valid
        let result = ModelChoice::try_specific("my model name");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().specific_name(), Some("my model name"));
    }

    // -------------------------------------------------------------------------
    // ChatMessage Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_chat_message_deserialize_user() {
        let json = r#"{"role": "user", "content": "Hello!"}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role(), MessageRole::User);
        assert_eq!(msg.content(), "Hello!");
    }

    #[test]
    fn test_chat_message_deserialize_system() {
        let json = r#"{"role": "system", "content": "You are a helpful assistant."}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role(), MessageRole::System);
    }

    #[test]
    fn test_chat_message_rejects_empty_user_content() {
        let json = r#"{"role": "user", "content": ""}"#;
        let result = serde_json::from_str::<ChatMessage>(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_chat_message_allows_empty_assistant_content() {
        // Assistant messages can be empty (partial responses)
        let json = r#"{"role": "assistant", "content": ""}"#;
        let result = serde_json::from_str::<ChatMessage>(json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_chat_message_content_length_unicode() {
        let msg = ChatMessage::try_new(MessageRole::User, "Hello 👋 世界").expect("valid content");
        // "Hello 👋 世界" = 10 characters (emoji and CJK count as 1 each)
        assert_eq!(msg.content_length(), 10);
    }

    #[test]
    fn test_chat_message_new_validates_user_content() {
        // ChatMessage::new should reject empty content for User role
        let result = ChatMessage::try_new(MessageRole::User, "");
        assert!(
            result.is_err(),
            "ChatMessage::try_new should reject empty User content"
        );
    }

    #[test]
    fn test_chat_message_new_validates_system_content() {
        // ChatMessage::new should reject empty content for System role
        let result = ChatMessage::try_new(MessageRole::System, "");
        assert!(
            result.is_err(),
            "ChatMessage::try_new should reject empty System content"
        );
    }

    #[test]
    fn test_chat_message_new_allows_empty_assistant_content() {
        // Assistant messages can have empty content (function calls, etc.)
        let result = ChatMessage::try_new(MessageRole::Assistant, "");
        assert!(
            result.is_ok(),
            "ChatMessage::try_new should allow empty Assistant content"
        );
    }

    #[test]
    fn test_chat_message_new_validates_whitespace_only() {
        // Whitespace-only content should be rejected for User/System
        let result = ChatMessage::try_new(MessageRole::User, "   ");
        assert!(
            result.is_err(),
            "ChatMessage::try_new should reject whitespace-only User content"
        );
    }

    // -------------------------------------------------------------------------
    // ChatCompletionRequest Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_request_deserialize_minimal() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Hello!"}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model(), &ModelChoice::Auto);
        assert_eq!(req.messages().len(), 1);
        assert!(!req.stream());
    }

    #[test]
    fn test_request_deserialize_with_options() {
        let json = r#"{
            "model": "fast",
            "messages": [{"role": "user", "content": "Hello!"}],
            "stream": true,
            "temperature": 0.7,
            "max_tokens": 1000
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model(), &ModelChoice::Fast);
        assert!(req.stream());
        assert_eq!(req.temperature(), Some(0.7));
        assert_eq!(req.max_tokens(), Some(1000));
    }

    #[test]
    fn test_request_rejects_empty_messages() {
        let json = r#"{
            "model": "auto",
            "messages": []
        }"#;
        let result = serde_json::from_str::<ChatCompletionRequest>(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_request_rejects_invalid_temperature() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "temperature": 3.0
        }"#;
        let result = serde_json::from_str::<ChatCompletionRequest>(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("temperature"));
    }

    #[test]
    fn test_request_rejects_invalid_top_p() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "top_p": 0.0
        }"#;
        let result = serde_json::from_str::<ChatCompletionRequest>(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("top_p"));
    }

    #[test]
    fn test_request_rejects_zero_max_tokens() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 0
        }"#;
        let result = serde_json::from_str::<ChatCompletionRequest>(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_tokens"));
    }

    #[test]
    fn test_request_rejects_negative_temperature() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "temperature": -0.5
        }"#;
        let result = serde_json::from_str::<ChatCompletionRequest>(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("temperature"));
    }

    #[test]
    fn test_request_rejects_out_of_range_presence_penalty() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "presence_penalty": 2.5
        }"#;
        let result = serde_json::from_str::<ChatCompletionRequest>(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("presence_penalty"));
    }

    #[test]
    fn test_request_rejects_out_of_range_frequency_penalty() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "frequency_penalty": -3.0
        }"#;
        let result = serde_json::from_str::<ChatCompletionRequest>(json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("frequency_penalty")
        );
    }

    #[test]
    fn test_request_to_prompt_string() {
        let json = r#"{
            "model": "auto",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello!"}
            ]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        let prompt = req.to_prompt_string();
        assert!(prompt.contains("System: You are helpful."));
        assert!(prompt.contains("User: Hello!"));
    }

    #[test]
    fn test_request_last_user_content() {
        let json = r#"{
            "model": "auto",
            "messages": [
                {"role": "user", "content": "First"},
                {"role": "assistant", "content": "Response"},
                {"role": "user", "content": "Second"}
            ]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.last_user_content(), Some("Second"));
    }

    #[test]
    fn test_request_infer_task_type_code() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Write a function to sort an array"}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        let metadata = req.to_route_metadata();
        assert_eq!(metadata.task_type, TaskType::Code);
    }

    #[test]
    fn test_request_infer_task_type_analysis() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "Analyze this data and compare trends"}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        let metadata = req.to_route_metadata();
        assert_eq!(metadata.task_type, TaskType::DeepAnalysis);
    }

    #[test]
    fn test_request_infer_task_type_default() {
        let json = r#"{
            "model": "auto",
            "messages": [{"role": "user", "content": "What is the capital of France?"}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        let metadata = req.to_route_metadata();
        assert_eq!(metadata.task_type, TaskType::QuestionAnswer);
    }

    // -------------------------------------------------------------------------
    // ChatCompletion Response Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_chat_completion_new() {
        let created = 1700000000; // Fixed timestamp for test reproducibility
        let response = ChatCompletion::new(
            "Hello! How can I help?".to_string(),
            "fast-1".to_string(),
            10,
            created,
        );

        assert!(response.id.starts_with("chatcmpl-"));
        assert_eq!(response.object, OBJECT_CHAT_COMPLETION);
        assert_eq!(response.model, "fast-1");
        assert_eq!(response.created, created);
        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content,
            "Hello! How can I help?"
        );
        assert_eq!(response.choices[0].finish_reason, FinishReason::Stop);
    }

    #[test]
    fn test_chat_completion_serializes_correctly() {
        let created = 1700000000;
        let response = ChatCompletion::new("Test".to_string(), "model".to_string(), 4, created);
        let json = serde_json::to_string(&response).unwrap();

        assert!(json.contains("\"object\":\"chat.completion\""));
        assert!(json.contains("\"finish_reason\":\"stop\""));
        assert!(json.contains("\"role\":\"assistant\""));
    }

    // -------------------------------------------------------------------------
    // Usage Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_usage_new_calculates_total() {
        let usage = Usage::new(100, 50);
        assert_eq!(usage.prompt_tokens(), 100);
        assert_eq!(usage.completion_tokens(), 50);
        assert_eq!(usage.total_tokens(), 150);
    }

    #[test]
    fn test_usage_total_always_equals_sum() {
        // Property: total_tokens always equals prompt_tokens + completion_tokens
        let test_cases = [
            (0, 0),
            (1, 0),
            (0, 1),
            (100, 200),
            (u32::MAX / 2, u32::MAX / 2), // Near overflow but not exceeding
        ];

        for (prompt, completion) in test_cases {
            let usage = Usage::new(prompt, completion);
            assert_eq!(
                usage.total_tokens(),
                prompt + completion,
                "total_tokens should equal prompt + completion for ({}, {})",
                prompt,
                completion
            );
        }
    }

    #[test]
    fn test_usage_estimate_calculates_from_chars() {
        // ~4 chars per token heuristic
        let usage = Usage::estimate(400, 200);
        assert_eq!(usage.prompt_tokens(), 100); // 400 / 4
        assert_eq!(usage.completion_tokens(), 50); // 200 / 4
        assert_eq!(usage.total_tokens(), 150);
    }

    #[test]
    fn test_usage_serializes_correctly() {
        let usage = Usage::new(10, 20);
        let json = serde_json::to_string(&usage).unwrap();

        assert!(json.contains("\"prompt_tokens\":10"));
        assert!(json.contains("\"completion_tokens\":20"));
        assert!(json.contains("\"total_tokens\":30"));
    }

    #[test]
    fn test_usage_deserializes_correctly() {
        let json = r#"{"prompt_tokens":15,"completion_tokens":25,"total_tokens":40}"#;
        let usage: Usage = serde_json::from_str(json).unwrap();

        assert_eq!(usage.prompt_tokens(), 15);
        assert_eq!(usage.completion_tokens(), 25);
        assert_eq!(usage.total_tokens(), 40);
    }

    // -------------------------------------------------------------------------
    // ChatCompletionChunk Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_chunk_initial() {
        let chunk = ChatCompletionChunk::initial("test-id", "model", 12345);
        assert_eq!(chunk.object, OBJECT_CHAT_COMPLETION_CHUNK);
        assert_eq!(chunk.choices[0].delta.role, Some("assistant".to_string()));
        assert!(chunk.choices[0].delta.content.is_none());
        assert!(chunk.choices[0].finish_reason.is_none());
    }

    #[test]
    fn test_chunk_content() {
        let chunk = ChatCompletionChunk::content("test-id", "model", 12345, "Hello");
        assert!(chunk.choices[0].delta.role.is_none());
        assert_eq!(chunk.choices[0].delta.content, Some("Hello".to_string()));
        assert!(chunk.choices[0].finish_reason.is_none());
    }

    #[test]
    fn test_chunk_finish() {
        let chunk = ChatCompletionChunk::finish("test-id", "model", 12345);
        assert!(chunk.choices[0].delta.role.is_none());
        assert!(chunk.choices[0].delta.content.is_none());
        assert_eq!(chunk.choices[0].finish_reason, Some(FinishReason::Stop));
    }

    // -------------------------------------------------------------------------
    // ModelsListResponse Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_models_list_response() {
        let models = vec![
            ModelObject::new("auto", "octoroute"),
            ModelObject::new("fast", "octoroute"),
        ];
        let response = ModelsListResponse::new(models);

        assert_eq!(response.object, OBJECT_LIST);
        assert_eq!(response.data.len(), 2);
        assert_eq!(response.data[0].id, "auto");
        assert_eq!(response.data[0].object, OBJECT_MODEL);
    }

    #[test]
    fn test_model_object_serializes() {
        let model = ModelObject::new("test-model", "owner");
        let json = serde_json::to_string(&model).unwrap();
        assert!(json.contains("\"id\":\"test-model\""));
        assert!(json.contains("\"object\":\"model\""));
        assert!(json.contains("\"owned_by\":\"owner\""));
    }

    // -------------------------------------------------------------------------
    // ChatCompletionRequest Builder Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_builder_minimal() {
        let request = ChatCompletionRequest::builder()
            .user_message("Hello!")
            .build()
            .expect("valid request");

        assert_eq!(request.model(), &ModelChoice::Auto);
        assert_eq!(request.messages().len(), 1);
        assert_eq!(request.messages()[0].content(), "Hello!");
        assert!(!request.stream());
    }

    #[test]
    fn test_builder_with_model() {
        let request = ChatCompletionRequest::builder()
            .model(ModelChoice::Fast)
            .user_message("Hello!")
            .build()
            .expect("valid request");

        assert_eq!(request.model(), &ModelChoice::Fast);
    }

    #[test]
    fn test_builder_with_system_message() {
        let request = ChatCompletionRequest::builder()
            .system_message("You are helpful.")
            .user_message("Hello!")
            .build()
            .expect("valid request");

        assert_eq!(request.messages().len(), 2);
        assert_eq!(request.messages()[0].role(), MessageRole::System);
        assert_eq!(request.messages()[1].role(), MessageRole::User);
    }

    #[test]
    fn test_builder_with_streaming() {
        let request = ChatCompletionRequest::builder()
            .user_message("Hello!")
            .stream(true)
            .build()
            .expect("valid request");

        assert!(request.stream());
    }

    #[test]
    fn test_builder_with_temperature() {
        let request = ChatCompletionRequest::builder()
            .user_message("Hello!")
            .temperature(0.7)
            .build()
            .expect("valid request");

        assert_eq!(request.temperature(), Some(0.7));
    }

    #[test]
    fn test_builder_with_max_tokens() {
        let request = ChatCompletionRequest::builder()
            .user_message("Hello!")
            .max_tokens(1000)
            .build()
            .expect("valid request");

        assert_eq!(request.max_tokens(), Some(1000));
    }

    #[test]
    fn test_builder_rejects_empty_messages() {
        let result = ChatCompletionRequest::builder().build();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_builder_rejects_invalid_temperature() {
        let result = ChatCompletionRequest::builder()
            .user_message("Hello!")
            .temperature(3.0)
            .build();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("temperature"));
    }

    #[test]
    fn test_builder_with_all_options() {
        let request = ChatCompletionRequest::builder()
            .model(ModelChoice::Deep)
            .system_message("You are a coding assistant.")
            .user_message("Write a function")
            .stream(true)
            .temperature(0.8)
            .max_tokens(2000)
            .top_p(0.9)
            .presence_penalty(0.5)
            .frequency_penalty(-0.5)
            .user("test-user")
            .build()
            .expect("valid request");

        assert_eq!(request.model(), &ModelChoice::Deep);
        assert!(request.stream());
        assert_eq!(request.temperature(), Some(0.8));
        assert_eq!(request.max_tokens(), Some(2000));
    }

    #[test]
    fn test_builder_with_custom_message() {
        let msg = ChatMessage::try_new(MessageRole::User, "Custom message").unwrap();
        let request = ChatCompletionRequest::builder()
            .message(msg)
            .build()
            .expect("valid request");

        assert_eq!(request.messages().len(), 1);
        assert_eq!(request.messages()[0].content(), "Custom message");
    }

    #[test]
    fn test_builder_with_messages_vec() {
        let messages = vec![
            ChatMessage::try_new(MessageRole::System, "System prompt").unwrap(),
            ChatMessage::try_new(MessageRole::User, "User question").unwrap(),
        ];
        let request = ChatCompletionRequest::builder()
            .messages(messages)
            .build()
            .expect("valid request");

        assert_eq!(request.messages().len(), 2);
    }
}
