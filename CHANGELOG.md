# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2025-11-24

### Added

#### Core Features
- **Intelligent Multi-Model Routing**: Automatically route requests to optimal model tier (Fast/8B, Balanced/30B, Deep/120B) based on task characteristics
- **Three Routing Strategies**:
  - `rule`: Fast pattern-based routing (<1ms latency)
  - `llm`: LLM-powered intelligent routing (~250ms latency)
  - `hybrid`: Rule-based with LLM fallback (recommended)
- **Multi-Endpoint Support**: Configure multiple endpoints per tier for load balancing and high availability
- **Priority-Based Selection**: Endpoints with higher priority are tried first, with weighted random selection within same priority
- **Health Checking**: Background health monitoring with automatic endpoint recovery
  - Consecutive failure threshold (3 failures = unhealthy)
  - Immediate recovery on successful requests
  - 30-second health check interval

#### HTTP API
- `POST /chat`: Submit chat requests with intelligent routing
- `GET /health`: System health status with detailed subsystem reporting
- `GET /models`: List all model endpoints with health status
- `GET /metrics`: Prometheus metrics endpoint

#### Observability
- **Prometheus Metrics**:
  - `octoroute_requests_total{tier, strategy}`: Request counts
  - `octoroute_routing_duration_ms{strategy}`: Routing latency histogram
  - `octoroute_model_invocations_total{tier}`: Model invocations
  - `octoroute_health_tracking_failures_total{endpoint, error_type}`: Health tracking failures
  - `octoroute_metrics_recording_failures_total{operation}`: Metrics recording failures
  - `octoroute_background_health_task_failures_total`: Background task restarts
- **Structured Logging**: Human-readable logs via `tracing` with configurable log levels
- **Request Warnings**: Non-fatal issues surfaced in API responses

#### Configuration
- TOML-based configuration with comprehensive validation
- Per-tier timeout overrides
- Configurable router tier for LLM/hybrid strategies
- Weight and priority settings for load balancing

#### Reliability
- Retry logic with request-scoped endpoint exclusion (max 3 attempts)
- Exponential backoff between retries
- Graceful degradation when endpoints fail
- Background health task auto-restart (max 5 restarts)

#### Developer Experience
- Comprehensive test suite (235+ unit tests, 46 integration test files)
- Criterion benchmarks for routing performance
- justfile with 20+ development recipes
- Zero clippy warnings policy
- Pre-commit hooks for code quality

### Technical Details

- **Framework**: Axum 0.8 on Tokio async runtime
- **LLM SDK**: open-agent-sdk 0.6 for model invocation
- **Minimum Rust Version**: 1.90.0 (Edition 2024)
- **Dependencies**: Updated to latest stable versions
  - `toml` 0.9
  - `criterion` 0.7
  - `rand` 0.9

### Documentation

- Architecture guide with system diagrams
- Complete API reference with examples
- Configuration guide with validation error examples
- Observability guide with Grafana dashboard examples
- Development guide with TDD workflow
- Deployment guide (binary, systemd, Docker)

---

[0.1.0]: https://github.com/slb350/octoroute/releases/tag/v0.1.0
