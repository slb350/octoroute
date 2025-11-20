# Octoroute Technical Design Document

**Version**: 1.0
**Last Updated**: 2025-11-17
**Status**: Draft

---

## Table of Contents

1. [System Overview](#system-overview)
2. [Architecture](#architecture)
3. [Routing Strategies](#routing-strategies)
4. [API Design](#api-design)
5. [Data Flow](#data-flow)
6. [Error Handling](#error-handling)
7. [Observability](#observability)
8. [Performance Considerations](#performance-considerations)
9. [Testing Strategy](#testing-strategy)
10. [Implementation Plan](#implementation-plan)

---

## 1. System Overview

### Purpose

Octoroute is an intelligent HTTP API router that sits between client applications and multiple local LLM endpoints. It automatically routes requests to the optimal model based on task characteristics, reducing compute costs while maintaining quality.

### Key Principles

1. **Local-first**: All models run on local/homelab infrastructure
2. **Zero-latency rule routing**: Simple decisions use fast pattern matching
3. **Intelligent fallback**: LLM-based routing for ambiguous cases
4. **Observable**: Every routing decision is logged and traceable
5. **Extensible**: Support for custom routing strategies via configuration

### System Diagram

```
┌──────────────────────────────────────────────────────────────┐
│                    Client Applications                        │
│          (CLI, Web UI, API consumers, etc.)                  │
└────────────────────┬─────────────────────────────────────────┘
                     │ HTTP POST /chat
                     │ { prompt, importance, task_type, ... }
                     ▼
┌──────────────────────────────────────────────────────────────┐
│                      Octoroute API                           │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Axum HTTP Server (Tokio runtime)                     │  │
│  │  - Request validation                                  │  │
│  │  - Metadata extraction                                 │  │
│  │  - Router selection                                    │  │
│  └────────────┬───────────────────────────────────────────┘  │
│               │                                              │
│  ┌────────────▼───────────────────────────────────────────┐  │
│  │  Router Layer                                          │  │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────────┐     │  │
│  │  │ Rule     │  │ LLM      │  │ Tool-based       │     │  │
│  │  │ Router   │  │ Router   │  │ Router           │     │  │
│  │  └──────────┘  └──────────┘  └──────────────────┘     │  │
│  │       │              │              │                  │  │
│  │       └──────────────┴──────────────┘                  │  │
│  │                      │                                 │  │
│  │                      ▼                                 │  │
│  │            Model Selection Decision                   │  │
│  │            (fast_8b | balanced_30b | deep_120b)       │  │
│  └────────────┬───────────────────────────────────────────┘  │
│               │                                              │
│  ┌────────────▼───────────────────────────────────────────┐  │
│  │  Model Client Manager (open-agent-sdk)                │  │
│  │  - Client pooling                                      │  │
│  │  - Request proxying                                    │  │
│  │  - Response streaming                                  │  │
│  └────────────┬───────────────────────────────────────────┘  │
└───────────────┼──────────────────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────────┐
│              Local Model Servers                            │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ 8B Model     │  │ 30B Model    │  │ 120B Model   │      │
│  │ (Ollama)     │  │ (LM Studio)  │  │ (llama.cpp)  │      │
│  │ :11434       │  │ :1234        │  │ :8080        │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. Architecture

### Technology Stack

| Layer | Technology | Rationale |
|-------|------------|-----------|
| HTTP Server | Axum 0.8 | Ergonomic, Tower ecosystem, same runtime as SDK |
| Async Runtime | Tokio 1.x | Industry standard, used by `open-agent-sdk` |
| LLM SDK | `open-agent-sdk` 0.6 | Streaming, tools, hooks, context management |
| Configuration | TOML + serde | Human-readable, type-safe parsing |
| Error Handling | thiserror | Rich error context, `IntoResponse` integration |
| Logging | tracing + tracing-subscriber | Structured logging with spans |
| Metrics | prometheus 0.14 | Direct Prometheus integration, homelab-friendly, stable (switched from deprecated opentelemetry-prometheus) |
| Testing | proptest + criterion | Property tests + benchmarks |

### Module Hierarchy

```
octoroute/
├── main.rs                    # Axum server entrypoint
├── lib.rs                     # Public library API
│
├── config/                    # Configuration management
│   ├── mod.rs
│   ├── models.rs             # ModelConfig, ModelEndpoint
│   └── routing.rs            # RoutingConfig, Strategy enum
│
├── router/                    # Routing strategies
│   ├── mod.rs                # Router trait + factory
│   ├── rule_based.rs         # Fast pattern-based routing
│   ├── llm_based.rs          # LLM-powered routing (30B)
│   ├── tool_based.rs         # Tool-based routing (experimental)
│   └── metadata.rs           # RouteMetadata struct
│
├── models/                    # Model client management
│   ├── mod.rs
│   ├── client.rs             # Wrapper around open-agent-sdk Client
│   └── pool.rs               # Client pooling (optional)
│
├── handlers/                  # HTTP request handlers
│   ├── mod.rs
│   ├── chat.rs               # POST /chat
│   └── health.rs             # GET /health
│
├── middleware/                # Axum middleware
│   ├── mod.rs
│   ├── logging.rs            # Request/response tracing
│   └── metrics.rs            # Prometheus metrics (optional)
│
├── error.rs                   # AppError, AppResult types
└── telemetry.rs              # Tracing setup
```

---

## 3. Routing Strategies

### 3.1 Rule-Based Router

**Purpose**: Fast, deterministic routing with zero LLM overhead.

**Algorithm**:

```rust
pub struct RuleBasedRouter {
    config: RoutingConfig,
}

impl RuleBasedRouter {
    pub fn route(&self, meta: &RouteMetadata) -> Option<TargetModel> {
        use Importance::*;
        use TaskType::*;

        // Rule 1: Trivial/casual tasks → 8B
        if matches!(meta.task_type, CasualChat)
            && meta.token_estimate < 256
            && !matches!(meta.importance, High)
        {
            return Some(TargetModel::Fast8B);
        }

        // Rule 2: Medium-depth tasks → 30B
        if meta.token_estimate < 2048
            && !matches!(meta.task_type, DeepAnalysis | CreativeWriting)
        {
            return Some(TargetModel::Balanced30B);
        }

        // Rule 3: High importance or deep work → 120B
        if matches!(meta.importance, High)
            || matches!(meta.task_type, DeepAnalysis | CreativeWriting)
        {
            return Some(TargetModel::Deep120B);
        }

        // Rule 4: Code generation (special case)
        if matches!(meta.task_type, Code) {
            return if meta.token_estimate > 1024 {
                Some(TargetModel::Deep120B)
            } else {
                Some(TargetModel::Balanced30B)
            };
        }

        // No rule matched → delegate to LLM router
        None
    }
}
```

**Performance**: <1ms per routing decision (pure CPU, no I/O).

**Trade-offs**:
- ✅ Fast, predictable
- ✅ No model invocation overhead
- ❌ Limited by rule expressiveness
- ❌ Requires manual tuning

---

### 3.2 LLM-Based Router

**Purpose**: Intelligent routing for ambiguous or complex cases.

**Algorithm**:

```rust
pub struct LlmBasedRouter {
    router_client: Arc<Mutex<Client>>, // 30B model client
}

impl LlmBasedRouter {
    pub async fn route(&self,
        user_prompt: &str,
        meta: &RouteMetadata
    ) -> Result<TargetModel, AppError> {
        let router_prompt = format!(
            "You are a router that chooses which LLM to use.\n\n\
             Available models:\n\
             - FAST_8B: Quick (8B params), for simple chat, short Q&A, casual tasks.\n\
             - BALANCED_30B: Good reasoning (30B params), coding, document summaries, explanations.\n\
             - DEEP_120B: Deep reasoning (120B params), creative writing, complex analysis, research.\n\n\
             User request:\n{}\n\n\
             Metadata:\n\
             - Estimated tokens: {}\n\
             - Importance: {:?}\n\
             - Task type: {:?}\n\n\
             Respond with ONLY one of: FAST_8B, BALANCED_30B, DEEP_120B",
            user_prompt,
            meta.token_estimate,
            meta.importance,
            meta.task_type
        );

        let mut client = self.router_client.lock().await;
        client.send(&router_prompt).await?;

        let mut response_text = String::new();
        while let Some(block) = client.receive().await {
            match block? {
                ContentBlock::Text(t) => response_text.push_str(&t.text),
                _ => {}
            }
        }

        // Parse router decision
        let normalized = response_text.trim().to_uppercase();
        let target = if normalized.contains("FAST_8B") {
            TargetModel::Fast8B
        } else if normalized.contains("BALANCED_30B") {
            TargetModel::Balanced30B
        } else {
            TargetModel::Deep120B // Default to largest
        };

        Ok(target)
    }
}
```

**Performance**: 100-500ms (depends on 30B model speed).

**Trade-offs**:
- ✅ Intelligent, adaptive
- ✅ Can handle nuanced cases
- ❌ Adds latency (router invocation)
- ❌ Requires reliable 30B model
- ❌ Token cost for router prompt

---

### 3.3 Tool-Based Router (Experimental)

**Purpose**: Let the router model directly invoke target models via tools.

**Algorithm**:

```rust
pub struct ToolBasedRouter {
    router_client: Arc<Mutex<Client>>, // 30B with tools
}

impl ToolBasedRouter {
    pub async fn new(config: &ModelConfig) -> Result<Self, AppError> {
        // Define tools for each model
        let fast_tool = tool("call_fast_8b", "Use the 8B model for simple, quick tasks")
            .param("prompt", "string", "The user's prompt")
            .build(|args| async move {
                let prompt = args["prompt"].as_str().unwrap();
                let mut client = create_client_for(TargetModel::Fast8B)?;
                client.send(prompt).await?;

                let mut response = String::new();
                while let Some(block) = client.receive().await {
                    if let ContentBlock::Text(t) = block? {
                        response.push_str(&t.text);
                    }
                }

                Ok(json!({ "response": response }))
            });

        // Similar tools for balanced_30b and deep_120b...

        let options = AgentOptions::builder()
            .system_prompt(
                "You are a routing assistant. Decide which model to use \
                 by calling the appropriate tool. Never respond directly."
            )
            .model(&config.router_model)
            .base_url(&config.router_base_url)
            .tool(fast_tool)
            .tool(balanced_tool)
            .tool(deep_tool)
            .auto_execute_tools(true)
            .max_tool_iterations(3)
            .build()?;

        let router_client = Client::new(options)?;

        Ok(Self {
            router_client: Arc::new(Mutex::new(router_client)),
        })
    }

    pub async fn route(&self, user_prompt: &str) -> Result<String, AppError> {
        let mut client = self.router_client.lock().await;
        client.send(user_prompt).await?;

        // Router will choose and execute the appropriate tool
        // We just collect the final response
        let mut final_response = String::new();
        while let Some(block) = client.receive().await {
            match block? {
                ContentBlock::Text(t) => final_response.push_str(&t.text),
                ContentBlock::ToolResult(result) => {
                    // Extract response from tool result
                    if let Some(response) = result.content()["response"].as_str() {
                        final_response = response.to_string();
                    }
                }
                _ => {}
            }
        }

        Ok(final_response)
    }
}
```

**Trade-offs**:
- ✅ Most LangChain-like pattern
- ✅ Router decides AND executes
- ✅ Hooks can track tool usage
- ❌ Most complex implementation
- ❌ Highest latency (router + target model)
- ❌ Router must support reliable tool calling

---

### 3.4 Hybrid Router (Recommended)

**Purpose**: Combine speed of rules with intelligence of LLM.

```rust
pub struct HybridRouter {
    rule_router: RuleBasedRouter,
    llm_router: LlmBasedRouter,
}

impl HybridRouter {
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata
    ) -> Result<TargetModel, AppError> {
        // Try rule-based first (fast path)
        if let Some(target) = self.rule_router.route(meta) {
            tracing::info!(
                target = ?target,
                strategy = "rule",
                "Route decision made"
            );
            return Ok(target);
        }

        // Fall back to LLM router for ambiguous cases
        tracing::debug!("No rule matched, delegating to LLM router");
        let target = self.llm_router.route(user_prompt, meta).await?;
        tracing::info!(
            target = ?target,
            strategy = "llm",
            "Route decision made"
        );

        Ok(target)
    }
}
```

---

## 4. API Design

### 4.1 Request Schema

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    /// User's prompt/message
    pub prompt: String,

    /// Optional importance level (low, normal, high)
    #[serde(default)]
    pub importance: Importance,

    /// Optional task type hint
    #[serde(default)]
    pub task_type: TaskType,

    /// Optional explicit model override
    #[serde(default)]
    pub model: Option<TargetModel>,

    /// Optional temperature override
    #[serde(default)]
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Importance {
    Low,
    Normal,
    High,
}

impl Default for Importance {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    CasualChat,
    Code,
    CreativeWriting,
    DeepAnalysis,
    DocumentSummary,
    QuestionAnswer,
}

impl Default for TaskType {
    fn default() -> Self {
        Self::QuestionAnswer
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetModel {
    Fast8B,
    Balanced30B,
    Deep120B,
}
```

### 4.2 Response Schema

```rust
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    /// Generated response text
    pub response: String,

    /// Which model was used
    pub model_used: TargetModel,

    /// Routing strategy that made the decision
    pub routing_strategy: String, // "rule", "llm", "tool", "override"

    /// Token count estimate (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,

    /// Processing time in milliseconds
    pub processing_time_ms: u64,
}
```

### 4.3 HTTP Endpoints

#### `POST /chat`

**Purpose**: Submit a chat request and get a routed response.

**Handler**:

```rust
use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use std::sync::Arc;
use std::time::Instant;

pub async fn chat_handler(
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    let start = Instant::now();

    // 1. Extract metadata
    let metadata = RouteMetadata::from_request(&request)?;

    // 2. Determine target model (override or route)
    let target = if let Some(model) = request.model {
        tracing::info!(model = ?model, "Using explicit model override");
        model
    } else {
        app_state.router.route(&request.prompt, &metadata).await?
    };

    // 3. Get client for target model
    let mut client = app_state.create_client_for(target).await?;

    // 4. Send request to model
    client.send(&request.prompt).await
        .map_err(|e| AppError::ModelInvocation {
            model: target,
            source: e,
        })?;

    // 5. Stream response
    let mut response_text = String::new();
    while let Some(block) = client.receive().await {
        match block.map_err(|e| AppError::ModelInvocation {
            model: target,
            source: e,
        })? {
            ContentBlock::Text(text) => {
                response_text.push_str(&text.text);
            }
            _ => {}
        }
    }

    // 6. Build response
    let elapsed = start.elapsed();
    Ok(Json(ChatResponse {
        response: response_text,
        model_used: target,
        routing_strategy: "hybrid".to_string(), // TODO: track actual strategy
        token_count: None, // TODO: estimate tokens
        processing_time_ms: elapsed.as_millis() as u64,
    }))
}
```

#### `GET /health`

**Purpose**: Health check endpoint.

```rust
pub async fn health_handler() -> &'static str {
    "OK"
}
```

#### `GET /models`

**Purpose**: List available models and their status.

```rust
#[derive(Serialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelStatus>,
}

#[derive(Serialize)]
pub struct ModelStatus {
    pub name: String,
    pub endpoint: String,
    pub healthy: bool,
}

pub async fn models_handler(
    State(app_state): State<Arc<AppState>>,
) -> Json<ModelsResponse> {
    // TODO: Implement health checks for each model
    Json(ModelsResponse {
        models: vec![
            ModelStatus {
                name: "fast_8b".to_string(),
                endpoint: "http://macmini-1:11434".to_string(),
                healthy: true,
            },
            // ... more models
        ],
    })
}
```

---

## 5. Data Flow

### 5.1 Request Flow (Hybrid Router)

```
1. HTTP Request arrives at Axum
   ↓
2. Middleware: Request logging, metrics
   ↓
3. Handler: Extract ChatRequest JSON
   ↓
4. Build RouteMetadata (token estimate, task type, importance)
   ↓
5. Check for explicit model override
   ├─ Yes → Skip routing, use specified model
   └─ No  → Continue to router
           ↓
6. RuleBasedRouter.route(metadata)
   ├─ Rule matched → Return target model
   └─ No match    → LlmBasedRouter.route(prompt, metadata)
                     ↓
7. Create open-agent-sdk Client for target model
   ↓
8. Client.send(prompt)
   ↓
9. Stream response blocks from model
   ↓
10. Accumulate text blocks into response
   ↓
11. Build ChatResponse (model_used, timing, etc.)
   ↓
12. Return JSON response
```

### 5.2 Error Flow

```
Error occurs at any step
   ↓
Map to AppError variant
   ↓
AppError::into_response()
   ↓
Return HTTP error response
   - 400: Invalid request (validation)
   - 500: Internal error (config, logic)
   - 503: Model unavailable
   - 504: Model timeout
```

---

## 6. Error Handling

### 6.1 Error Type Hierarchy

Following Axum's error model (all errors must convert to HTTP responses):

```rust
use thiserror::Error;
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Invalid request: {0}")]
    Validation(String),

    #[error("Model invocation failed for {model:?}: {source}")]
    ModelInvocation {
        model: TargetModel,
        #[source]
        source: open_agent::Error,
    },

    #[error("Model unavailable: {model:?}")]
    ModelUnavailable {
        model: TargetModel,
    },

    #[error("Routing failed: {0}")]
    RoutingFailed(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::ModelInvocation { model, source } => {
                (StatusCode::BAD_GATEWAY, format!("Model {:?} error: {}", model, source))
            }
            AppError::ModelUnavailable { model } => {
                (StatusCode::SERVICE_UNAVAILABLE, format!("Model {:?} unavailable", model))
            }
            AppError::RoutingFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        let body = Json(serde_json::json!({
            "error": message,
        }));

        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
```

### 6.2 Error Handling Best Practices

1. **Use `?` operator**: Propagate errors up to handler
2. **Map SDK errors**: Convert `open_agent::Error` to `AppError::ModelInvocation`
3. **Validate early**: Check request validity before routing
4. **Log errors**: Use `tracing::error!` for all error paths
5. **Never panic**: All errors should be handled gracefully

---

## 7. Observability

### 7.1 Structured Logging

Using `tracing` for structured logs:

```rust
use tracing::{info, debug, error, instrument};

#[instrument(skip(app_state, request), fields(prompt_len = request.prompt.len()))]
pub async fn chat_handler(
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    info!(importance = ?request.importance, task_type = ?request.task_type, "Chat request received");

    let metadata = RouteMetadata::from_request(&request)?;
    debug!(token_estimate = metadata.token_estimate, "Metadata extracted");

    let target = app_state.router.route(&request.prompt, &metadata).await?;
    info!(model = ?target, "Routing decision made");

    // ... rest of handler
}
```

### 7.2 Metrics (Optional)

Using direct Prometheus integration for simple, homelab-friendly observability:

**Why Direct Prometheus?**
- **Simplicity**: No intermediate abstraction layers - direct Prometheus integration
- **Homelab-friendly**: Works with existing Prometheus/Grafana stacks out of the box
- **Stability**: Mature, actively maintained `prometheus` crate
- **Zero overhead**: No OTEL collector or additional infrastructure required

**Dependencies (behind `metrics` feature flag):**

```toml
[dependencies]
# Metrics (optional, behind feature flag)
prometheus = { version = "0.14", optional = true }
lazy_static = { version = "1.5", optional = true }

[features]
default = []
metrics = ["prometheus", "lazy_static"]
```

**Implementation:**

```rust
use prometheus::{CounterVec, Encoder, HistogramOpts, HistogramVec, Opts, Registry, TextEncoder};
use std::sync::Arc;

pub struct Metrics {
    registry: Arc<Registry>,
    requests_total: CounterVec,
    routing_duration: HistogramVec,
    model_invocations: CounterVec,
}

impl Metrics {
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        // Counter: Total requests by tier and routing strategy
        let requests_total = CounterVec::new(
            Opts::new("octoroute_requests_total", "Total number of chat requests"),
            &["tier", "strategy"],
        )?;
        registry.register(Box::new(requests_total.clone()))?;

        // Histogram: Routing decision latency
        let routing_duration = HistogramVec::new(
            HistogramOpts::new("octoroute_routing_duration_ms", "Routing decision latency in milliseconds")
                .buckets(vec![0.1, 0.5, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0]),
            &["strategy"],
        )?;
        registry.register(Box::new(routing_duration.clone()))?;

        // Counter: Model invocations by tier
        let model_invocations = CounterVec::new(
            Opts::new("octoroute_model_invocations_total", "Total model invocations by tier"),
            &["tier"],
        )?;
        registry.register(Box::new(model_invocations.clone()))?;

        Ok(Self {
            registry: Arc::new(registry),
            requests_total,
            routing_duration,
            model_invocations,
        })
    }

    /// Export metrics in Prometheus text format
    pub fn gather(&self) -> Result<String, prometheus::Error> {
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        TextEncoder::new().encode(&metric_families, &mut buffer)?;
        String::from_utf8(buffer).map_err(|e| prometheus::Error::Msg(format!("UTF-8 error: {}", e)))
    }

    /// Record a request with labels
    pub fn record_request(&self, tier: &str, strategy: &str) {
        self.requests_total.with_label_values(&[tier, strategy]).inc();
    }

    /// Record routing decision latency
    pub fn record_routing_duration(&self, strategy: &str, duration_ms: f64) {
        self.routing_duration.with_label_values(&[strategy]).observe(duration_ms);
    }

    /// Record model invocation
    pub fn record_model_invocation(&self, tier: &str) {
        self.model_invocations.with_label_values(&[tier]).inc();
    }
}
```

**Axum Handler:**

```rust
// Expose /metrics endpoint for Prometheus scraping
pub async fn handler(State(state): State<AppState>) -> (StatusCode, String) {
    match state.metrics() {
        Some(metrics) => match metrics.gather() {
            Ok(output) => (StatusCode::OK, output),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to gather metrics: {}", e)),
        },
        None => (StatusCode::NOT_FOUND, "Metrics not enabled. Build with --features metrics".to_string()),
    }
}
```

**Usage in Homelab:**

Homelab users can scrape `/metrics` with their existing Prometheus setup:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'octoroute'
    static_configs:
      - targets: ['localhost:3000']
    metrics_path: '/metrics'
    scrape_interval: 15s
```

**Available Metrics:**

- `octoroute_requests_total{tier, strategy}` - Total requests by tier (fast/balanced/deep) and strategy (rule/llm)
- `octoroute_routing_duration_ms{strategy}` - Histogram of routing decision latency
- `octoroute_model_invocations_total{tier}` - Total model invocations by tier

**Future**: OpenTelemetry traces could be added separately if distributed tracing is needed, but metrics work standalone with Prometheus.

### 7.3 Tracing Setup

```rust
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_telemetry() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "octoroute=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}
```

---

## 8. Performance Considerations

### 8.1 Latency Budget

| Component | Target Latency | Notes |
|-----------|----------------|-------|
| Rule routing | <1ms | Pure CPU, no I/O |
| LLM routing | 100-500ms | Depends on 30B model speed |
| Model invocation | 500-5000ms | Depends on model size and prompt |
| Total (rule path) | <6s | Dominated by model invocation |
| Total (LLM path) | <6.5s | +500ms for routing overhead |

### 8.2 Optimization Strategies

1. **Client pooling**: Reuse `open-agent-sdk` clients (avoid repeated connection setup)
2. **Caching**: Cache routing decisions for identical prompts (with TTL)
3. **Parallel requests**: Use `tokio::spawn` for independent operations
4. **Streaming**: Stream model responses to client (reduce TTFB)

### 8.3 Concurrency Model

- **Tokio runtime**: Default worker threads = CPU cores
- **Client locking**: Use `Arc<Mutex<Client>>` for thread-safe access
- **Request isolation**: Each request is an independent async task

---

## 9. Testing Strategy

### 9.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_router_casual_chat() {
        let router = RuleBasedRouter::default();
        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Fast8B));
    }

    #[test]
    fn test_rule_router_deep_analysis() {
        let router = RuleBasedRouter::default();
        let meta = RouteMetadata {
            token_estimate: 500,
            importance: Importance::High,
            task_type: TaskType::DeepAnalysis,
        };

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Deep120B));
    }
}
```

### 9.2 Integration Tests

```rust
#[tokio::test]
async fn test_chat_endpoint_with_rule_routing() {
    let app = create_test_app().await;

    let request = ChatRequest {
        prompt: "Hello!".to_string(),
        importance: Importance::Low,
        task_type: TaskType::CasualChat,
        model: None,
        temperature: None,
    };

    let response = app
        .oneshot(
            Request::builder()
                .uri("/chat")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request).unwrap()))
                .unwrap()
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
    let chat_response: ChatResponse = serde_json::from_slice(&body).unwrap();

    assert_eq!(chat_response.model_used, TargetModel::Fast8B);
    assert_eq!(chat_response.routing_strategy, "rule");
}
```

### 9.3 Property Tests

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn router_always_returns_valid_model(
        token_estimate in 0..10000usize,
        importance in prop_oneof![
            Just(Importance::Low),
            Just(Importance::Normal),
            Just(Importance::High),
        ]
    ) {
        let router = RuleBasedRouter::default();
        let meta = RouteMetadata {
            token_estimate,
            importance,
            task_type: TaskType::QuestionAnswer,
        };

        // Property: Router always returns a valid model (or None for delegation)
        let result = router.route(&meta);
        assert!(result.is_none() || matches!(result, Some(TargetModel::Fast8B | TargetModel::Balanced30B | TargetModel::Deep120B)));
    }
}
```

---

## 10. Implementation Plan

### Phase 1: Foundation (Week 1)

**Goals**: Basic HTTP server + rule-based routing + multi-model config schema

- [x] Initialize Cargo project with dependencies
- [x] Define `AppError` and `IntoResponse` implementation
- [x] Create `config.toml` schema and parser **with multi-model support**
  - [x] Support arrays of models per tier (fast, balanced, deep)
  - [x] Add `weight` and `priority` fields to `ModelEndpoint`
  - [x] Config validation (empty arrays, invalid values, port conflicts)
- [x] Implement `RuleBasedRouter`
- [x] Write unit tests for rule matching
- [x] Set up Axum server with `/health` endpoint
- [x] Add `tracing` setup

**Deliverable**: Server that responds to `/health` with rule router tested. Config supports multiple models per tier (structure only).

**Note**: Phase 1 establishes the multi-model *config structure* (arrays, weights, priorities) but does NOT implement model selection/invocation. Actual model selection logic and `/chat` endpoint are Phase 2 features.

### Phase 2: Model Integration + Load Balancing (Week 2)

**Goals**: Integrate `open-agent-sdk` for model clients + implement model selection

- [ ] Create `ModelClient` wrapper around `open_agent::Client`
- [ ] Implement `ModelSelector` for choosing from multiple models per tier
  - [ ] **Phase 2a: Simple selection** (first available or round-robin)
  - [ ] **Phase 2b: Weighted load balancing** (respects `weight` field)
  - [ ] **Phase 2c: Priority-based selection** (respects `priority` field)
  - [ ] Health checks for model availability
  - [ ] Retry logic with fallback to lower-weight/priority models
  - [ ] Circuit breaker pattern for failed models
- [ ] Implement `create_client_for(TargetModel)` factory that returns selected endpoint
- [ ] Add `/chat` endpoint with request validation
- [ ] Test against multiple fast tier models (load balancing)
- [ ] Add response streaming from `open-agent-sdk`
- [ ] Write integration tests

**Deliverable**: `/chat` endpoint routes requests to models with intelligent load balancing across multiple endpoints per tier

**Note**: Phase 2 brings the multi-model config to life by implementing actual model selection and invocation.

### Phase 3: LLM Routing (Week 3)

**Goals**: Add intelligent routing fallback

- [ ] Implement `LlmBasedRouter` with 30B client
- [ ] Create `HybridRouter` combining rule + LLM
- [ ] Add routing strategy to response
- [ ] Test LLM router with 30B model
- [ ] Add metrics for routing decisions
- [ ] Write property tests

**Deliverable**: Hybrid router delegates ambiguous cases to LLM

### Phase 4: Tool-Based Router (Week 4 - Optional)

**Goals**: Experimental tool-based routing

- [ ] Implement `ToolBasedRouter` with tool definitions
- [ ] Add `auto_execute_tools` support
- [ ] Test tool execution with hooks
- [ ] Compare performance vs LLM router
- [ ] Document trade-offs

**Deliverable**: Tool-based router as alternative strategy

### Phase 5: Polish & Observability (Production-Ready) ✅ COMPLETE

**Goals**: Production-ready features and operational tooling

- [x] Add `/models` endpoint with health checks
  - List all models by tier with health status
  - Show active/inactive endpoints
  - Provide endpoint metadata (URL, model name, priority, weight)
- [x] Implement Prometheus metrics (optional, behind `metrics` feature flag)
  - **Architecture Decision**: Switched from deprecated `opentelemetry-prometheus` to direct `prometheus` crate (v0.14)
  - Metrics: Request counts by tier, routing strategy distribution, latency histograms
  - Export to Prometheus format via `/metrics` endpoint
  - Three instruments: `requests_total`, `routing_duration_ms`, `model_invocations_total`
  - Document Prometheus scraping config for homelab users
- [x] Add request timeout handling
  - Configurable global timeout (1-300 seconds)
  - Per-tier timeout overrides (fast=15s, balanced=30s, deep=60s)
  - Proper timeout error responses with diagnostic messages
- [x] Create `justfile` with dev tasks
  - 20+ recipes including `test`, `bench`, `run`, `check`, `ci`, `build-release`
  - Comprehensive development workflow automation
- [x] Write comprehensive README
  - Quick start guide with examples
  - Configuration reference (including per-tier timeouts)
  - API documentation with curl examples
  - Architecture overview with diagrams
  - Observability setup (logs, metrics)
  - FAQ section with metrics/observability guidance
- [x] Add benchmarks for routing performance
  - Criterion benchmarks with async support
  - Metadata creation: ~940 picoseconds
  - Config parsing: ~9.7 microseconds
  - Token estimation: ~5-10 nanoseconds
  - All performance targets met (<1ms for pure CPU operations)
- [x] Set up CI/CD (GitHub Actions)
  - Test suite on push/PR (all 270 tests: 203 lib + 67 integration)
  - Clippy and rustfmt checks (zero warnings policy enforced)
  - MSRV testing (1.90.0) + stable
  - Benchmark compilation check
  - Documentation build validation
  - Cargo caching for ~80% faster builds

**Deliverable**: ✅ Production-ready router service ready for homelab deployment with full observability

**Final Stats**:
- 270 tests passing (203 lib + 67 integration)
- Zero clippy warnings
- Zero tech debt
- 7 commits for Phase 5 (ae9fe21 → 84b3c6d)
- All features documented and tested

---

## Appendix A: Configuration Schema

```toml
# config.toml

[server]
host = "0.0.0.0"
port = 3000
request_timeout_seconds = 30

# Multi-model support: Each tier can have multiple models for load balancing
# Phase 1: Simple selection (first available or round-robin)
# Phase 2: Weighted load balancing with health checks

# Fast tier models (e.g., 8B models for quick tasks)
[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://macmini-1:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0      # Load balancing weight (Phase 2)
priority = 1      # Higher priority models tried first (Phase 2)

[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://macmini-2:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

# Balanced tier models (e.g., 30B models for coding/analysis)
[[models.balanced]]
name = "qwen3-30b-instruct"
base_url = "http://lmstudio-host:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

# Deep tier models (e.g., 120B models for complex reasoning)
[[models.deep]]
name = "gpt-oss-120b"
base_url = "http://llamacpp-box:8080/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1

[routing]
# Strategy: "rule", "llm", "hybrid", "tool"
strategy = "hybrid"

# Default importance if not specified
default_importance = "normal"

# Use balanced tier for LLM-based routing
router_model = "balanced"

[observability]
# Log level: "trace", "debug", "info", "warn", "error"
log_level = "info"

# Metrics configuration (requires --features metrics)
# Exposes /metrics endpoint in Prometheus format via OpenTelemetry
metrics_enabled = false

# Optional: OTLP exporter for sending traces to OTEL collector
# If not set, only Prometheus export is available
# otlp_endpoint = "http://localhost:4317"
```

---

## Appendix B: References

1. [Axum Documentation](https://docs.rs/axum)
2. [Axum Error Handling](https://docs.rs/axum/latest/axum/error_handling)
3. [open-agent-sdk Repository](https://github.com/slb350/open-agent-sdk-rust)
4. [Tokio Documentation](https://tokio.rs)
5. [Tower Middleware](https://docs.rs/tower)
6. [thiserror Documentation](https://docs.rs/thiserror)

---

**Document Status**: Ready for implementation
**Next Review**: After Phase 1 completion
