# API Reference

Complete HTTP API documentation for Octoroute.

---

## Table of Contents

1. [Overview](#overview)
2. [Request/Response Format](#requestresponse-format)
3. [Endpoints](#endpoints)
4. [Error Responses](#error-responses)
5. [Examples](#examples)

---

## Overview

Octoroute exposes a simple HTTP API for routing requests to local LLM endpoints. All endpoints return JSON responses.

**Base URL**: `http://localhost:3000` (configurable via `config.toml`)

**Content-Type**: All requests and responses use `application/json`

---

## Request/Response Format

### Common Types

#### Importance

Optional importance level hint for routing decisions.

```json
"importance": "low" | "normal" | "high"
```

Default: `"normal"`

#### TaskType

Optional task type hint for routing decisions.

```json
"task_type": "casual_chat" | "code" | "creative_writing" | "deep_analysis" | "document_summary" | "question_answer"
```

Default: `"question_answer"`

#### ModelTier

Represents which model tier was selected (appears in responses).

```json
"model_tier": "fast" | "balanced" | "deep"
```

- `fast`: 8B models for quick tasks
- `balanced`: 30B models for coding/analysis
- `deep`: 120B models for complex reasoning

---

## Endpoints

### POST /chat

Submit a chat request and get a routed response.

#### Request Body

```json
{
  "message": "string (required)",
  "importance": "low | normal | high (optional, default: normal)",
  "task_type": "casual_chat | code | creative_writing | deep_analysis | document_summary | question_answer (optional, default: question_answer)"
}
```

**Fields**:

- `message` (string, required): The user's message or question
- `importance` (enum, optional): Importance level for routing decisions
- `task_type` (enum, optional): Task type hint for routing decisions

Routing tier is chosen automatically based on routing logic; manual tier overrides are not supported.

#### Response Body

```json
{
  "content": "string",
  "model_tier": "fast | balanced | deep",
  "model_name": "string",
  "routing_strategy": "rule | llm",
  "warnings": ["string"] // optional, omitted if empty
}
```

**Fields**:

- `content` (string): The model's response text
- `model_tier` (enum): Which tier was used (fast/balanced/deep)
- `model_name` (string): Specific model name that handled the request
- `routing_strategy` (string): How the routing decision was made
  - `"rule"`: Rule-based router matched a pattern
  - `"llm"`: LLM router made the decision (ambiguous case)
  - **Note**: Never returns `"hybrid"`. Hybrid routing configuration returns either `"rule"` or `"llm"` based on which path was taken.
- `warnings` (array, optional): Non-fatal warnings encountered during routing. Omitted if empty.
  - Examples: health tracking failures, metrics recording issues

#### Status Codes

- `200 OK`: Request successful
- `400 Bad Request`: Invalid request (empty message, invalid enum values)
- `500 Internal Server Error`: Configuration error, routing failed, or health check failed
- `502 Bad Gateway`: Stream interrupted, model query failed, or LLM routing error
- `504 Gateway Timeout`: Endpoint timeout exceeded

---

### GET /health

Health check endpoint for monitoring.

#### Response Body

```json
{
  "status": "OK",
  "health_tracking_status": "operational | degraded",
  "metrics_recording_status": "operational | degraded",
  "background_task_status": "operational | degraded",
  "background_task_failures": 0
}
```

**Fields**:

- `status` (string): Always "OK" if server is running
- `health_tracking_status` (enum): Health of mark_success/mark_failure operations
  - `"operational"`: No failures
  - `"degraded"`: Some health tracking failures occurred
- `metrics_recording_status` (enum): Health of Prometheus metrics recording
  - `"operational"`: No failures
  - `"degraded"`: Some metrics recording failures occurred
- `background_task_status` (enum): Health of background health check task
  - `"operational"`: Task running normally
  - `"degraded"`: Task has restarted due to failures
- `background_task_failures` (integer): Number of background task restarts

#### Status Codes

- `200 OK`: Server is running

**Note**: This endpoint checks internal system health. Use `GET /models` for individual endpoint health status.

---

### GET /models

List all configured model endpoints with health status.

#### Response Body

```json
{
  "models": [
    {
      "name": "string",
      "tier": "fast | balanced | deep",
      "endpoint": "string",
      "healthy": true | false,
      "last_check_seconds_ago": 5,
      "consecutive_failures": 0
    }
  ]
}
```

**Fields**:

- `name` (string): Model name (e.g., "qwen3-8b-instruct")
- `tier` (enum): Which tier this model belongs to
- `endpoint` (string): Base URL for the model endpoint
- `healthy` (boolean): Current health status
- `last_check_seconds_ago` (integer): Seconds since last health check
- `consecutive_failures` (integer): Number of consecutive health check failures

**Note on Health Reporting**:

On server startup, all endpoints initialize as `healthy: true` with `last_check_seconds_ago` reflecting time since process start. This optimistic health status remains until the first background health check runs (~30 seconds after boot) or a user request updates the status. For the first 30 seconds of operation, health data should be considered provisional until actual endpoint probes complete.

#### Status Codes

- `200 OK`: Models list retrieved successfully

---

### GET /metrics

Prometheus metrics endpoint for monitoring.

#### Response Format

Prometheus text exposition format.

#### Example Response

```
# HELP octoroute_requests_total Total number of chat requests
# TYPE octoroute_requests_total counter
octoroute_requests_total{tier="fast",strategy="rule"} 42
octoroute_requests_total{tier="balanced",strategy="llm"} 15

# HELP octoroute_routing_duration_ms Routing decision latency in milliseconds
# TYPE octoroute_routing_duration_ms histogram
octoroute_routing_duration_ms_bucket{strategy="rule",le="0.1"} 30
octoroute_routing_duration_ms_bucket{strategy="rule",le="0.5"} 42
octoroute_routing_duration_ms_bucket{strategy="rule",le="+Inf"} 42
octoroute_routing_duration_ms_sum{strategy="rule"} 12.5
octoroute_routing_duration_ms_count{strategy="rule"} 42

# HELP octoroute_model_invocations_total Total model invocations by tier
# TYPE octoroute_model_invocations_total counter
octoroute_model_invocations_total{tier="fast"} 42
octoroute_model_invocations_total{tier="balanced"} 15
```

#### Metrics

**Core Metrics**:

- `octoroute_requests_total{tier, strategy}`: Total requests by tier and routing strategy
- `octoroute_routing_duration_ms{strategy}`: Histogram of routing decision latency
- `octoroute_model_invocations_total{tier}`: Total model invocations by tier

**Health/Observability Metrics**:

- `octoroute_health_tracking_failures_total{endpoint, error_type}`: Health tracking failures (mark_success/mark_failure)
- `octoroute_metrics_recording_failures_total{operation}`: Prometheus metrics recording failures
- `octoroute_background_health_task_failures_total`: Background health check task restarts

#### Status Codes

- `200 OK`: Metrics exported successfully
- `500 Internal Server Error`: Failed to gather metrics

**Security Note**: This endpoint is unauthenticated. See [Deployment Guide](deployment.md) for security recommendations.

---

### POST /v1/chat/completions (OpenAI-Compatible)

OpenAI-compatible chat completions endpoint. Drop-in replacement for OpenAI API clients.

#### Request Body

```json
{
  "model": "auto | fast | balanced | deep | <endpoint-name>",
  "messages": [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": "Hello!"}
  ],
  "stream": false,
  "temperature": 0.7,
  "max_tokens": 1000
}
```

**Fields**:

- `model` (string, required): Model selection
  - `"auto"` - Use LLM/hybrid routing to select optimal tier
  - `"fast"` / `"balanced"` / `"deep"` - Route directly to that tier
  - Specific endpoint name (e.g., `"qwen3-8b"`) - Bypass routing, use endpoint directly
- `messages` (array, required): Conversation history
- `stream` (boolean, optional): Enable SSE streaming (default: `false`)
- `temperature` (number, optional): Sampling temperature 0.0-2.0 (default: `0.7`)
- `max_tokens` (integer, optional): Maximum tokens to generate

#### Response Body (Non-Streaming)

```json
{
  "id": "chatcmpl-abc123",
  "object": "chat.completion",
  "created": 1699000000,
  "model": "qwen3-8b",
  "choices": [{
    "index": 0,
    "message": {"role": "assistant", "content": "Hello!"},
    "finish_reason": "stop"
  }],
  "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
}
```

#### Response (Streaming)

When `stream: true`, returns Server-Sent Events (SSE):

```
data: {"id":"chatcmpl-abc123","object":"chat.completion.chunk","choices":[{"delta":{"role":"assistant"}}]}

data: {"id":"chatcmpl-abc123","object":"chat.completion.chunk","choices":[{"delta":{"content":"Hello"}}]}

data: {"id":"chatcmpl-abc123","object":"chat.completion.chunk","choices":[{"finish_reason":"stop"}]}

data: [DONE]
```

#### Retry Behavior

**Important**: Retry behavior differs based on model selection:

- **Tier-based requests** (`auto`, `fast`, `balanced`, `deep`): Automatic retry with endpoint exclusion. If an endpoint fails, the request retries on a different endpoint in the same tier (up to 3 attempts with exponential backoff).

- **Specific model requests** (e.g., `"qwen3-8b"`): **No automatic retry**. If the specified endpoint fails, the request fails immediately. This is because specific model selection indicates the user wants that exact endpoint.

- **Streaming requests**: **No automatic retry** regardless of model selection. Once streaming begins, mid-stream failures cannot be retried. Error events are sent to the client with request IDs for debugging.

#### Warning Headers

Non-fatal issues are reported via the `X-Octoroute-Warning` response header:

```
X-Octoroute-Warning: health tracking failed: endpoint not found (endpoint health state may be stale)
```

**Note on Streaming**: Warning headers cannot be modified after streaming begins. For streaming requests, health tracking warnings are logged server-side but not surfaced to clients. Check server logs for full observability.

#### Status Codes

- `200 OK`: Request successful
- `400 Bad Request`: Invalid request (empty messages, invalid parameters)
- `500 Internal Server Error`: Configuration error or routing failed
- `502 Bad Gateway`: Model query failed or stream interrupted
- `504 Gateway Timeout`: Endpoint timeout exceeded

---

### GET /v1/models (OpenAI-Compatible)

List available models in OpenAI-compatible format.

#### Response Body

```json
{
  "object": "list",
  "data": [
    {
      "id": "auto",
      "object": "model",
      "owned_by": "octoroute",
      "description": "Auto-routing (LLM/hybrid selects optimal tier)"
    },
    {
      "id": "fast",
      "object": "model",
      "owned_by": "octoroute-tier",
      "description": "Fast tier routing"
    },
    {
      "id": "qwen3-8b",
      "object": "model",
      "owned_by": "octoroute-endpoint",
      "description": "Direct endpoint access"
    }
  ]
}
```

**Model Types**:

- `owned_by: "octoroute"` - Virtual routing models (`auto`)
- `owned_by: "octoroute-tier"` - Tier selectors (`fast`, `balanced`, `deep`)
- `owned_by: "octoroute-endpoint"` - Direct endpoint access

---

## Error Responses

All errors return JSON with an `error` field:

```json
{
  "error": "Human-readable error message"
}
```

### Error Scenarios

#### 400 Bad Request

**Cause**: Invalid request (validation failed)

**Examples**:
- Empty message: `{"error": "message cannot be empty or contain only whitespace"}`
- Invalid importance: `{"error": "unknown variant 'urgent', expected 'low', 'normal', or 'high'"}`

#### 500 Internal Server Error

**Cause**: Configuration or routing logic error

**Examples**:
- `{"error": "Configuration error: no endpoints defined for Fast tier"}`
- `{"error": "Routing failed: unable to determine target tier"}`

#### 502 Bad Gateway

**Cause**: Model invocation failed

**Examples**:
- `{"error": "Failed to query model at http://localhost:1234/v1: connection refused"}`
- `{"error": "Stream interrupted from http://localhost:1234/v1 after receiving 1024 bytes (5 blocks)"}`
- `{"error": "Router LLM returned unparseable response: The answer is maybe"}`

#### 504 Gateway Timeout

**Cause**: Request exceeded configured timeout

**Example**:
- `{"error": "Request to http://localhost:1234/v1 timed out after 30 seconds"}`

---

## Examples

### Basic Chat Request

```bash
curl -X POST http://localhost:3000/chat \
  -H "Content-Type: application/json" \
  -d '{
    "message": "What is the capital of France?"
  }'
```

**Response**:

```json
{
  "content": "The capital of France is Paris.",
  "model_tier": "fast",
  "model_name": "qwen3-8b-instruct",
  "routing_strategy": "rule"
}
```

**Routing Decision**: Simple question → rule-based router → fast tier (8B model)

---

### High Importance Request

```bash
curl -X POST http://localhost:3000/chat \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Analyze the implications of quantum computing on cryptography.",
    "importance": "high",
    "task_type": "deep_analysis"
  }'
```

**Response**:

```json
{
  "content": "Quantum computing poses significant challenges to current cryptographic systems...",
  "model_tier": "deep",
  "model_name": "gpt-oss-120b",
  "routing_strategy": "rule"
}
```

**Routing Decision**: High importance + deep analysis → rule-based router → deep tier (120B model)

---

### Code Generation Request

```bash
curl -X POST http://localhost:3000/chat \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Write a function to parse JSON in Rust with error handling.",
    "task_type": "code"
  }'
```

**Response**:

```json
{
  "content": "Here's a Rust function to parse JSON with proper error handling:\n\n```rust\nuse serde_json::Value;\n...",
  "model_tier": "balanced",
  "model_name": "qwen3-30b-instruct",
  "routing_strategy": "rule"
}
```

**Routing Decision**: Code task, moderate size → rule-based router → balanced tier (30B model)

---

### Ambiguous Request (LLM Routing)

```bash
curl -X POST http://localhost:3000/chat \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Tell me about Rust",
    "importance": "high",
    "task_type": "casual_chat"
  }'
```

**Response**:

```json
{
  "content": "Rust is a systems programming language...",
  "model_tier": "balanced",
  "model_name": "qwen3-30b-instruct",
  "routing_strategy": "llm"
}
```

**Routing Decision**: Ambiguous (casual chat but high importance) → no rule match → LLM router decides → balanced tier

**Routing Latency**: +100-500ms for LLM routing decision

---

### Check Model Health

```bash
curl http://localhost:3000/models
```

**Response**:

```json
{
  "models": [
    {
      "name": "qwen3-8b-instruct-1",
      "tier": "fast",
      "endpoint": "http://macmini-1:11434/v1",
      "healthy": true,
      "last_check_seconds_ago": 2,
      "consecutive_failures": 0
    },
    {
      "name": "qwen3-8b-instruct-2",
      "tier": "fast",
      "endpoint": "http://macmini-2:11434/v1",
      "healthy": false,
      "last_check_seconds_ago": 45,
      "consecutive_failures": 3
    },
    {
      "name": "qwen3-30b-instruct",
      "tier": "balanced",
      "endpoint": "http://lmstudio-host:1234/v1",
      "healthy": true,
      "last_check_seconds_ago": 1,
      "consecutive_failures": 0
    },
    {
      "name": "gpt-oss-120b",
      "tier": "deep",
      "endpoint": "http://llamacpp-box:8080/v1",
      "healthy": true,
      "last_check_seconds_ago": 3,
      "consecutive_failures": 0
    }
  ]
}
```

**Interpretation**:
- Fast tier: 2 endpoints (1 healthy, 1 unhealthy)
- Balanced tier: 1 endpoint (healthy)
- Deep tier: 1 endpoint (healthy)

---

### Scrape Metrics

```bash
curl http://localhost:3000/metrics
```

**Response**: Prometheus text format (see `/metrics` endpoint documentation above)

**Use Case**: Configure Prometheus to scrape this endpoint for monitoring routing decisions and model usage.

---

## Rate Limiting

Octoroute does not implement rate limiting. Consider using a reverse proxy (nginx, Caddy) for rate limiting in production.

## Authentication

Octoroute does not implement authentication. All endpoints are unauthenticated by default. See [Deployment Guide](deployment.md) for security recommendations.
