# Octoroute Architecture

**Version**: 1.0
**Last Updated**: 2025-11-20

---

## Table of Contents

1. [System Overview](#system-overview)
2. [Architecture](#architecture)
3. [Routing Strategies](#routing-strategies)
4. [Data Flow](#data-flow)
5. [Error Handling](#error-handling)
6. [Performance Considerations](#performance-considerations)

---

## System Overview

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
                     │ { message, importance, task_type }
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
│  │  │ Rule     │  │ LLM      │  │ Hybrid           │     │  │
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
│  │  Model Invocation (open-agent-sdk)                    │  │
│  │  - Build AgentOptions per request                     │  │
│  │  - Call open_agent::query() with endpoint + prompt    │  │
│  │  - Buffer and return full response                    │  │
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

## Architecture

### Technology Stack

| Layer | Technology | Rationale |
|-------|------------|-----------|
| HTTP Server | Axum 0.8 | Ergonomic, Tower ecosystem, same runtime as SDK |
| Async Runtime | Tokio 1.x | Industry standard, used by `open-agent-sdk` |
| LLM SDK | `open-agent-sdk` 0.6 | Streaming, tools, hooks, context management |
| Configuration | TOML + serde | Human-readable, type-safe parsing |
| Error Handling | thiserror | Rich error context, `IntoResponse` integration |
| Logging | tracing + tracing-subscriber | Structured logging with spans |
| Metrics | prometheus 0.14 | Direct Prometheus integration, homelab-friendly |
| Testing | criterion | Benchmarks for routing performance |

### Module Hierarchy

```
octoroute/
├── src/
│   ├── main.rs                    # Axum server entrypoint
│   ├── lib.rs                     # Public library API
│   │
│   ├── config.rs                  # Configuration management (ModelConfig, RoutingConfig, etc.)
│   │
│   ├── router/                    # Routing strategies
│   │   ├── mod.rs                # Router enum, RouteMetadata, Importance, TaskType
│   │   ├── rule_based.rs         # Fast pattern-based routing
│   │   ├── llm_based.rs          # LLM-powered routing (30B)
│   │   └── hybrid.rs             # Hybrid router (rule + LLM fallback)
│   │
│   ├── models/                    # Model client management
│   │   ├── mod.rs
│   │   ├── client.rs             # ModelClient (unused, reserved for future tool-based routing)
│   │   ├── selector/             # Model selection logic
│   │   │   ├── mod.rs            # ModelSelector with load balancing
│   │   │   └── balanced.rs       # BalancedSelector (type-safe tier selection)
│   │   ├── health.rs             # Health checking with background monitoring
│   │   └── endpoint_name.rs      # Type-safe endpoint identifiers
│   │
│   ├── handlers/                  # HTTP request handlers
│   │   ├── mod.rs
│   │   ├── chat.rs               # POST /chat
│   │   ├── health.rs             # GET /health
│   │   ├── models.rs             # GET /models
│   │   └── metrics.rs            # GET /metrics
│   │
│   ├── middleware/                # Axum middleware
│   │   ├── mod.rs
│   │   └── request_id.rs         # Request ID generation and propagation
│   │
│   ├── metrics.rs                 # Prometheus metrics implementation
│   ├── error.rs                   # AppError, AppResult types
│   └── telemetry.rs              # Tracing setup
├── tests/                         # Integration tests
├── benches/                       # Benchmarks
└── Cargo.toml
```

### Key Design Decisions

#### Why Axum over Actix-web?

- Built on Tokio (same async runtime as `open-agent-sdk`)
- Minimal overhead, ergonomic extractors
- Tower middleware ecosystem for observability
- Type-safe routing and error handling

#### Why Direct Prometheus Integration?

- **Simplicity**: No intermediate abstraction layers
- **Homelab-friendly**: Works with existing Prometheus/Grafana stacks out of the box
- **Stability**: Mature, actively maintained `prometheus` crate (v0.14)
- **Zero overhead**: No OTEL collector or additional infrastructure required

Note: `lazy_static` is not a direct dependency. It's a transitive dependency pulled in by `prometheus` internally. The metrics module uses `Arc<Registry>` for thread-safe shared state.

#### Configuration via TOML

- Human-readable and version control friendly
- Standard in Rust ecosystem (Cargo.toml, etc.)
- Strong type safety via serde deserialization
- Validation at parse time prevents runtime errors

---

## Routing Strategies

### Rule-Based Router

**Purpose**: Fast, deterministic routing with zero LLM overhead.

**Algorithm**:

```rust
pub struct RuleBasedRouter;

impl RuleBasedRouter {
    pub async fn route(
        &self,
        _user_prompt: &str,
        meta: &RouteMetadata,
        _selector: &ModelSelector,
    ) -> AppResult<Option<RoutingDecision>> {
        // Try rule-based matching
        if let Some(target) = self.evaluate_rules(meta) {
            return Ok(Some(RoutingDecision::new(target, RoutingStrategy::Rule)));
        }

        // No rule matched - return None to signal caller should use fallback
        Ok(None)
    }

    fn evaluate_rules(&self, meta: &RouteMetadata) -> Option<TargetModel> {
        use Importance::*;
        use TaskType::*;

        // Rule 1: Trivial/casual tasks → Fast tier
        if matches!(meta.task_type, CasualChat)
            && meta.token_estimate < 256
            && !matches!(meta.importance, High)
        {
            return Some(TargetModel::Fast);
        }

        // Rule 2: High importance or deep work → Deep tier
        // (Check this BEFORE medium-depth rule to prioritize importance)
        // (Exclude CasualChat + High as it's ambiguous → delegate to LLM)
        if (matches!(meta.importance, High) && !matches!(meta.task_type, CasualChat))
            || matches!(meta.task_type, DeepAnalysis | CreativeWriting)
        {
            return Some(TargetModel::Deep);
        }

        // Rule 3: Code generation (special case)
        if matches!(meta.task_type, Code) {
            return if meta.token_estimate > 1024 {
                Some(TargetModel::Deep)
            } else {
                Some(TargetModel::Balanced)
            };
        }

        // Rule 4: Medium-depth tasks → Balanced tier
        // (Only non-code, non-deep tasks with sufficient complexity)
        // (Minimum 200 tokens to justify balanced model)
        if meta.token_estimate >= 200
            && meta.token_estimate < 2048
            && matches!(meta.task_type, QuestionAnswer | DocumentSummary)
        {
            return Some(TargetModel::Balanced);
        }

        // No rule matched → delegate to LLM router
        None
    }
}
```

**Performance**: <1ms per routing decision (pure CPU, no I/O).

**Trade-offs**:
- ✓ Fast, predictable
- ✓ No model invocation overhead
- ✗ Limited by rule expressiveness
- ✗ Requires manual tuning

**Fallback Behavior** (Rule-only strategy):
When no rule matches and `strategy = "rule"`, the router uses `ModelSelector::default_tier()` which selects the highest-priority tier available. This provides a deterministic fallback without LLM overhead.

---

### LLM-Based Router

**Purpose**: Intelligent routing for ambiguous or complex cases.

**Algorithm**:

```rust
pub struct LlmBasedRouter {
    selector: BalancedSelector,
}

impl LlmBasedRouter {
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata
    ) -> AppResult<RoutingDecision> {
        // Build router prompt with truncation for safety
        let router_prompt = Self::build_router_prompt(user_prompt, meta);

        // Retry up to MAX_ROUTER_RETRIES (2) times with different endpoints
        let mut failed_endpoints = ExclusionSet::new();

        for attempt in 1..=MAX_ROUTER_RETRIES {
            // Select endpoint from balanced tier (with health filtering + exclusions)
            let endpoint = match self.selector.select_balanced(&failed_endpoints).await {
                Some(ep) => ep.clone(),
                None => {
                    return Err(AppError::RoutingFailed(format!(
                        "No healthy balanced tier endpoints for routing \
                        (attempt {}/{})", attempt, 2  // MAX_ROUTER_RETRIES
                    )));
                }
            };

            // Try to query this endpoint
            let query_result = self
                .try_router_query(&endpoint, &router_prompt, attempt, MAX_ROUTER_RETRIES)
                .await;

            match query_result {
                Ok(target_model) => {
                    // Success! Mark endpoint healthy and return
                    self.selector.health_checker().mark_success(endpoint.name()).await?;
                    return Ok(RoutingDecision::new(target_model, RoutingStrategy::Llm));
                }
                Err(e) if Self::is_retryable_error(&e) => {
                    // Retryable error - try different endpoint
                    failed_endpoints.insert(endpoint.name().into());
                    continue;
                }
                Err(e) => {
                    // Systemic error - fail immediately
                    return Err(e);
                }
            }
        }

        Err(AppError::RoutingFailed("All 2 router retry attempts exhausted".to_string()))
    }

    fn build_router_prompt(user_prompt: &str, meta: &RouteMetadata) -> String {
        // Truncate user prompt to prevent prompt injection via context overflow
        const MAX_USER_PROMPT_CHARS: usize = 500;

        let char_count = user_prompt.chars().count();
        let truncated_prompt = if char_count > MAX_USER_PROMPT_CHARS {
            let truncated: String = user_prompt.chars().take(MAX_USER_PROMPT_CHARS).collect();
            format!("{}... [truncated]", truncated)
        } else {
            user_prompt.to_string()
        };

        format!(
            "You are a router that chooses which LLM to use.\n\n\
             Available models:\n\
             - FAST: Quick (small params), for simple chat, short Q&A, casual tasks.\n\
             - BALANCED: Good reasoning (medium params), coding, document summaries, explanations.\n\
             - DEEP: Deep reasoning (large params), creative writing, complex analysis, research.\n\n\
             User request:\n{}\n\n\
             Metadata:\n\
             - Estimated tokens: {}\n\
             - Importance: {:?}\n\
             - Task type: {:?}\n\n\
             Based on the above, respond with ONLY one word: FAST, BALANCED, or DEEP.\n\
             Do not include explanations or other text.",
            truncated_prompt, meta.token_estimate, meta.importance, meta.task_type
        )
    }
}
```

**Error Handling**:
- Unparseable responses (no FAST/BALANCED/DEEP found) return an error and fail the request
- Refusal patterns (CANNOT, ERROR, etc.) are detected and return an error
- No silent fallback to any tier - routing failures are explicit

**Performance**: 100-500ms (depends on 30B model speed).

**Trade-offs**:
- ✓ Intelligent, adaptive
- ✓ Can handle nuanced cases
- ✗ Adds latency (router invocation)
- ✗ Requires reliable 30B model
- ✗ Token cost for router prompt
- ✗ Can fail if router model malfunctions

---

### Hybrid Router (Default)

**Purpose**: Combine speed of rules with intelligence of LLM.

**Algorithm**:

```rust
pub struct HybridRouter {
    rule_router: RuleBasedRouter,
    llm_router: Arc<dyn LlmRouter>,
    selector: Arc<ModelSelector>,
}

impl HybridRouter {
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata
    ) -> AppResult<RoutingDecision> {
        // Try rule-based first (fast path, ~70-80% of requests)
        match self
            .rule_router
            .route(user_prompt, meta, &self.selector)
            .await?
        {
            Some(decision) => {
                tracing::info!(
                    target = ?decision.target(),
                    strategy = "rule",
                    "Route decision made"
                );
                Ok(decision)
            }
            None => {
                // Fall back to LLM router for ambiguous cases (~20% of requests)
                tracing::debug!("No rule matched, delegating to LLM router");
                let decision = self.llm_router.route(user_prompt, meta).await?;
                tracing::info!(
                    target = ?decision.target(),
                    strategy = "llm",
                    "Route decision made"
                );

                Ok(decision)
            }
        }
    }
}
```

**Routing Logic**:
- **Rule-based first**: Fast path with zero latency for obvious cases
- **LLM fallback**: Intelligent routing for ambiguous requests instead of defaulting to a specific tier
- **Performance**: 70-80% hit rule-based path (<1ms), 20% use LLM fallback (+100-500ms)

**Trade-offs**:
- ✓ Best of both worlds
- ✓ Fast for common cases
- ✓ Intelligent for edge cases
- ✗ More complex implementation

---

## Data Flow

### Request Flow (Hybrid Router)

```
1. HTTP Request arrives at Axum
   ↓
2. Middleware: Request ID generation and propagation
   ↓
3. Handler: Extract ChatRequest JSON
   ↓
4. Build RouteMetadata (token estimate, task type, importance)
   ↓
5. RuleBasedRouter.route(metadata)
   ├─ Rule matched → Return routing decision
   └─ No match    → LlmBasedRouter.route(prompt, metadata)
                     ↓
6. ModelSelector.select_endpoint(tier, exclusions)
   ├─ Filter by priority (highest priority tier)
   ├─ Filter by health status (exclude unhealthy)
   ├─ Weighted random selection within tier
   └─ Return selected endpoint
           ↓
7. Build AgentOptions for endpoint and invoke with open_agent::query()
   ↓
8. Stream response from model endpoint
   ↓
9. Buffer full response and return to client
   ↓
10. Build ChatResponse with model tier, routing strategy, and content
   ↓
11. Return JSON response to client
```

### Error Flow

```
Error occurs at any step
   ↓
Map to AppError variant
   ↓
AppError::into_response()
   ↓
Return HTTP error response
   - 400: Invalid request (validation)
   - 500: Internal error (config, routing, health check failures)
   - 502: Bad Gateway (stream interrupted, model query failed, LLM routing error)
   - 504: Gateway Timeout (endpoint timeout exceeded)
```

### Health Check Flow

```
Background Task (every 30 seconds)
   ↓
For each endpoint:
   ├─ Send HEAD {base_url}/models request
   ├─ Check HTTP status (no response body)
   ├─ Update health status
   │  ├─ Success → mark_success() (reset failure counter)
   │  └─ Failure → mark_failure() (increment counter, unhealthy after 3)
   └─ Log health state changes
```

**Immediate Recovery**:
- Successful user requests call `mark_success()` immediately
- Endpoints recover from unhealthy state as soon as they respond successfully
- Background checks provide periodic validation even without user traffic

**Restart Logic**:
- Background health check task can crash (panics, unexpected exits)
- Automatic restart with exponential backoff (1s, 2s, 4s, 8s, 16s)
- Maximum 5 restart attempts before giving up
- After 5 failures, background health checking stops but server continues (graceful degradation)
- Health status remains at last known state without background updates

---

## Error Handling

### Error Type Hierarchy

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

    #[error("Routing failed: {0}")]
    RoutingFailed(String),

    #[error("Stream interrupted from {endpoint} after receiving {bytes_received} bytes ({blocks_received} blocks)")]
    StreamInterrupted {
        endpoint: String,
        bytes_received: usize,
        blocks_received: usize,
    },

    #[error("Request to {endpoint} timed out after {timeout_seconds} seconds")]
    EndpointTimeout {
        endpoint: String,
        timeout_seconds: u64,
    },

    #[error("Health check failed for {endpoint}: {reason}")]
    HealthCheckFailed {
        endpoint: String,
        reason: String,
    },

    #[error("Failed to query model at {endpoint}: {reason}")]
    ModelQueryFailed {
        endpoint: String,
        reason: String,
    },

    #[error(transparent)]
    LlmRouting(#[from] LlmRouterError),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            Self::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            Self::RoutingFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            Self::StreamInterrupted { .. } => (StatusCode::BAD_GATEWAY, self.to_string()),
            Self::EndpointTimeout { .. } => (StatusCode::GATEWAY_TIMEOUT, self.to_string()),
            Self::HealthCheckFailed { .. } => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            Self::ModelQueryFailed { .. } => (StatusCode::BAD_GATEWAY, self.to_string()),
            Self::LlmRouting(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
        };

        let body = Json(serde_json::json!({
            "error": message,
        }));

        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
```

### Error Classification

**Systemic Errors** (do not retry, fail immediately):
- Configuration errors (invalid TOML, missing required fields)
- HTTP client creation failures (invalid URLs, SSL errors)
- Empty response streams (model sent no data)
- JSON deserialization errors (invalid model response format)

**Transient Errors** (retry with different endpoint):
- Connection timeouts (endpoint unreachable)
- Stream errors with partial data (network interruption)
- HTTP 5xx errors (temporary server issues)
- Health check failures (endpoint temporarily down)

**Retry Logic**:
- Maximum 3 retry attempts per request
- Request-scoped exclusion: Never retry same endpoint within single request
- Global health tracking: 3 consecutive failures → mark endpoint unhealthy
- Endpoints recover immediately on successful requests

### Error Handling Best Practices

1. **Use `?` operator**: Propagate errors up to handler
2. **Map SDK errors**: Convert `open_agent::Error` to `AppError::ModelQueryFailed` or `AppError::LlmRouting`
3. **Validate early**: Check request validity before routing
4. **Log errors**: Use `tracing::error!` for all error paths
5. **Never panic**: All errors should be handled gracefully (except health monitor exhaustion)

---

## Performance Considerations

### Latency Budget

| Component | Target Latency | Notes |
|-----------|----------------|-------|
| Rule routing | <1ms | Pure CPU, no I/O |
| LLM routing | 100-500ms | Depends on 30B model speed |
| Model invocation | 500-5000ms | Depends on model size and prompt |
| Total (rule path) | <6s | Dominated by model invocation |
| Total (LLM path) | <6.5s | +500ms for routing overhead |

### Timeout Configuration

**Global Timeout**:
- Default: 30 seconds
- Range: 1-300 seconds (validated at config parse time)
- Applies per request attempt (not cumulative across retries)

**Per-Tier Timeout Overrides**:
- Fast tier: 15 seconds (8B models respond quickly)
- Balanced tier: 30 seconds (30B models need more time)
- Deep tier: 60 seconds (120B models require patience)

**Worst-Case Latency**:
- 3 retry attempts × 30s timeout = 90s maximum total latency
- Operator should tune timeouts based on model performance

### Optimization Strategies

1. **Health-aware selection**: Skip unhealthy endpoints immediately
2. **Weighted load balancing**: Distribute load according to endpoint capacity
3. **Priority-based fallback**: Try high-priority endpoints first
4. **Parallel requests**: Use `tokio::spawn` for independent operations

### Concurrency Model

- **Tokio runtime**: Default worker threads = CPU cores
- **Stateless invocation**: Uses `open_agent::query()` for per-request model calls (no client pooling)
- **Request isolation**: Each request is an independent async task
- **Health monitoring**: Background task runs independently on 30-second interval

### Performance Benchmarks

Performance benchmarks are available to measure:
- Metadata creation latency
- Config parsing overhead
- Token estimation speed
- Rule routing decision time

Run `cargo bench` to measure current performance on your hardware.

---

## References

1. [Axum Documentation](https://docs.rs/axum)
2. [Axum Error Handling](https://docs.rs/axum/latest/axum/error_handling)
3. [open-agent-sdk Repository](https://github.com/slb350/open-agent-sdk-rust)
4. [Tokio Documentation](https://tokio.rs)
5. [Tower Middleware](https://docs.rs/tower)
6. [thiserror Documentation](https://docs.rs/thiserror)
7. [Prometheus Rust Client](https://docs.rs/prometheus)

---

**Document Status**: Current as of Phase 5 completion
**Next Review**: As architecture evolves
