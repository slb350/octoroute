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
│   ├── hybrid.rs             # Hybrid router (rule + LLM fallback)
│   └── metadata.rs           # RouteMetadata struct
│
├── models/                    # Model client management
│   ├── mod.rs
│   ├── client.rs             # Wrapper around open-agent-sdk Client
│   ├── selector/             # Model selection logic
│   │   ├── mod.rs            # ModelSelector with load balancing
│   │   └── balanced.rs       # BalancedSelector (type-safe tier selection)
│   ├── health.rs             # Health checking with background monitoring
│   └── endpoint_name.rs      # Type-safe endpoint identifiers
│
├── handlers/                  # HTTP request handlers
│   ├── mod.rs
│   ├── chat.rs               # POST /chat
│   ├── health.rs             # GET /health
│   ├── models.rs             # GET /models
│   └── metrics.rs            # GET /metrics
│
├── middleware/                # Axum middleware
│   ├── mod.rs
│   └── logging.rs            # Request/response tracing
│
├── metrics.rs                 # Prometheus metrics implementation
├── error.rs                   # AppError, AppResult types
└── telemetry.rs              # Tracing setup
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
    pub fn route(&self, meta: &RouteMetadata) -> Option<RoutingDecision> {
        use Importance::*;
        use TaskType::*;

        // Rule 1: Trivial/casual tasks → 8B
        if matches!(meta.task_type, CasualChat)
            && meta.token_estimate < 256
            && !matches!(meta.importance, High)
        {
            return Some(RoutingDecision::new(ModelTier::Fast));
        }

        // Rule 2: Medium-depth tasks → 30B
        if meta.token_estimate < 2048
            && !matches!(meta.task_type, DeepAnalysis | CreativeWriting)
        {
            return Some(RoutingDecision::new(ModelTier::Balanced));
        }

        // Rule 3: High importance or deep work → 120B
        if matches!(meta.importance, High)
            || matches!(meta.task_type, DeepAnalysis | CreativeWriting)
        {
            return Some(RoutingDecision::new(ModelTier::Deep));
        }

        // Rule 4: Code generation (special case)
        if matches!(meta.task_type, Code) {
            return if meta.token_estimate > 1024 {
                Some(RoutingDecision::new(ModelTier::Deep))
            } else {
                Some(RoutingDecision::new(ModelTier::Balanced))
            };
        }

        // No rule matched → return None for LLM fallback
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

---

### LLM-Based Router

**Purpose**: Intelligent routing for ambiguous or complex cases.

**Algorithm**:

```rust
pub struct LlmBasedRouter {
    balanced_selector: Arc<BalancedSelector>,
}

impl LlmBasedRouter {
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata
    ) -> Result<RoutingDecision, AppError> {
        let router_prompt = format!(
            "You are a router that chooses which LLM to use.\n\n\
             Available models:\n\
             - FAST: Quick (8B params), for simple chat, short Q&A, casual tasks.\n\
             - BALANCED: Good reasoning (30B params), coding, document summaries, explanations.\n\
             - DEEP: Deep reasoning (120B params), creative writing, complex analysis, research.\n\n\
             User request:\n{}\n\n\
             Metadata:\n\
             - Estimated tokens: {}\n\
             - Importance: {:?}\n\
             - Task type: {:?}\n\n\
             Respond with ONLY one of: FAST, BALANCED, DEEP",
            user_prompt.chars().take(MAX_ROUTER_RESPONSE).collect::<String>(),
            meta.token_estimate,
            meta.importance,
            meta.task_type
        );

        // Use balanced tier (30B) for routing decisions
        let endpoint = self.balanced_selector.select_endpoint(&ExclusionSet::new())?;

        // Query the router model
        let response = open_agent::query(&endpoint.base_url, &endpoint.name, &router_prompt)
            .await?;

        // Parse router decision (fuzzy matching with word boundaries)
        let normalized = response.trim().to_uppercase();
        let target = if normalized.contains("FAST") {
            ModelTier::Fast
        } else if normalized.contains("BALANCED") {
            ModelTier::Balanced
        } else {
            ModelTier::Deep // Default to largest for safety
        };

        Ok(RoutingDecision::new(target))
    }
}
```

**Performance**: 100-500ms (depends on 30B model speed).

**Trade-offs**:
- ✓ Intelligent, adaptive
- ✓ Can handle nuanced cases
- ✗ Adds latency (router invocation)
- ✗ Requires reliable 30B model
- ✗ Token cost for router prompt

---

### Hybrid Router (Default)

**Purpose**: Combine speed of rules with intelligence of LLM.

**Algorithm**:

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
    ) -> Result<RoutingDecision, AppError> {
        // Try rule-based first (fast path, ~70-80% of requests)
        if let Some(decision) = self.rule_router.route(meta) {
            tracing::info!(
                tier = ?decision.tier(),
                strategy = "rule",
                "Route decision made"
            );
            return Ok(decision);
        }

        // Fall back to LLM router for ambiguous cases (~20% of requests)
        tracing::debug!("No rule matched, delegating to LLM router");
        let decision = self.llm_router.route(user_prompt, meta).await?;
        tracing::info!(
            tier = ?decision.tier(),
            strategy = "llm",
            "Route decision made"
        );

        Ok(decision)
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
2. Middleware: Request logging, metrics
   ↓
3. Handler: Extract ChatRequest JSON
   ↓
4. Build RouteMetadata (token estimate, task type, importance)
   ↓
5. Check for explicit model tier override
   ├─ Yes → Skip routing, use specified tier
   └─ No  → Continue to router
           ↓
6. RuleBasedRouter.route(metadata)
   ├─ Rule matched → Return routing decision
   └─ No match    → LlmBasedRouter.route(prompt, metadata)
                     ↓
7. ModelSelector.select_endpoint(tier, exclusions)
   ├─ Filter by priority (highest priority tier)
   ├─ Filter by health status (exclude unhealthy)
   ├─ Weighted random selection within tier
   └─ Return selected endpoint
           ↓
8. Create open-agent-sdk Client for endpoint
   ↓
9. Client sends prompt to model
   ↓
10. Stream response blocks from model
   ↓
11. Accumulate text blocks into response
   ↓
12. Build ChatResponse (tier, strategy, timing, etc.)
   ↓
13. Return JSON response
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
   - 500: Internal error (config, logic)
   - 502: Bad Gateway (model invocation failed)
   - 503: Service Unavailable (all endpoints unhealthy)
   - 504: Gateway Timeout (request timeout exceeded)
```

### Health Check Flow

```
Background Task (every 30 seconds)
   ↓
For each endpoint:
   ├─ Send GET /models request
   ├─ Check HTTP status
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
- Server panics after exhausting retries (prevents silent degradation)

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

    #[error("Model invocation failed: {source}")]
    ModelInvocation {
        #[source]
        source: open_agent::Error,
    },

    #[error("All {tier:?} tier endpoints are unhealthy")]
    NoHealthyEndpoints {
        tier: ModelTier,
    },

    #[error("Routing failed: {0}")]
    RoutingFailed(String),

    #[error("Request timeout after {timeout_secs}s (attempt {attempt}/{max_attempts})")]
    Timeout {
        timeout_secs: u64,
        attempt: usize,
        max_attempts: usize,
    },

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::ModelInvocation { source } => {
                (StatusCode::BAD_GATEWAY, format!("Model error: {}", source))
            }
            AppError::NoHealthyEndpoints { tier } => {
                (StatusCode::SERVICE_UNAVAILABLE, format!("{:?} tier unavailable", tier))
            }
            AppError::RoutingFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::Timeout { timeout_secs, .. } => {
                (StatusCode::GATEWAY_TIMEOUT, format!("Request timeout after {}s", timeout_secs))
            }
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
2. **Map SDK errors**: Convert `open_agent::Error` to `AppError::ModelInvocation`
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
5. **Streaming**: Stream model responses to client (reduce TTFB)

### Concurrency Model

- **Tokio runtime**: Default worker threads = CPU cores
- **Client locking**: Use `Arc<Mutex<Client>>` for thread-safe access (LLM router only)
- **Request isolation**: Each request is an independent async task
- **Health monitoring**: Background task runs independently on 30-second interval

### Benchmark Results

All performance targets met:

- **Metadata creation**: ~940 picoseconds
- **Config parsing**: ~9.7 microseconds
- **Token estimation**: ~5-10 nanoseconds
- **Rule routing**: <1ms (pure CPU operations)

Benchmarks available via: `cargo bench`

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
