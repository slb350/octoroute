# Changelog

<!-- markdownlint-disable MD024 -->

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Build the executable v3 runtime with authenticated OpenAI-compatible ingress,
  virtual-model discovery, local-pool admission, and held-first-byte streaming.
- Add a lazy HTTP provider registry with isolated credentials and concurrency,
  bounded timeouts, schema-preserving OpenAI-compatible dispatch, explicit
  OpenRouter Auto shaping, ordered pre-commit fallback, provider readiness, and
  bounded response headers. Unsupported protocols and missing credentials fail
  closed without prompt disclosure.

### Changed

- Make version 3 the only executable/configuration contract: `config.toml`, the
  CLI generator, laptop profile, startup path, crate metadata, public
  integration tests, and operator documentation now use the inference fabric.
- Move reusable environment, capability, privacy, HTTP-limit, error, and
  held-first-byte streaming primitives under neutral or fabric ownership.

### Removed

- Remove the runtime version switch and the superseded single-local/OpenRouter
  service, semantic forecaster, calibration command, session latch, metrics,
  configuration parser, transport, documentation, and tests.


## [2.2.2] - 2026-08-23

### Security

- Update `zerovec-derive` from 0.11.4 to 0.11.6 and `zerovec` from 0.11.7
  to 0.11.8, fixing the high-severity invalid multi-element ULE validation
  described by GHSA-7fx9-626j-vqph. The vulnerable validator could accept
  invalid bit patterns and permit undefined behavior. Manifest floors also
  protect builds that do not consume Octoroute's tracked lockfile.

### Changed

- Refresh other Rust 1.90-compatible locked dependencies, including
  `quinn-proto` 0.11.17, `icu_provider` 2.3.1, `uuid` 1.25.0, and their
  companion maintenance releases.

## [2.2.1] - 2026-08-22

### Security

- Constrain the HTTP/2 transport graph to `h2` 0.4.16 or newer and lock
  0.4.18, preventing unbounded processing of empty DATA frames described by
  RUSTSEC-2026-0258. The manifest floor also protects downstream builds that
  do not consume Octoroute's tracked lockfile.

### Changed

- Pin the development toolchain to Rust 1.97.1 while retaining Rust 1.90 as
  the minimum supported version and explicit CI compatibility target.
- Refresh Rust 1.90-compatible locked dependencies, including Futures 0.3.34,
  rustls-webpki 0.103.14, ICU4X 2.3.0, and their companion packages.

## [2.2.0] - 2026-08-14

### Changed

- Replace binary local-model route selection with a strict local-success
  forecast and deterministic, configurable threshold policy. Shadow remains
  the default while the provisional thresholds await labeled calibration.
- Add a versioned local model capability card derived from configured local identity
  and capabilities, including measured limitations and anti-framing guidance;
  calibration binds rows to a SHA-256 fingerprint of the rendered card.
- Expose bounded local-success probability histograms by semantic mode and
  capability boundary without using generated text as metric labels.
- Add an offline `calibrate` command for strict labeled JSONL artifacts,
  calibration bins, Brier score, threshold sweeps, baseline comparison,
  latency, cloud outcomes, and estimated cloud cost. Bin bounds are explicit,
  and online/offline threshold equality shares one roundoff-safe comparator.
- Add shadow-only deterministic trajectory evidence from strict, paired typed
  tool results, with bounded error, recovery, environment, test, and context
  signals and fail-closed abstention.
- Add an opt-in enforced-mode cloud-only session latch using consecutive hard
  forecast evidence, bounded SHA-256-hashed state, TTL expiry, deterministic
  eviction, and forced-local overrides.
- Add deterministic request-ID-based shadow sampling with a full-observation
  default, bounded sampled/skipped metrics, and enforced-mode isolation.

### Fixed

- Bind capability cards and calibration artifacts to a required immutable local
  model revision, so changing weights under a stable alias invalidates prior
  calibration.
- Keep calibration failures bounded for malformed UTF-8, reject cloud costs
  that cannot produce finite reports, and align offline bins with Prometheus's
  upper-inclusive deciles.
- Sweep expired session state on every latch operation and preserve active
  latches under capacity pressure by evicting pending entries only.
- Expose a dedicated gateway correlation header without overwriting safe
  upstream request IDs, and restore lowercase `RouteDestination`
  deserialization for downstream callers.

## [2.1.2] - 2026-08-08

### Fixed

- Fail closed to cloud for automatic requests with malformed message objects,
  roles, content shapes, or typed content blocks instead of treating them as
  plain local chat.
- Require local tool capability for tool-call history even when the current
  request omits top-level tool definitions.
- Restrict locally eligible typed content to llama.cpp's verified Chat
  Completions block names and shapes, preventing unsupported aliases from
  reaching a capability-enabled local upstream.

## [2.1.1] - 2026-08-02

### Security

- Refresh the locked runtime dependency graph to `rustls` 0.23.43, which
  fixes a pre-authentication panic in the non-default aws-lc-rs stateless
  resumption ticketer and two QUIC validation or panic defects. Octoroute's
  current reqwest TLS path uses the unaffected ring provider.
- Update `http` to 1.5.0 so URI path-and-query parsing enforces its maximum
  length consistently.

### Changed

- Refresh six additional Rust 1.90-compatible lockfile dependencies covering
  CLI help, proc-macro, IP subnet iteration, and TOML parsing fixes.

## [2.1.0] - 2026-08-01

### Added

- Add `routing.semantic_mode` with `disabled`, `shadow`, and `enforced`
  behavior so operators can bypass or evaluate semantic classification before
  allowing it to select cloud.
- Expose bounded `octoroute_semantic_decisions_total{mode,outcome}` metrics
  for local, cloud, and failed classifier observations.

### Changed

- Default semantic routing to non-enforcing `shadow` mode after external
  evaluation measured 44% enforced-route accuracy versus 73% for the
  compatible always-local baseline.
- Reuse reserved local capacity after shadow decisions and classifier failures
  so observations do not race the request's subsequent local admission.

### Fixed

- Restore the opt-in evidence gate for semantic enforcement and document its
  measured 760–1500 ms latency cost.

## [2.0.1] - 2026-07-26

### Fixed

- Restored task-aware `auto` routing: local model now makes a constrained local
  semantic decision before compatible automatic work is admitted for local
  inference.
- Route requests that need stronger intelligence to `openrouter/auto`, even
  when local capacity is healthy and idle.
- Fail invalid, timed-out, or unavailable semantic decisions safely to cloud
  without weakening explicit local or local-only privacy.

### Security

- Pin every third-party GitHub Action and the CI `cargo-audit` installer to
  reviewed versions, and default workflow token permissions to read-only
  repository contents.

## [2.0.0] - 2026-07-25

### Added

- Local-first OpenAI-compatible gateway for local model llama.cpp and OpenRouter.
- Deterministic `auto`, `local`, `cloud`, local-alias, and explicit
  `provider/model` routing intent.
- Exact llama.cpp health, free-slot, input-token, context, and local
  concurrency admission.
- OpenRouter Auto Beta policy injection with authoritative cost-quality and
  optional model-allowlist settings.
- Bearer authentication, bounded request/header sizes, rate limiting, and
  global/local/cloud concurrency ceilings.
- Opaque streaming proxying with pre-commit local-to-cloud fallback,
  cancellation-safe permits, safe response headers, and generation IDs.
- Local-only privacy enforcement through explicit local intent or
  `X-Octoroute-Privacy: local-only`.
- Liveness, aggregated readiness, bounded Prometheus metrics, and route
  diagnostics.

### Changed

- `model: auto` now chooses the local or cloud destination instead of a local
  size tier.
- Configuration now requires `config_version = 2` and environment-backed
  credentials.
- OpenRouter owns cloud model/provider selection; Octoroute owns only the
  local-versus-cloud boundary.
- The HTTP data path now preserves the original JSON schema and upstream body
  stream instead of reconstructing requests through an agent SDK.
- Immediate cloud routes now skip local feature inference, cloud request
  mutation consumes the parsed body, local serialization avoids cloning the
  request DOM, and stale llama.cpp health and slot probes run concurrently.
- CI now enforces the locked dependency graph, denied RustSec warnings,
  benchmark compilation, and current GitHub-hosted Action runtimes.
- Local agent instruction files are explicitly excluded from crate packages.

### Removed

- Fast, balanced, and deep tier names and configuration.
- Rule/LLM/hybrid tier classifiers and local endpoint load balancing.
- The legacy `POST /chat` endpoint.
- The `open-agent-sdk` production data path.

### Fixed

- Treat null output-token limits as unset during local context admission.
- Route unknown or malformed message content blocks to cloud instead of
  assuming text-only local compatibility.
- Preserve an allowlisted upstream `X-Request-Id` on committed responses.

See [the v1-to-v2 migration guide](docs/migration-v2.md).

## [1.0.0] - 2025-11-27

### Added

#### OpenAI-Compatible API (Major Feature)

- **Drop-in replacement for OpenAI clients**: Works with OpenAI SDK,
  LangChain, and any OpenAI-compatible tool
- **`POST /v1/chat/completions`**: Full OpenAI-compatible chat completions
  endpoint
  - Supports `model` field: `auto` (intelligent routing),
    `fast`/`balanced`/`deep` (tier routing), or specific endpoint names
  - Full SSE streaming support with `stream: true`
  - All sampling parameters: `temperature`, `max_tokens`, `top_p`,
    `presence_penalty`, `frequency_penalty`
  - Request validation with clear error messages
- **`GET /v1/models`**: List available models in OpenAI format
  - Shows routing tiers (`auto`, `fast`, `balanced`, `deep`) with `owned_by: "octoroute"`
  - Shows configured endpoints with `owned_by: "user"`
- **Streaming support**: Real-time token streaming via Server-Sent Events (SSE)
  - Proper chunk format with `delta` objects
  - `[DONE]` termination signal
  - Mid-stream error handling with error chunks
- **Shared query execution**: Refactored retry logic shared between legacy
  and OpenAI handlers

#### Testing

- 17 new integration tests for OpenAI endpoints (`tests/openai_*.rs`)
- Comprehensive validation boundary tests
- SSE streaming tests with chunk parsing

### Changed

- Legacy `/chat` endpoint now uses shared query execution logic
- Reduced code duplication (~450 lines → ~60 lines in chat.rs)
- Test count increased to 348+ tests across 51 integration test files

### Technical Details

- New `handlers/openai/` module with `types.rs`, `completions.rs`, `models.rs`,
  `streaming.rs`, `extractor.rs`
- New `shared/query.rs` for retry logic reuse
- Custom serde deserializers with validation for request types

---

## [0.1.1] - 2025-11-25

### Changed

- **TLS Backend**: Switched from native-tls (OpenSSL) to rustls for easier
  cross-compilation
- **CI/CD**: Added release workflow with pre-built binaries for Linux
  (x86_64, aarch64) and macOS (x86_64, aarch64)

### Fixed

- Cross-compilation for Linux ARM64 now works without manual OpenSSL configuration

---

## [0.1.0] - 2025-11-24

### Added

#### Core Features

- **Intelligent Multi-Model Routing**: Automatically route requests to the
  optimal model tier (Fast/8B, Balanced/30B, Deep/120B) based on task
  characteristics
- **Three Routing Strategies**:
  - `rule`: Fast pattern-based routing (<1ms latency)
  - `llm`: LLM-powered intelligent routing (~250ms latency)
  - `hybrid`: Rule-based with LLM fallback (recommended)
- **Multi-Endpoint Support**: Configure multiple endpoints per tier for load
  balancing and high availability
- **Priority-Based Selection**: Try higher-priority endpoints first, with
  weighted random selection within the same priority
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
  - `octoroute_health_tracking_failures_total{endpoint, error_type}`:
    Health tracking failures
  - `octoroute_metrics_recording_failures_total{operation}`:
    Metrics recording failures
  - `octoroute_background_health_task_failures_total`: Background task restarts
- **Structured Logging**: Human-readable logs via `tracing` with configurable
  log levels
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

[Unreleased]: https://github.com/slb350/octoroute/compare/v2.2.2...HEAD
[2.2.2]: https://github.com/slb350/octoroute/compare/v2.2.1...v2.2.2
[2.2.1]: https://github.com/slb350/octoroute/compare/v2.2.0...v2.2.1
[2.2.0]: https://github.com/slb350/octoroute/releases/tag/v2.2.0
[2.1.2]: https://github.com/slb350/octoroute/releases/tag/v2.1.2
[2.1.1]: https://github.com/slb350/octoroute/releases/tag/v2.1.1
[2.1.0]: https://github.com/slb350/octoroute/releases/tag/v2.1.0
[2.0.1]: https://github.com/slb350/octoroute/releases/tag/v2.0.1
[2.0.0]: https://github.com/slb350/octoroute/releases/tag/v2.0.0
[1.0.0]: https://github.com/slb350/octoroute/releases/tag/v1.0.0
[0.1.1]: https://github.com/slb350/octoroute/releases/tag/v0.1.1
[0.1.0]: https://github.com/slb350/octoroute/releases/tag/v0.1.0
