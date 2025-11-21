# Configuration Guide

Complete configuration reference for Octoroute.

---

## Table of Contents

1. [Overview](#overview)
2. [Configuration File](#configuration-file)
3. [Server Configuration](#server-configuration)
4. [Model Configuration](#model-configuration)
5. [Routing Configuration](#routing-configuration)
6. [Timeout Configuration](#timeout-configuration)
7. [Observability Configuration](#observability-configuration)
8. [Example Configurations](#example-configurations)

---

## Overview

Octoroute is configured via a TOML configuration file (default: `config.toml` in the working directory).

**Configuration Validation**:
- All configuration is validated at startup
- Invalid values cause immediate error with clear messages
- No runtime surprises from misconfiguration

**Configuration Reloading**:
- Currently requires server restart to apply changes
- Future versions may support hot-reloading

---

## Configuration File

### File Location

Default: `./config.toml` in the current working directory

**Note**: The configuration file path is currently hardcoded. The server must be run from the directory containing `config.toml`.

### File Format

TOML (Tom's Obvious, Minimal Language)

- Human-readable key-value pairs
- Strong typing via serde deserialization
- Comments supported with `#`

---

## Server Configuration

```toml
[server]
host = "0.0.0.0"
port = 3000
```

### Fields

- `host` (string, required): IP address to bind to
  - `"0.0.0.0"`: Listen on all interfaces
  - `"127.0.0.1"`: Listen only on localhost
- `port` (integer, required): Port number to listen on
  - Range: 1-65535
  - Recommended: 3000 (default) or any unused port

---

## Model Configuration

Multi-model support allows multiple endpoints per tier for load balancing and high availability.

### Structure

```toml
[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://macmini-1:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://macmini-2:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "qwen3-30b-instruct"
base_url = "http://lmstudio-host:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "gpt-oss-120b"
base_url = "http://llamacpp-box:8080/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1
```

### Fields

- `name` (string, required): Model name
  - Must match the model name on the endpoint server
  - Used in health checks (`HEAD {base_url}/models`)

- `base_url` (string, required): Model endpoint base URL
  - Must start with `http://` or `https://`
  - Must end with `/v1` (validated at parse time)
  - Example: `"http://localhost:11434/v1"`

- `max_tokens` (integer, required): Maximum tokens for responses
  - Must be > 0
  - Typical values: 4096 (8B), 8192 (30B), 16384 (120B)

- `temperature` (float, optional): Sampling temperature
  - Range: 0.0-2.0 typically
  - Default: 0.7
  - Lower = more deterministic, higher = more creative

- `weight` (float, optional): Load balancing weight
  - Must be > 0.0 and finite
  - Default: 1.0
  - Higher weight = more traffic
  - Example: `weight = 2.0` gets 2x traffic of `weight = 1.0`

- `priority` (integer, optional): Priority level
  - Higher values = tried first
  - Default: 1
  - Endpoints with same priority are weighted randomly
  - Example: `priority = 2` endpoints tried before `priority = 1`

### Tiers

Three tiers are supported:

- `models.fast`: 8B models for quick tasks
- `models.balanced`: 30B models for coding/analysis
- `models.deep`: 120B models for complex reasoning

Each tier must have at least one endpoint configured.

### Load Balancing

**Priority-Based Selection**:
1. Filter endpoints to highest priority tier only
2. Filter out unhealthy endpoints
3. Weighted random selection within remaining endpoints

**Example**:

```toml
# Endpoint A: priority=2, weight=1.0
# Endpoint B: priority=2, weight=2.0
# Endpoint C: priority=1, weight=5.0
```

Result: A gets 33% traffic, B gets 67% traffic, C gets 0% traffic (lower priority)

### Health Checking

**Background Health Checks**:
- Run every 30 seconds automatically
- Send `HEAD {base_url}/models` to each endpoint
- Track consecutive failures (unhealthy after 3 failures)
- Automatic recovery on successful requests

**Immediate Recovery**:
- Successful user requests reset failure counters immediately
- No need to wait for background health check

**Health Status**:
- View via `GET /models` endpoint
- `healthy: true` = endpoint is available
- `healthy: false` = endpoint failed 3+ consecutive health checks

---

## Routing Configuration

```toml
[routing]
strategy = "hybrid"
default_importance = "normal"
router_model = "balanced"
```

### Fields

- `strategy` (string, required): Routing strategy to use
  - `"rule"`: Rule-based only (fastest)
  - `"llm"`: LLM-based only (most intelligent)
  - `"hybrid"`: Rule-based with LLM fallback (recommended)
  - **Note**: `"tool"` is accepted by the config parser but rejected at runtime with a configuration error. Use `"rule"`, `"llm"`, or `"hybrid"` only.

- `default_importance` (string, optional): Default importance when not specified in request
  - Values: `"low"`, `"normal"`, `"high"`
  - Default: `"normal"` (if not specified)

- `router_model` (string, required): Specifies which tier should be used for LLM-based routing
  - Values: `"fast"`, `"balanced"`, `"deep"`
  - **Note**: Currently not implemented. LLM-based routing always uses the Balanced tier regardless of this setting.
  - This field is validated but the value is not consulted at runtime.
  - Recommended: Keep as `"balanced"` for future compatibility

### Routing Strategies

#### Rule-Based (`"rule"`)

- Fastest (< 1ms routing latency)
- Deterministic pattern matching
- No LLM overhead
- Limited to predefined rules

**Use Case**: Predictable workloads with clear task types

#### LLM-Based (`"llm"`)

- Intelligent routing for all requests
- Adaptive to nuanced cases
- Higher latency (+100-500ms)
- Requires reliable router tier endpoint

**Use Case**: Complex, varied workloads where routing quality matters most

#### Hybrid (`"hybrid"`) - Recommended

- Rule-based fast path (70-80% of requests)
- LLM fallback for ambiguous cases (20% of requests)
- Best balance of speed and intelligence

**Use Case**: General-purpose routing for mixed workloads

---

## Timeout Configuration

### Global Timeout

Configure in `[server]` section:

```toml
[server]
host = "0.0.0.0"
port = 3000
request_timeout_seconds = 30
```

- `request_timeout_seconds` (integer, optional): Default timeout for all requests
  - Range: 1-300 seconds
  - Default: 30 seconds if not specified
  - Applies per retry attempt (not cumulative)

### Per-Tier Timeout Overrides

Override timeouts for specific tiers in `[timeouts]` section:

```toml
[timeouts]
fast = 15      # Fast tier (8B) timeout in seconds
balanced = 30  # Balanced tier (30B) timeout in seconds
deep = 60      # Deep tier (120B) timeout in seconds
```

**Timeout Precedence**:
1. Tier-specific override from `[timeouts]` section (if set)
2. Global `server.request_timeout_seconds` (if set)
3. Default 30 seconds

**Note**: Endpoint-level `timeout_seconds` is NOT supported. Timeouts are configured per-tier, not per-endpoint.

### Retry Behavior

- Maximum 3 retry attempts per request
- Timeout applies per attempt (not cumulative)
- Failed endpoints excluded from retries within same request

**Worst-Case Latency**:
- 3 attempts × 30s timeout = 90s maximum total latency

**Example**: With deep tier timeout of 60s:
- 3 attempts × 60s = 180s maximum total latency

---

## Observability Configuration

```toml
[observability]
log_level = "info"
```

### Fields

- `log_level` (string, optional): Logging verbosity level
  - Values: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`
  - Default: `"info"` (if not specified)

### Log Levels

- `"trace"`: Very detailed, includes all internal operations
- `"debug"`: Detailed, includes routing decisions and metadata
- `"info"`: Normal, includes requests and routing strategy
- `"warn"`: Warnings only, unusual but non-critical events
- `"error"`: Errors only, failures and critical issues

**Recommended**:
- Development: `"debug"`
- Production: `"info"`
- Troubleshooting: `"debug"` or `"trace"`

### Environment Override

Override log level at runtime:

```bash
RUST_LOG=octoroute=debug cargo run
```

Supports per-module filtering:

```bash
RUST_LOG=octoroute=debug,octoroute::router=trace cargo run
```

---

## Example Configurations

### Minimal Configuration

Single endpoint per tier, simple setup:

```toml
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://localhost:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "qwen3-30b-instruct"
base_url = "http://localhost:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "gpt-oss-120b"
base_url = "http://localhost:8080/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_model = "balanced"

[observability]
log_level = "info"
```

---

### High Availability Configuration

Multiple endpoints per tier with load balancing:

```toml
[server]
host = "0.0.0.0"
port = 3000

# Fast tier: 3 endpoints with equal weight
[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://macmini-1:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://macmini-2:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://macmini-3:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

# Balanced tier: 2 endpoints with different weights
[[models.balanced]]
name = "qwen3-30b-instruct"
base_url = "http://lmstudio-1:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 2.0  # Higher spec machine
priority = 1

[[models.balanced]]
name = "qwen3-30b-instruct"
base_url = "http://lmstudio-2:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0  # Lower spec machine
priority = 1

# Deep tier: 1 high-priority + 1 fallback
[[models.deep]]
name = "gpt-oss-120b"
base_url = "http://llamacpp-box:8080/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 2  # Try this first

[[models.deep]]
name = "gpt-oss-120b"
base_url = "http://llamacpp-backup:8080/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1  # Fallback only

[routing]
strategy = "hybrid"
default_importance = "normal"
router_model = "balanced"

[timeouts]
fast = 20
balanced = 45
deep = 90

[observability]
log_level = "info"
```

---

### Performance-Focused Configuration

Optimized for low latency:

```toml
[server]
host = "127.0.0.1"
port = 3000

# Fast tier only (no balanced/deep tiers needed)
[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://localhost:11434/v1"
max_tokens = 2048  # Lower max for faster responses
temperature = 0.5  # More deterministic
weight = 1.0
priority = 1

# Balanced tier required for LLM routing
[[models.balanced]]
name = "qwen3-30b-instruct"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

# Deep tier for fallback
[[models.deep]]
name = "gpt-oss-120b"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"  # Rule-only for minimum latency
default_importance = "low"  # Bias toward fast tier
router_model = "balanced"

[timeouts]
fast = 10
balanced = 15
deep = 30

[observability]
log_level = "warn"  # Minimal logging overhead
```

---

### Development Configuration

Local testing setup:

```toml
[server]
host = "127.0.0.1"
port = 3000

# All endpoints on localhost for testing
[[models.fast]]
name = "qwen3-8b-instruct"
base_url = "http://localhost:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "qwen3-30b-instruct"
base_url = "http://localhost:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "gpt-oss-120b"
base_url = "http://localhost:8080/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_model = "balanced"

[timeouts]
fast = 60
balanced = 120
deep = 180  # Generous for local debugging

[observability]
log_level = "debug"  # Verbose for development
```

---

## Configuration Validation

All configuration is validated at startup with clear error messages:

**Invalid base_url**:
```
Configuration error: endpoint 'http://localhost:11434' must end with '/v1'
```

**Invalid weight**:
```
Configuration error: weight must be positive and finite, got -1.0
```

**Invalid timeout**:
```
Configuration error: Invalid timeout configuration: timeouts.deep cannot exceed 300 seconds (5 minutes), got 500. This limit prevents connection pool exhaustion and arithmetic overflow.
```

**Missing tier**:
```
Configuration error: models.fast must contain at least one model endpoint
```

**Invalid port**:
```
TOML parse error: invalid value: integer `70000`, expected u16 for key `server.port`
```

Note: Port validation is enforced by Rust's type system (u16 = 0-65535). Values outside this range fail during TOML parsing.

---

## Configuration Best Practices

1. **Start Simple**: Use minimal config with one endpoint per tier
2. **Add Load Balancing**: Scale up with multiple endpoints as needed
3. **Use Priority Tiers**: High-priority for main endpoints, low-priority for fallbacks
4. **Tune Timeouts**: Match timeouts to model performance (fast=15s, balanced=30s, deep=60s)
5. **Monitor Health**: Check `GET /models` regularly to verify endpoint health
6. **Test Changes**: Validate config changes in development before production
7. **Document Custom Settings**: Add comments explaining non-standard configuration

---

## Troubleshooting

### Server Won't Start

Check configuration validation errors in startup logs:

```bash
cargo run 2>&1 | grep "Configuration error"
```

### Endpoints Always Unhealthy

1. Verify `base_url` ends with `/v1`
2. Check endpoint is reachable: `curl http://endpoint:port/v1/models`
3. Verify model name matches endpoint: `curl http://endpoint:port/v1/models | jq`

### Routing Always Uses Same Tier

1. Check routing strategy: `"rule"` may always match same tier
2. Try `"hybrid"` or `"llm"` for more dynamic routing
3. Review rule logic in [architecture.md](architecture.md)

### Timeouts Too Short/Long

1. Monitor actual response times via `/metrics`
2. Adjust per-tier timeouts in `[timeouts]` section (`fast`, `balanced`, `deep`) based on observed latency
3. Consider model performance when setting timeouts
