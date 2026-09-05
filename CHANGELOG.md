# Changelog

<!-- markdownlint-disable MD024 -->

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Stop running `cargo-mutants` for every ordinary CI revision. Added, modified, deleted, and
  renamed inline tests now run the owning source files' mutants; integration tests, fixtures,
  snapshots, and ambiguous mappings fall back to the full sweep. Production-only revisions
  skip mutation, while manual dispatch and the fifth-day monthly schedule always sweep the tree.
  Failed runs retain a bounded repair report for the following day's autonomous repair PR.

### Fixed

- Correct the README fallback contract: `unauthenticated` is a seventh, opt-in fallback trigger, and a missing or rejected upstream credential falls forward when a route configures it instead of always returning to the client.
- Fill README gaps against the runtime: the `X-Octoroute-Reason` and `X-Octoroute-Upstream` response headers, `OPENAI_API_KEY` in the quick-start env sample, the inbound per-minute rate limit, the pool/routing-duration metrics coverage, the optional `first_byte_timeout_ms` knob, and the full seven-command CI gate. Align AGENTS.md with `config.laptop.toml`'s loopback member address.

## [3.0.1] - 2026-08-29

### Fixed

- Stop shipping `rust-toolchain.toml` in the published crate. rustup honours that file in an extracted crate, so building or `cargo install`ing 3.0.0 could pull the consumer onto the 1.97.1 toolchain and trigger a download they did not ask for.

### Changed

- Exclude dev-only files from the crate: internal review notes, the mutation-sweep scripts, git hooks, CI workflows, design plans, and the local lint and build config. 121 files down to 104. No code change.

## [3.0.0] - 2026-08-29

### Added

- Build the executable v3 runtime with authenticated OpenAI-compatible ingress,
  virtual-model discovery, local-pool admission, and held-first-byte streaming.
- Add a lazy HTTP provider registry with isolated credentials and concurrency,
  bounded timeouts, schema-preserving OpenAI-compatible dispatch, explicit
  OpenRouter Auto shaping, ordered pre-commit fallback, provider readiness, and
  bounded response headers. Unsupported request features and missing
  credentials fail closed without prompt disclosure.
- Add explicit Anthropic Messages translation for system/developer and chat
  messages, function tools and history, reasoning, sampling, errors,
  non-streaming responses, usage, and fragmented SSE.
- Add a locked-down Codex CLI provider with ChatGPT-auth diagnostics, a filtered
  child environment, ephemeral read-only execution, disabled integrations,
  bounded JSONL lifecycle parsing, and OpenAI response translation.
- Add cached, coalesced, bounded provider authentication/reachability probes;
  fixed-label admission, response, fallback, and probe counters; representative
  OpenCode-style provider integration tests; and a deployment canary script.
- Bound virtual and physical model identifiers, credential-command argv,
  upstream deadlines, and member/provider concurrency; reserve `auto` for the
  default-route alias; reject duplicate route targets; and require successful
  Codex diagnostics before readiness.
- Apply local-pool reasoning defaults when callers omit reasoning controls and
  reject multi-choice requests before Codex prompt disclosure.
- Reject invalid local-member bearer credentials before the listener binds.
- Add `first_byte_timeout_ms` to local pools and providers, bounding how long a
  hung upstream holds its member and inbound permits before the route falls
  forward. Unset by default; configure it only from measured behaviour.
- Add `token_count_timeout_ms` to local pools so tokenizing a large prompt no
  longer shares the two-second health-probe deadline.
- Add `octoroute_fabric_pool_admissions_total{pool,state}`,
  `octoroute_fabric_pool_fallbacks_total{pool,trigger}`,
  `octoroute_fabric_routing_duration_seconds`, and
  `octoroute_fabric_unknown_upstream_types_total{adapter}`. Local spillover to
  the next route step now emits a tracing warning.
- Add `ProviderAdmissionState::Unauthenticated` and the matching
  `unauthenticated` fallback trigger, outside the default trigger set, so an
  expired credential surfaces instead of silently rerouting traffic and spend.
- Add `PoolAdmissionState::TokenCountUnavailable`, distinguishing a member whose
  token-count endpoint has gone missing from an unreachable one.
- Report `degraded` from `/health` when some configured target is unavailable
  while others still serve.
- Wire `cargo-mutants` into the tracked pre-commit hook, the justfile, and CI,
  replacing a benchmark-compilation job that compiled nothing. The documentation
  build with `RUSTDOCFLAGS: -D warnings` remains a separate CI job.

### Changed

- Make version 3 the only executable/configuration contract: `config.toml`, the
  CLI generator, laptop profile, startup path, crate metadata, public
  integration tests, and operator documentation now use the inference fabric.
- Move reusable environment, capability, privacy, HTTP-limit, error, and
  held-first-byte streaming primitives under neutral or fabric ownership.
- Make route order the sole provider preference contract and remove the inert
  provider-priority field.
- Give the hardened systemd service an explicit private `CODEX_HOME` state
  directory so ChatGPT-managed Codex readiness works with `ProtectHome=true`.
- Align the workstation profile with the shipped loopback llama.cpp service.
- **Breaking:** client `model` values are route names, not provider slugs. v2
  accepted `provider/model` passthrough such as `openrouter/auto`; v3 resolves
  the provider from the route, so identifiers are restricted to letters, digits,
  dots, underscores, and hyphens and a slug returns 400. Point clients at a
  configured route (`auto`, `auto-route`, `cloud-sota`) instead.
- **Breaking:** `/health/ready` and `/health` return the per-pool and
  per-provider breakdown only to an authenticated caller. Anonymous callers get
  the status code and aggregate. Readiness is also cached briefly, so an
  anonymous caller cannot drive `codex doctor` spawns and credentialed `/models`
  probes at request rate.
- Enable thinking on Anthropic providers only when the caller sends a reasoning
  control or the provider config sets `reasoning_effort`, and cap the budget at
  half of `max_tokens` so a real answer allowance remains. Sampling controls are
  dropped when thinking is on, which the Anthropic API requires.
- Inject `reasoning_effort` only into OpenAI-protocol providers explicitly
  configured for it, rather than from the route default.
- Forward the upstream `error.message` and `error.type` from Anthropic error
  responses instead of replacing them with a fixed message.
- Refuse upstream redirects. The provider credential travels in a custom
  `x-api-key` header, which reqwest does not strip across origins, so a 3xx is
  now a pre-commit failure.
- Share one pooled rustls client across local probes, local inference, and every
  provider.
- Require local pool members to be on a loopback, private-range, or `.local`
  address, so a public member cannot satisfy `X-Octoroute-Privacy: local-only`.
- Filter a route declaring `privacy = "local_only"` to local steps whether or
  not the caller sent the header.
- Give `cloud-sota` the `incompatible` trigger so an image, audio, or video
  request falls through Codex to OpenRouter instead of returning 503.
- Print startup failures with `Display` and exit non-zero, and initialize
  telemetry before service construction.
- Raise the `zerovec-derive` manifest floor to 0.11.6, matching what 2.2.2
  states for unlocked builds.
- Child-process execution moved from `codex/mod.rs` into `codex/process.rs`, keeping both files inside the 600-line limit.

### Removed

- Remove the runtime version switch and the superseded single-local/OpenRouter
  service, semantic forecaster, calibration command, session latch, metrics,
  configuration parser, transport, documentation, and tests.

### Fixed

Resolve the PR #11 review: 9 functional and contract findings, 11 test-coverage
gaps, and 45 smaller findings. The suite grew from 126 tests to 212.

- Honour a caller's `n_predict` as the local output-token budget. llama.cpp's
  `oaicompat_chat_params_parse` gives `n_predict` precedence over `max_tokens`
  on the endpoint Octoroute proxies, so a request carrying it was budgeted at
  the pool default while the member attempted the larger value. The
  input-plus-output-plus-reserve guarantee did not hold.
- Treat a request-caused 4xx from `/v1/chat/completions/input_tokens` as a
  request rejection instead of member incapability. The token-count endpoint
  applies the chat template, so a template rejection is deterministic across
  members, yet it was retried against each one, reported as an unhealthy pool,
  and spilled to a paid provider. A 404 or 501 still reports the endpoint
  missing.
- Record `octoroute_fabric_pool_fallbacks_total` when an admitted local member
  fails before commitment. Only admission-time rejections were counted, so a
  member that accepted work and then failed was invisible in the one metric
  that reports local capacity spilling to cloud.
- Fall forward on a dispatch-time provider 401 or 403 when the route opts into
  `unauthenticated`, which was previously honoured at credential resolution but
  not at credential rejection, and stop returning an upstream 401 in a form a
  client reads as its own credential failing.
- Map a Codex CLI authenticated with an API key rather than a ChatGPT
  subscription to `unauthenticated` instead of `unhealthy`, so the
  misconfiguration surfaces rather than silently redirecting traffic and spend
  through the default fallback set.
- Apply the Anthropic adapter's fail-closed rule to nested objects. Unknown keys
  inside messages, content blocks, tool and function objects, and `reasoning`
  were dropped, so `reasoning: {"enabled": true}` produced a non-thinking
  request and `function.strict` was discarded. `reasoning.enabled` and
  `reasoning.max_tokens` now map onto the thinking budget.
- Reject a local request whose output reservation cannot fit the context window
  before probing any member, so a verdict the configuration already determined
  no longer discloses the prompt.
- Bound the local probe body reads, the credential-command total wait, and
  environment variable name length, all of which were unbounded against
  upstream-controlled or operator-controlled input.
- Skip an `input_json_delta` whose `content_block_start` was skipped, rather
  than failing the stream. Deltas arrive after the response has committed, so a
  future Anthropic block type carrying partial tool arguments turned a complete
  generation into a truncated one for a client already reading it.
- Correct a range of error classifications: a local upstream 429 now uses the
  `rate_limited` trigger, a terminal route error reports the rejection that
  governed the route rather than the last step's state, `no_eligible_target`
  carries an error type consistent with its 503, an incomplete request body is
  no longer reported as too large, and gateway-side translation failures are no
  longer labelled client errors.
- CI mutation shards are now shifted to cargo-mutants' zero-based `k/n` indexing: the one-based matrix passed `8/8` straight through, failing that shard as an argument error and never running shard `0/8`.
- Executable test fixtures are written from a child process, eliminating the Linux-only `ETXTBSY` spawn flake that failed mutation baselines and could record an uncaught mutant as caught.
- `ProcessGroup` is one cross-platform type with cfg'd blocks instead of a `cfg(not(unix))` module the sweep could never compile, and a test now pins that dropping the guard kills the whole process group. A zero nested `reasoning.max_tokens` is pinned to the nested parser's own rejection label rather than the downstream affordability check's.
- A group signal that fails with `EPERM` is treated as an already-terminated group only once our own leader has exited. Darwin reports `EPERM` rather than `ESRCH` for a recycled pgid, so Codex cleanup failed on roughly one run in ten and returned `Process` in place of the `OutputTooLarge` or `Timeout` that had triggered it. A leader still running that we may no longer signal stays a failure: `api_key_command` runs an operator's own executable, and one that changes credentials must not report a clean shutdown.
- Codex cleanup failure no longer replaces the error that caused cleanup. The route's fallback policy reads that trigger, so the substitution changed routing decisions for a request that had actually hit a bound.
- `scripts/mutants-run.sh` now reaps processes the sweep spawned and could not kill itself, on entry and on its EXIT trap. The mutants that disable the process-group kill path are the ones that time out, so their fixture outlives the run; two `endless-codex` spinners were found holding 15 CPU-minutes. Trashing scratch directories had no process-side counterpart.
- The three remaining `#[cfg(all(test, unix))]` gates are now bare `#[cfg(test)]`, so cargo-mutants stops reporting five test helpers as production mutants. `tests/source_hygiene.rs` fails the build if either that gate or a `cfg(not(unix))` module reappears.

### Security

- Parse route steps before duplicate-checking them, so raw unvalidated
  configuration text is never interpolated into an error message. A step
  carrying a planted secret or a newline reached stderr.
- Add the missing coverage for the upstream redirect refusal. The check that
  keeps a provider credential from following a 3xx cross-origin had no test, and
  deleting it passed the whole suite.
- Assert that the inbound bearer never reaches an upstream. The existing
  matchers asserted the correct upstream credential was present, not that the
  inbound one was absent, so a regression forwarding both would have passed.
- Stop an upstream claiming `x-request-id` on a committed response, and make
  `insert_header` fail closed rather than panicking on the request path.

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
  measured 760-1500 ms latency cost.

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
  (Superseded in v3: the benchmark job compiled nothing and is replaced by a
  `cargo-mutants` sweep.)
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

[Unreleased]: https://github.com/slb350/octoroute/compare/v3.0.1...HEAD
[3.0.1]: https://github.com/slb350/octoroute/compare/v3.0.0...v3.0.1
[3.0.0]: https://github.com/slb350/octoroute/compare/v2.2.2...v3.0.0
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
