# Observability Guide

Logging, metrics, and monitoring for Octoroute.

---

## Table of Contents

1. [Overview](#overview)
2. [Structured Logging](#structured-logging)
3. [Prometheus Metrics](#prometheus-metrics)
4. [Health Monitoring](#health-monitoring)
5. [Grafana Dashboards](#grafana-dashboards)
6. [Troubleshooting](#troubleshooting)

---

## Overview

Octoroute provides comprehensive observability through:

- **Structured Logging**: Human-readable formatted logs with rich context via `tracing`
- **Prometheus Metrics**: Direct Prometheus integration for monitoring routing decisions and model usage
- **Health Checks**: Background health monitoring with automatic recovery

---

## Structured Logging

### Log Format

Octoroute uses `tracing` for structured logging with human-readable output by default.

**Example Log Output**:

```
2025-11-20T15:30:42.123456Z  INFO octoroute::handlers::chat: Chat request received importance=Normal task_type=QuestionAnswer prompt_len=42
2025-11-20T15:30:42.125678Z DEBUG octoroute::router::hybrid: No rule matched, delegating to LLM router
2025-11-20T15:30:42.423456Z  INFO octoroute::router::hybrid: Route decision made tier=Balanced strategy="llm"
2025-11-20T15:30:42.456789Z  INFO octoroute::models::selector: Selected endpoint endpoint="http://lmstudio-host:1234/v1" model="qwen3-30b-instruct"
2025-11-20T15:30:43.987654Z  INFO octoroute::handlers::chat: Request completed tier=Balanced strategy="llm" duration_ms=1864
```

### Log Levels

Configure via `config.toml`:

```toml
[observability]
log_level = "info"
```

Or override with environment variable:

```bash
RUST_LOG=octoroute=debug cargo run
```

**Levels**:
- `trace`: Very detailed (every function call, variable state)
- `debug`: Detailed (routing decisions, health checks, metadata)
- `info`: Normal (requests, routing strategy, completions)
- `warn`: Warnings (health degradation, retry attempts)
- `error`: Errors (failures, timeouts, configuration issues)

**Recommended**:
- Development: `debug`
- Production: `info`
- Troubleshooting: `debug` or `trace`

### Per-Module Filtering

Enable verbose logging for specific modules:

```bash
# Trace router decisions, info for everything else
RUST_LOG=octoroute=info,octoroute::router=trace cargo run

# Debug health checks only
RUST_LOG=octoroute=info,octoroute::models::health=debug cargo run

# Trace LLM router specifically
RUST_LOG=octoroute=info,octoroute::router::llm_based=trace cargo run
```

### JSON Logging

For log aggregation (ELK, Loki, etc.), enable JSON output:

```bash
# Future: OCTOROUTE_LOG_FORMAT=json cargo run
# Currently outputs human-readable format only
```

### Key Log Events

**Request Handling**:
```
INFO octoroute::handlers::chat: Chat request received importance=High task_type=Code prompt_len=256
```

**Routing Decisions**:
```
INFO octoroute::router::hybrid: Route decision made tier=Deep strategy="rule"
DEBUG octoroute::router::rule_based: Rule matched rule="high_importance" tier=Deep
```

**Model Selection**:
```
INFO octoroute::models::selector: Selected endpoint endpoint="http://localhost:8080/v1" model="gpt-oss-120b"
DEBUG octoroute::models::selector: Priority filtering priority=2 candidates=2
DEBUG octoroute::models::selector: Health filtering healthy=1 unhealthy=1
```

**Health Checks**:
```
INFO octoroute::models::health: Health check passed endpoint="http://localhost:11434/v1"
WARN octoroute::models::health: Health check failed endpoint="http://localhost:1234/v1" error="connection refused" consecutive_failures=1
ERROR octoroute::models::health: Endpoint marked unhealthy endpoint="http://localhost:1234/v1" consecutive_failures=3
```

**Errors**:
```
ERROR octoroute::handlers::chat: Request failed error="Request timeout after 30s" attempt=3
ERROR octoroute::models::health: Background health check task crashed error="panic: ..." restart_attempt=2
```

---

## Prometheus Metrics

### Metrics Endpoint

**URL**: `http://localhost:3000/metrics`

**Format**: Prometheus text exposition format

**Authentication**: None (unauthenticated by default - see security note below)

### Available Metrics

**Cardinality Safety**: Octoroute uses **type-safe enums** for metric labels to prevent cardinality explosion:

- `Tier` enum: 3 variants (Fast, Balanced, Deep)
- `Strategy` enum: 2 variants (Rule, Llm)
- **Maximum cardinality**: 3 tiers × 2 strategies = **6 time series** for `octoroute_requests_total`

**Why This Matters**: Using raw strings for labels could create unbounded time series (e.g., if user input leaked into labels). Type-safe enums enforce compile-time cardinality bounds.

**Note**: `Strategy::Hybrid` exists in the routing configuration but is **never recorded** as a metric label. Hybrid router records either "rule" or "llm" based on which path was taken.

---

#### octoroute_requests_total

**Type**: Counter

**Description**: Total number of chat requests

**Labels**:
- `tier`: Model tier used (`fast`, `balanced`, `deep`)
- `strategy`: Routing strategy (`rule`, `llm`)

**Note**: `strategy="hybrid"` is never recorded. Hybrid router records either `"rule"` or `"llm"` based on which path was taken.

**Example**:
```
octoroute_requests_total{tier="fast",strategy="rule"} 142
octoroute_requests_total{tier="balanced",strategy="rule"} 58
octoroute_requests_total{tier="balanced",strategy="llm"} 23
octoroute_requests_total{tier="deep",strategy="rule"} 15
```

**Cardinality**: 6 time series maximum (3 tiers × 2 strategies)

---

#### octoroute_routing_duration_ms

**Type**: Histogram

**Description**: Routing decision latency in milliseconds

**Labels**:
- `strategy`: Routing strategy (`rule`, `llm`)

**Buckets**: 0.1, 0.5, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0 ms

**Example**:
```
octoroute_routing_duration_ms_bucket{strategy="rule",le="0.1"} 95
octoroute_routing_duration_ms_bucket{strategy="rule",le="0.5"} 142
octoroute_routing_duration_ms_bucket{strategy="rule",le="1.0"} 142
octoroute_routing_duration_ms_sum{strategy="rule"} 45.2
octoroute_routing_duration_ms_count{strategy="rule"} 142

octoroute_routing_duration_ms_bucket{strategy="llm",le="100.0"} 15
octoroute_routing_duration_ms_bucket{strategy="llm",le="500.0"} 23
octoroute_routing_duration_ms_sum{strategy="llm"} 4832.5
octoroute_routing_duration_ms_count{strategy="llm"} 23
```

**Interpretation**:
- Rule routing: ~0.3ms average (45.2ms / 142 requests)
- LLM routing: ~210ms average (4832.5ms / 23 requests)

**Cardinality**: 2 time series (2 strategies)

---

#### octoroute_model_invocations_total

**Type**: Counter

**Description**: Total model invocations by tier

**Labels**:
- `tier`: Model tier (`fast`, `balanced`, `deep`)

**Example**:
```
octoroute_model_invocations_total{tier="fast"} 142
octoroute_model_invocations_total{tier="balanced"} 81
octoroute_model_invocations_total{tier="deep"} 15
```

**Note**: This metric only counts successful user model invocations (requests to `/chat`). Internal LLM router decisions are NOT recorded in this metric.

**Cardinality**: 3 time series (3 tiers)

---

### Prometheus Configuration

Add Octoroute to your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'octoroute'
    static_configs:
      - targets: ['localhost:3000']
    metrics_path: '/metrics'
    scrape_interval: 15s
    scrape_timeout: 10s
```

**For remote hosts**:
```yaml
scrape_configs:
  - job_name: 'octoroute'
    static_configs:
      - targets: ['octoroute.homelab.local:3000']
    metrics_path: '/metrics'
    scrape_interval: 15s
```

### Useful PromQL Queries

**Request rate by tier**:
```promql
rate(octoroute_requests_total[5m])
```

**Routing strategy distribution**:
```promql
sum by (strategy) (increase(octoroute_requests_total[1h]))
```

**Average routing latency**:
```promql
rate(octoroute_routing_duration_ms_sum[5m]) / rate(octoroute_routing_duration_ms_count[5m])
```

**95th percentile routing latency**:
```promql
histogram_quantile(0.95, rate(octoroute_routing_duration_ms_bucket[5m]))
```

**Model tier utilization**:
```promql
sum by (tier) (rate(octoroute_model_invocations_total[5m]))
```

**Rule router hit rate**:
```promql
sum(rate(octoroute_requests_total{strategy="rule"}[5m]))
/
sum(rate(octoroute_requests_total[5m])) * 100
```

---

## Health Monitoring

### Health Check Endpoint

**URL**: `http://localhost:3000/health`

**Response**: `"OK"` (plain text)

**Use Case**: Simple uptime monitoring (does not check model availability)

### Model Health Endpoint

**URL**: `http://localhost:3000/models`

**Response**: JSON with detailed health status

**Example**:
```json
{
  "models": [
    {
      "name": "qwen3-8b-instruct",
      "tier": "fast",
      "endpoint": "http://macmini-1:11434/v1",
      "healthy": true,
      "last_check_seconds_ago": 2,
      "consecutive_failures": 0
    },
    {
      "name": "qwen3-8b-instruct",
      "tier": "fast",
      "endpoint": "http://macmini-2:11434/v1",
      "healthy": false,
      "last_check_seconds_ago": 45,
      "consecutive_failures": 3
    }
  ]
}
```

**Use Case**: Detailed model availability monitoring

### Background Health Checks

**Frequency**: Every 30 seconds

**Method**: `HEAD {base_url}/models` to each endpoint (note: HEAD request, not GET)

**Failure Threshold**: 3 consecutive failures → mark unhealthy

**Recovery**: Immediate on successful request (reset failure counter)

**Restart Logic**:
- Background task can crash (panics, unexpected exits)
- Automatic restart with exponential backoff (1s, 2s, 4s, 8s, 16s)
- Maximum 5 restart attempts
- After 5 failures, background health checking stops but server continues (graceful degradation)
- Health status remains at last known state without background updates

### Health Monitoring with Prometheus

**Note**: Octoroute does not export per-endpoint health metrics. Health status is available via the `GET /models` endpoint.

**Monitor via request success rate**:

```yaml
groups:
  - name: octoroute
    rules:
      - alert: OctorouteNoSuccessfulRequests
        expr: rate(octoroute_requests_total[5m]) == 0
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "Octoroute has processed no successful requests in 5 minutes"
```

**Alternative**: Monitor error responses via HTTP status code metrics (if using reverse proxy with access logs).

---

## Grafana Dashboards

### Example Dashboard

Create a Grafana dashboard with these panels:

#### Request Rate by Tier

**Query**:
```promql
sum by (tier) (rate(octoroute_requests_total[5m]))
```

**Visualization**: Time series line graph

---

#### Routing Strategy Distribution

**Query**:
```promql
sum by (strategy) (increase(octoroute_requests_total[1h]))
```

**Visualization**: Pie chart

---

#### Routing Latency (p50, p95, p99)

**Queries**:
```promql
# p50
histogram_quantile(0.50, rate(octoroute_routing_duration_ms_bucket[5m]))

# p95
histogram_quantile(0.95, rate(octoroute_routing_duration_ms_bucket[5m]))

# p99
histogram_quantile(0.99, rate(octoroute_routing_duration_ms_bucket[5m]))
```

**Visualization**: Time series line graph

---

#### Model Tier Utilization

**Query**:
```promql
sum by (tier) (rate(octoroute_model_invocations_total[5m]))
```

**Visualization**: Stacked area chart

---

#### Rule Router Hit Rate

**Query**:
```promql
sum(rate(octoroute_requests_total{strategy="rule"}[5m]))
/
sum(rate(octoroute_requests_total[5m])) * 100
```

**Visualization**: Gauge (0-100%)

**Interpretation**: Higher = more requests handled by fast rule-based routing

---

### Dashboard Template

A complete Grafana dashboard JSON template is available in the repository:

```bash
# Future: Export dashboard JSON to docs/grafana-dashboard.json
# Currently: Build custom dashboard using queries above
```

---

## Troubleshooting

### No Metrics Appearing in Prometheus

1. **Check metrics endpoint**:
   ```bash
   curl http://localhost:3000/metrics
   ```

2. **Verify Prometheus config**:
   - Target URL correct?
   - Port accessible?
   - Firewall blocking?

3. **Check Prometheus targets**:
   - Navigate to Prometheus UI → Status → Targets
   - Verify Octoroute target is "UP"

---

### Logs Not Appearing

1. **Check log level**:
   ```toml
   [observability]
   log_level = "info"  # Must be info or lower
   ```

2. **Check environment override**:
   ```bash
   unset RUST_LOG  # Remove any env overrides
   ```

3. **Verify stderr**:
   - Logs write to stderr by default
   - Check stderr redirection in deployment

---

### High Routing Latency

1. **Check routing strategy**:
   - LLM routing adds 100-500ms
   - Rule routing should be <1ms

2. **Monitor LLM router tier**:
   - Balanced tier slow?
   - Check balanced tier endpoint health

3. **Review PromQL**:
   ```promql
   histogram_quantile(0.95, rate(octoroute_routing_duration_ms_bucket{strategy="llm"}[5m]))
   ```

---

### Endpoints Flapping (healthy → unhealthy → healthy)

1. **Check endpoint stability**:
   - Is model server overloaded?
   - Network issues?

2. **Increase health check interval**:
   - Currently hardcoded to 30s
   - Future: Configurable interval

3. **Review health check logs**:
   ```bash
   RUST_LOG=octoroute::models::health=debug cargo run
   ```

---

### Background Health Check Task Crashed

**Symptom**: Log message showing restart attempts

**Cause**: Panic in health check code or resource exhaustion

**Resolution**:
1. Check logs for panic message
2. Verify endpoint URLs are valid
3. Check network connectivity to all endpoints
4. If 5 restart attempts exhausted, server continues with degraded health checking (last known status)
5. Restart server to restore full health monitoring functionality

---

## Security Note

**The `/metrics` endpoint is unauthenticated by default.**

**What is exposed**:
- Request counts by tier and strategy
- Routing latency statistics
- Model invocation counts

**What is NOT exposed**:
- User prompts or responses
- Model outputs
- IP addresses
- Authentication credentials

**Recommendations for deployment**:

1. **Reverse proxy with authentication** (nginx, Caddy):
   ```nginx
   location /metrics {
       auth_basic "Metrics";
       auth_basic_user_file /etc/nginx/.htpasswd;
       proxy_pass http://localhost:3000/metrics;
   }
   ```

2. **Firewall rules** (restrict to Prometheus server):
   ```bash
   sudo ufw allow from 192.168.1.10 to any port 3000  # Prometheus server only
   ```

3. **Network segmentation** (bind to management network):
   ```toml
   [server]
   host = "192.168.100.10"  # Management network
   port = 3000
   ```

See [Deployment Guide](deployment.md) for complete security hardening.

---

## Best Practices

1. **Start with `log_level = "info"`**: Provides good balance of detail vs noise
2. **Monitor routing strategy distribution**: Aim for 70-80% rule-based routing
3. **Alert on unhealthy endpoints**: Set up Prometheus alerts for degraded health
4. **Track p95 latency**: Monitor routing latency to catch LLM router slowdowns
5. **Review logs during incidents**: Switch to `debug` when troubleshooting
6. **Secure metrics endpoint**: Use reverse proxy or firewall rules in production
7. **Dashboard everything**: Build Grafana dashboards for visibility
