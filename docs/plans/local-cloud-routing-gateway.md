# Local and Cloud Routing Gateway Implementation Plan

> **Superseded in part on 2026-07-25 and corrected on 2026-08-01.** This v2.0
> planning record introduced the compatibility-and-capacity-only `auto`
> regression, but its classifier evidence gate was sound. The active product
> contract is [intelligent-auto-routing.md](intelligent-auto-routing.md):
> semantic routing is now disabled, shadowed, or explicitly enforced, with
> shadow as the safe default.

**Status:** Approved direction; implementation not started

**Branch:** `codex/feat/local-cloud-routing-gateway`

**Decision date:** 2026-07-22

**Target release:** Octoroute 2.0.0

## Executive decision

Octoroute will become the single OpenAI-compatible gateway for the user's
personal AI traffic:

```text
Client
  |
  v
Octoroute
  |-- local --> Strix llama.cpp (`strixtea`)
  |
  `-- cloud --> OpenRouter (`openrouter/auto-beta`)
                     |
                     `--> Anthropic, OpenAI, Google, DeepSeek, and others
```

Octoroute owns the local-versus-cloud decision. OpenRouter owns cloud model
and provider selection. Octoroute will not duplicate OpenRouter's cloud
routing logic.

The v2 gateway will use a direct streaming HTTP transport. It will not turn
chat messages into a single prompt or use `open-agent-sdk` in the proxy data
path. That is required to preserve tools, structured output, multimodal
content, reasoning fields, OpenRouter plugins, unknown forward-compatible
fields, streaming semantics, and the actual selected cloud model.

This is a major-version change. The current fast/balanced/deep tier model,
legacy `/chat` endpoint, and v1 configuration shape will not remain as a
permanent compatibility layer.

## Why this architecture

The current project has valuable gateway machinery: Axum, retries, health
tracking, SSE support, request IDs, metrics, tests, and a small operational
footprint. Its current abstraction is wrong for the intended deployment,
however:

- It chooses among local size tiers instead of local versus cloud.
- It does not configure upstream authentication.
- It flattens OpenAI messages into a string before model invocation.
- It has no representation for tools, multimodal content, structured output,
  OpenRouter plugins, or provider routing controls.
- Its LLM router consumes a model tier merely to choose another model tier.
- Its health checks assume unauthenticated `/models` endpoints.
- Its fixed `/v1` URL rule does not fit OpenRouter's `/api/v1` base path.
- Its English keyword classifier is not a reliable quality boundary.

OpenRouter Auto Beta already classifies cloud requests into task types, ranks
models using current usage evidence, applies cost controls, and supplies
provider fallbacks. Reimplementing that selection inside Octoroute would add
maintenance without improving the local-versus-cloud decision.

## Evidence gathered

### Current Strix runtime

The live Strix host was inspected on 2026-07-22:

- llama.cpp model alias: `strixtea`
- Model file at the latest verification: `Agents-A1-Q8_0.gguf`
- Context window: 65,536 tokens
- Parallel slots: 1
- Slot monitoring: available through `GET /slots`
- Health monitoring: available through `GET /health`
- Metrics endpoint: disabled because llama.cpp is not started with
  `--metrics`
- Server bind: `0.0.0.0:8080`
- Process manager: none; the current server is an abandoned SSH session
  attached directly to systemd PID 1
- Port 3000 is occupied by Gitea
- Port 8081 is free and selected for Octoroute
- Octoroute is not currently running on Strix

The single slot makes admission control a first-class routing input. A request
that is valid for the local model should still spill to cloud immediately
when the slot is occupied.

The earlier benchmark ledger covered `puzzle-75b`; Strix changed to
`Agents-A1-Q8_0.gguf` under alias `strixtea` before implementation
verification. Until the new model has equivalent capability evidence, the
initial policy remains conservative for tools and complex structured
automation.

### Relevant upstream behavior

OpenRouter Auto Beta:

- Uses the model slug `openrouter/auto-beta`.
- Classifies requests into roughly 30 task types.
- Uses current community spend share to rank models for a task.
- Supports `allowed_models` and `cost_quality_tradeoff`.
- Defaults the Auto Beta cost-quality setting to 9.
- Supports streaming and standard OpenRouter features.
- Returns the actual selected model in the response.
- Charges no extra Auto Router fee beyond the selected model's price.

llama.cpp:

- Exposes `GET /slots?fail_on_no_slot=1`, which returns 503 when no slot is
  available.
- The current server API documents
  `POST /v1/chat/completions/input_tokens` for exact request token counting.
  Its live Strix response schema must be pinned in a contract fixture before
  it becomes an admission dependency.
- Supports streaming chat completions, structured output, tool-call parsing,
  and additional llama.cpp-specific fields.

## Goals

1. Provide one OpenAI-compatible URL and one client credential.
2. Route suitable requests to Strix without sending them to a cloud
   classifier first.
3. Route everything else through OpenRouter Auto Beta.
4. Preserve the request and upstream response schemas with minimal,
   documented mutation.
5. Spill from Strix to cloud before response commitment when Strix is busy,
   unhealthy, incompatible, or fails early.
6. Never violate an explicit local-only privacy request.
7. Keep cloud cost policy explicit and observable.
8. Preserve constant-memory streaming and backpressure.
9. Make routing decisions explainable through bounded reason codes.
10. Finish with no Rust source or test file above the repository's 800-line
    hard limit, and split modified files before they exceed 600 lines.

## Non-goals for v2.0

- Direct adapters for Anthropic, Google, OpenAI, or DeepSeek APIs.
- Letting OpenRouter route directly to the private Strix model.
- The OpenAI Responses API, Anthropic Messages API, embeddings, image
  generation, or audio endpoints.
- Prompt rewriting, retrieval augmentation, agent execution, or tool
  execution inside Octoroute.
- A web UI.
- Persisted billing or conversation history.
- High availability across a complete Strix host failure.
- A cloud semantic-classifier call on every request.

The first release remains focused on
`POST /v1/chat/completions`, `GET /v1/models`, health endpoints, and metrics.
The transport and policy interfaces should permit additional protocols later
without weakening the v2 contract.

## Compatibility and release contract

This work should ship as Octoroute 2.0.0.

### Preserved client behavior

- `POST /v1/chat/completions`
- Streaming and non-streaming chat completions
- OpenAI-shaped success and error responses
- `model: "auto"` as the default intelligent-routing entry point
- `GET /v1/models`
- Request ID propagation
- `/metrics`

### Intentional breaking changes

- Configuration requires `config_version = 2`.
- Fast, balanced, and deep tiers are removed.
- `model: "fast"`, `"balanced"`, and `"deep"` are removed.
- `model: "auto"` changes from choosing a local size tier to choosing the
  local or cloud destination.
- The legacy `POST /chat` route is removed.
- The v1 `models` and `routing.strategy` configuration is rejected.
- The public Rust configuration and routing types change.
- Authentication is required when cloud routing is enabled or the server
  listens on a non-loopback address.

Octoroute should fail startup with an actionable v1-to-v2 migration message.
It must not silently guess which old tier represents Strix.

## Request routing contract

### Virtual and explicit model names

| Client `model` value | Meaning | Fallback |
| --- | --- | --- |
| `auto` | Octoroute chooses local or cloud | Local may spill to cloud |
| `local` | Force the configured local model | Never cloud |
| `cloud` | Force OpenRouter Auto Beta | Cloud provider fallbacks only |
| `strixtea` | Force the exact configured local alias | Never cloud |
| `openrouter/auto-beta` | Force cloud Auto Beta | Provider fallbacks |
| `provider/model` | Force an OpenRouter model | Provider fallbacks |
| unknown unqualified name | Invalid request | Return 400 |

An explicit local route is also a privacy boundary. If local is unavailable
or incompatible, Octoroute returns a clear 503/422 response instead of
silently transmitting the request to OpenRouter.

`X-Octoroute-Privacy: local-only` is the explicit privacy directive. It has
the same no-cloud guarantee as `model: "local"`. A cloud model combined with
this header is contradictory and returns 400; Octoroute must not guess which
instruction wins.

### Ordered automatic policy

The `auto` policy evaluates these gates in order:

1. Validate the request envelope and body-size limit.
2. Apply an authenticated explicit route or local-only privacy directive.
3. Detect capabilities requested by the body.
4. Reject local when required capabilities are not enabled for Strix.
5. Reject local when the requested output and exact input-token count cannot
   fit inside the configured context safety limit.
6. Acquire Octoroute's non-blocking local concurrency permit.
7. Confirm llama.cpp readiness and free-slot status.
8. Apply the benchmark-backed workload eligibility policy.
9. Route eligible work to Strix.
10. Route all other work to OpenRouter Auto Beta.

The policy returns a typed decision:

```text
destination: local | cloud
reason:
  explicit_local
  explicit_cloud
  local_only
  local_capable
  local_incompatible
  local_context_limit
  local_busy
  local_unhealthy
  local_early_failure
  cloud_default
  classifier_cloud
```

Reason values are an enum, not prompt-derived strings, so metrics labels stay
bounded.

### Initial workload eligibility

The first enforceable policy should be simple and auditable:

- Prefer local for plain-text chat that fits the local context, requests no
  unsupported feature, and finds Strix ready and idle.
- Route tool calls, server-side plugins, multimodal input, unsupported
  modalities, and other explicitly unsupported features to cloud.
- Route unknown or malformed feature combinations to cloud only if the
  request is otherwise valid for OpenRouter.
- Permit the client to force cloud for quality-critical work.
- Keep the capability set in configuration rather than inferring support
  from the model name.

The initial Strix configuration should conservatively enable text chat,
streaming, and only capabilities verified against the running build and
model. Tool workloads should start cloud-bound because the current local
tooling score is materially weaker than its general chat scores.

### Optional semantic classifier

A fixed, cheap cloud classifier such as
`deepseek/deepseek-v4-flash` can improve the ambiguous plain-text boundary,
but it should not be part of the v2.0 correctness path.

It should be implemented only after the deterministic gateway is stable:

1. Add a `SemanticClassifier` trait behind explicit configuration.
2. Evaluate it against a labeled corpus of representative personal requests.
3. Run it in shadow mode and compare its decisions with human judgments and
   local-versus-cloud output quality.
4. Require high precision for local decisions; uncertainty routes cloud.
5. Skip it for explicit local-only requests.
6. Fail closed to OpenRouter Auto Beta if classification times out or returns
   invalid output.
7. Make the prompt disclosure and added latency visible in documentation.

Using a cloud classifier before a local response means the prompt is no
longer private or offline, even if Strix ultimately generates the answer.
That trade-off must be opt-in.

## Request and response mutation rules

Octoroute should parse the body into:

- The original bounded byte buffer.
- A minimally validated JSON object.
- A routing-facts view containing model, messages, stream mode, requested
  capabilities, output budget, and optional route metadata.

It should not deserialize and re-create the complete OpenAI schema.

### Local request mutation

- Replace a virtual model (`auto` or `local`) with the configured llama.cpp
  alias.
- Remove Octoroute-only metadata before forwarding.
- Preserve all other recognized and unknown fields.

### Cloud request mutation

- Replace `auto` or `cloud` with `openrouter/auto-beta`.
- Preserve explicit `provider/model` slugs.
- Upsert the server-owned `auto-router` plugin settings without deleting
  unrelated client plugins.
- Make server cost and allowed-model policy authoritative when it conflicts
  with client input.
- Preserve `session_id` so OpenRouter can provide model/provider stickiness.
- Request OpenRouter routing metadata only when configured.

### Response behavior

- Pass status, content type, body, SSE comments, SSE data, `[DONE]`, usage,
  error objects, and the actual upstream model through unchanged.
- After the first buffered chunk establishes commitment, proxy SSE body bytes
  opaquely. Do not reduce the stream to `data:` lines or decode and re-encode
  frames; that would lose comments and forward-compatible fields.
- Strip hop-by-hop and unsafe upstream headers.
- Preserve safe diagnostic headers such as `X-Generation-Id`.
- Add bounded gateway headers:
  - `X-Octoroute-Destination`
  - `X-Octoroute-Reason`
  - `X-Octoroute-Upstream`
  - `X-Request-Id`
- Never rewrite OpenRouter's selected `model` back to
  `openrouter/auto-beta`.
- Test the actual selected model in both a non-streaming response and every
  streaming chunk shape that contains a model field.

## Failure and retry semantics

The response commit point is the first response body byte sent to the client.

- Before commitment, an automatic local request may spill to cloud on a
  connection failure, free-slot 503, retryable local 5xx, or configured
  first-byte timeout.
- An explicit local or local-only request never spills to cloud.
- After commitment, Octoroute never switches upstream.
- Mid-stream errors are forwarded and recorded against the upstream request.
- Cloud retries and provider fallbacks remain OpenRouter's responsibility.
- Octoroute does not retry an OpenRouter request against another cloud model.
- The buffered request body is replayable, but bounded.
- Dropping a timed-out local response must cancel the upstream request.

Cancellation should be explicit: use a request-scoped cancellation token,
`tokio::select!`, and a drop guard for the local body stream. Aborting or
dropping the `reqwest` response future/stream must be verified against Strix
rather than assumed.

First-byte and total timeouts must be derived from measured Strix prompt
processing and generation latency. The implementation must not invent a
timeout that penalizes long prompts without a benchmark.

## Configuration design

Illustrative v2 configuration:

```toml
config_version = 2

[server]
host = "0.0.0.0"
port = 8081
api_key_env = "OCTOROUTE_API_KEY"
max_request_bytes = 8388608

[upstreams.local]
kind = "llama_cpp"
name = "strix"
base_url = "http://127.0.0.1:8080"
model = "strixtea"
context_window = 65536
context_safety_tokens = 1024
max_in_flight = 1
capabilities = ["chat", "stream"]
health_path = "/health"
slots_path = "/slots?fail_on_no_slot=1"
input_tokens_path = "/v1/chat/completions/input_tokens"

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
auto_model = "openrouter/auto-beta"
cost_quality_tradeoff = 9
app_title = "Octoroute"

[routing]
default = "prefer_local"
fallback_before_commit = true
```

`allowed_models` is optional. An empty value allows the current Auto Beta
pool. A personal deployment can restrict it to selected providers or model
families without changing Octoroute code.

The capability field is a closed enum. Initial valid values are `chat`,
`stream`, `tools`, `structured_output`, `image_input`, `audio_input`,
`video_input`, and `reasoning`. Unknown values fail startup. The initial
Strix example should use `["chat", "stream"]`; additional values are enabled
only after a live contract test.

`app_title` is Octoroute's configuration name for the value emitted in
OpenRouter's `X-OpenRouter-Title` header.

All secrets are read from named environment variables. Raw secret values are
invalid in TOML and must never appear in debug output.

## Security model

Cloud routing turns an unauthenticated LAN endpoint into a spend-capable
gateway. The v2 release must include the following controls:

- Bearer authentication at Octoroute.
- Constant-time credential comparison.
- Startup validation for required inbound and OpenRouter secrets.
- Authentication required by default for non-loopback binds and all
  cloud-enabled configurations.
- Per-credential request rate and concurrency limits.
- A global cloud concurrency ceiling.
- A bounded request body and bounded header sizes.
- A strict outbound header allowlist.
- No forwarding of inbound `Authorization`, cookies, proxy headers, or
  connection-specific headers.
- Octoroute supplies the correct credential independently to each upstream.
- HTTPS required for OpenRouter.
- Plain HTTP permitted only for explicitly configured local/private
  upstreams.
- Prompt, tool arguments, API keys, user IDs, and session IDs excluded from
  normal logs and metrics.
- CORS disabled unless an explicit origin allowlist is configured.
- Liveness separated from dependency readiness.
- Dependency audit and secret-scanning checks in CI.

The Strix deployment should make Octoroute the only normal ingress:

- Bind llama.cpp to `127.0.0.1:8080`.
- Bind Octoroute to the LAN interface.
- Terminate local TLS at Octoroute or a trusted reverse proxy.
- Keep llama.cpp's built-in filesystem/shell tools disabled.

Startup validation resolves secrets before the listener is bound. A
non-loopback or unspecified bind such as `0.0.0.0` is invalid until inbound
authentication is fully configured and validated.

## Proposed module boundaries

The implementation should introduce small, single-purpose modules:

```text
src/
├── auth/
│   ├── mod.rs
│   ├── bearer.rs
│   └── limits.rs
├── config/
│   ├── mod.rs
│   ├── routing.rs
│   ├── server.rs
│   ├── upstream.rs
│   └── validation.rs
├── gateway/
│   ├── mod.rs
│   ├── headers.rs
│   ├── request.rs
│   ├── response.rs
│   ├── stream.rs
│   └── transport.rs
├── routing/
│   ├── mod.rs
│   ├── decision.rs
│   ├── facts.rs
│   ├── policy.rs
│   └── resolver.rs
├── upstream/
│   ├── mod.rs
│   ├── llama_cpp/
│   │   ├── admission.rs
│   │   ├── client.rs
│   │   ├── mod.rs
│   │   └── probes.rs
│   └── openrouter/
│       ├── client.rs
│       ├── mod.rs
│       └── request.rs
└── handlers/
    ├── health.rs
    ├── metrics.rs
    └── openai/
        ├── chat.rs
        ├── extractor.rs
        ├── mod.rs
        └── models.rs
```

Shared behavior should be expressed through narrow traits:

- `UpstreamTransport`
- `LocalAdmission`
- `RoutePolicy`
- `TokenCounter`
- `SemanticClassifier` in the later classifier slice

The traits exist to make failure and streaming behavior testable, not to
create a generic provider framework.

## Phased implementation

Every phase follows RED, GREEN, REFACTOR, and COMMIT. Tests that prove the
new behavior must fail before production code is added.

### Phase 0: Baseline and characterization

1. Record the installed toolchain and establish Rust 1.90 verification.
2. Run the complete current suite, Clippy, formatting, docs, audit, and
   benchmarks.
3. Add characterization tests for request IDs, OpenAI error envelopes, SSE
   framing, and metrics that remain part of v2.
4. Record Strix latency, first-byte time, token-count latency, slot behavior,
   cancellation behavior, and current feature support.
5. Record a small OpenRouter canary for streaming, actual selected model,
   usage/cost, Auto Beta plugin settings, and an early error.
6. Build synthetic schema fixtures from the results. Store any raw local
   capture outside the repository, verify its path with `git check-ignore`,
   and never commit credentials or personal prompts.

### Phase 1: Introduce v2 configuration

RED:

- Reject missing or unknown config versions.
- Reject raw secrets.
- Reject missing environment variables.
- Reject HTTP OpenRouter URLs.
- Reject ambiguous v1/v2 mixed configuration.
- Require inbound auth for cloud or non-loopback service.
- Validate local context and concurrency limits.
- Reject unknown capability names.
- Reject contradictory local-only and cloud route intent.

GREEN:

- Add the split `config` module and immutable validated types.
- Generate a v2 template through `octoroute config`.
- Produce an actionable v1 migration error.
- Add secret-redacted `Debug` implementations.

REFACTOR:

- Split the current 1,591-line `src/config.rs`.
- Keep each resulting source and test file below 600 lines.

### Phase 2: Build the transparent gateway transport

RED:

- Preserve unknown JSON fields.
- Patch only documented routing fields.
- Strip forbidden headers.
- Apply separate upstream credentials.
- Proxy non-streaming responses unchanged.
- Stream large SSE responses with bounded memory and backpressure.
- Forward OpenRouter SSE comments.
- Cancel upstream work on client disconnect.

GREEN:

- Implement a shared `reqwest::Client` with pooled connections and rustls.
- Join validated base URLs and paths with `reqwest::Url`, never string
  concatenation.
- Implement bounded request extraction and minimal routing-fact parsing.
- Implement raw status/header/body forwarding.
- Implement the response commit state machine.

REFACTOR:

- Remove full-response reconstruction from the v2 path.
- Keep transport independent of route policy.

### Phase 3: Add the llama.cpp adapter and admission control

RED:

- Parse healthy, loading, and malformed health responses.
- Parse idle and busy slot responses.
- Treat disabled slot monitoring as configuration failure for auto routing.
- Enforce the local semaphore under concurrency.
- Count exact input tokens.
- Reject local context overflow with the output budget included.
- Release permits on success, error, cancellation, and panic.

GREEN:

- Implement cached health state with a short, configurable TTL.
- Use a non-blocking semaphore for configured local capacity.
- Confirm `GET /slots?fail_on_no_slot=1` before local dispatch.
- Use llama.cpp's chat input-token endpoint for final context admission.

REFACTOR:

- Replace the oversized generic health module with focused local probes and
  gateway readiness aggregation.

### Phase 4: Add deterministic route policy

RED:

- Cover the complete decision table.
- Prove local-only can never produce a cloud destination.
- Prove explicit cloud never probes or calls Strix.
- Prove unsupported features route cloud.
- Prove busy and unhealthy local states route cloud for `auto`.
- Prove the same states return an error for explicit local.
- Property-test that every valid request produces one typed decision.
- Property-test that reason labels are drawn from the bounded enum.

GREEN:

- Implement model resolution, capability facts, and ordered policy gates.
- Add virtual model discovery for `auto`, `local`, and `cloud`.
- Add exact local-alias and explicit OpenRouter-slug routing.

REFACTOR:

- Remove the fast/balanced/deep router, selector, and English keyword
  classifier from the v2 path.

### Phase 5: Add the OpenRouter adapter

RED:

- Set Bearer authentication without leaking the inbound key.
- Rewrite only virtual cloud models to `openrouter/auto-beta`.
- Preserve explicit OpenRouter model slugs.
- Merge Auto Router plugin policy deterministically.
- Preserve unrelated plugins and `session_id`.
- Preserve selected model, usage, cost, and generation ID.
- Preserve selected model in non-streaming and streaming response fixtures.
- Handle normalized OpenRouter errors and streaming error chunks.

GREEN:

- Implement the OpenRouter request policy and adapter.
- Make cost-quality and allowed-model settings explicit configuration.
- Pass provider fallback responsibility through to OpenRouter.

REFACTOR:

- Delete the remaining `open-agent-sdk` invocation path and dependency when
  no production path uses it.

### Phase 6: Implement fallback and streaming commitment

RED:

- Fall back from local connect failure before commitment.
- Fall back from local 503 before commitment.
- Fall back from a measured first-byte timeout.
- Never fall back after the first client body byte.
- Never fall back for explicit local or local-only.
- Forward mid-stream failures exactly once.
- Abort local work when cloud fallback begins.

GREEN:

- Implement the explicit uncommitted/committed stream state machine.
- Record the destination actually used, not merely the first choice.
- Preserve backpressure and cancellation in both paths.

REFACTOR:

- Remove duplicate legacy retry and SSE implementations.

### Phase 7: Authentication, limits, and readiness

RED:

- Reject missing, malformed, and incorrect bearer credentials.
- Rate-limit by authenticated credential.
- Enforce cloud and global concurrency ceilings.
- Reject oversized bodies before parsing.
- Verify no secret appears in errors, traces, metrics, or `Debug`.
- Verify liveness succeeds independently of upstream state.
- Verify readiness succeeds when at least one configured destination can
  serve and reports degraded dependencies separately.

GREEN:

- Add auth and limit middleware.
- Add `/health/live`, `/health/ready`, and a documented `/health` alias.
- Keep CORS disabled. Browser applications must use a same-origin reverse
  proxy rather than widening the inference API's origin boundary.

REFACTOR:

- Centralize security-sensitive header and redaction rules.

### Phase 8: Observability and policy evaluation

Add bounded metrics:

- `octoroute_route_decisions_total{destination,reason}`
- `octoroute_upstream_requests_total{upstream,outcome,status_class}`
- `octoroute_local_busy_spillovers_total`
- `octoroute_request_duration_seconds{destination}`
- `octoroute_time_to_first_byte_seconds{destination}`
- `octoroute_routing_duration_seconds`
- `octoroute_in_flight_requests{destination}`

Preserve the exact selected cloud model and usage/cost fields in the opaque
response. Do not parse or reconstruct streaming response bodies solely for a
cost counter or completion log; OpenRouter generation accounting is the
authoritative spend source. Never use arbitrary client or selected model
strings as Prometheus labels.

Before enabling the optional future semantic classifier, create a labeled
evaluation corpus from representative workloads and compare Strix with
OpenRouter. This corpus is not required while deterministic capability and
admission policy is the only active routing policy. Track:

- local acceptance quality
- false-local rate
- cloud spend avoided
- routing overhead
- busy spillover frequency
- local and cloud time to first byte
- cancellation success

### Phase 9: Remove legacy code and close file-size debt

1. Remove the legacy `/chat` handler.
2. Remove tier routing, selection, and obsolete configuration.
3. Remove dead dependencies.
4. Split all remaining source and test files above 600 lines.
5. Confirm no code or test file exceeds 800 lines.
6. Run repository-wide linting and fix every reported issue.
7. Update all architecture, API, configuration, deployment, observability,
   and README documentation.
8. Add a v1-to-v2 migration guide and changelog entry.

### Phase 10: Deployment and release

1. Build and install Octoroute as a systemd service on Strix.
2. Replace the abandoned-session llama.cpp process with a durable service.
3. Move llama.cpp to loopback-only ingress.
4. Enable llama.cpp `--metrics` and scrape it separately from Octoroute.
5. Bind Octoroute to port 8081; port 3000 belongs to Gitea.
6. Point clients at Octoroute, not port 8080.
7. Start in observation mode with explicit local/cloud requests.
8. Enable `auto` for a controlled client subset.
9. Validate metrics, logs, costs, and fallback behavior.
10. Run fault drills for a busy slot, stopped llama.cpp, unavailable
    OpenRouter, client disconnect, invalid credential, and context overflow.
11. Complete the security hardening checklist.
12. Verify stable and the `1.90` toolchain channel, which CI pins as
    `1.90.0`.
13. Publish 2.0.0 only after the migration and rollback paths are tested.

## Test layout

Keep test files focused and below 600 lines:

```text
tests/
├── auth/
├── config_v2/
├── gateway_headers/
├── gateway_non_streaming/
├── gateway_streaming/
├── llama_admission/
├── openrouter/
├── routing_policy/
└── support/
    ├── fixtures.rs
    └── mock_upstreams.rs
```

Default tests use local wiremock servers and make no live paid calls. Live
Strix and OpenRouter contract tests are opt-in, credential-gated, and capped
to a documented cost.

## Verification gate

Before each implementation commit that changes Rust or shell code:

1. Run the relevant failing test and record RED.
2. Implement the smallest complete behavior.
3. Run focused tests.
4. Run formatting and Clippy.
5. Run `/simplify` on staged code.
6. Check every path with `git check-ignore` before staging.

Before the branch is ready:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --no-default-features
cargo doc --all-features --no-deps
cargo audit
cargo bench --no-run
```

Run the full suite on stable and Rust 1.90.0. Build release artifacts for all
supported targets. Verify the generated package does not contain secrets,
local benchmark prompts, or personal configuration.

## Acceptance criteria

The feature is complete only when:

- One OpenAI client can use `auto`, `local`, `cloud`, a local alias, and an
  explicit OpenRouter model through the same base URL.
- A compatible idle request reaches Strix with no cloud classifier call.
- A busy Strix slot causes an `auto` request to reach OpenRouter.
- An explicit local-only request never reaches OpenRouter under any tested
  failure.
- Tools and unsupported capabilities route cloud without body loss.
- OpenRouter Auto Beta receives the configured cost/model policy.
- The actual selected cloud model and usage/cost survive the proxy.
- Streaming remains incremental and bounded in memory.
- Fallback occurs only before response commitment.
- Client disconnect cancels the upstream request.
- No inbound credential reaches an upstream.
- No upstream credential reaches a client, log, trace, metric, or other
  upstream.
- Health, readiness, metrics, and route-reason observability are usable.
- All tests, lints, docs, audits, and MSRV checks pass.
- No Rust source or test file exceeds 800 lines.
- Deployment, migration, security, and rollback drills are documented and
  executed.

## Rollback

Keep the current v1 binary, configuration, and service unit available during
the initial deployment. Rollback is:

1. Stop the v2 service.
2. Restore the v1 unit and v1 configuration.
3. Restore direct client access to the previous endpoint if needed.
4. Confirm `/health` and a non-streaming completion.

OpenRouter credentials are independent of the binary rollback. Revoke and
rotate them if any credential exposure is suspected.

## Risks and mitigations

- **Shared failure domain:** Octoroute on Strix is unavailable if the host
  fails. Document this and move the gateway to a separate host if HA matters.
- **Slot race:** Direct llama.cpp callers can race the free-slot check. Make
  Octoroute the only ingress and use an internal semaphore.
- **Uneven local quality:** Use conservative capability policy, a cloud
  override, and a benchmark corpus.
- **Cloud spend:** Require authentication, rate and concurrency limits, and
  OpenRouter budgets.
- **Schema loss:** Use raw JSON pass-through with narrow, tested mutations.
- **Mid-stream failure:** Enforce the commit-state rule, forward errors, and
  measure them.
- **Changing Auto Beta pool:** Preserve the actual model and cost, and allow
  a configured model policy.
- **Classifier disclosure:** Keep the classifier optional and disabled for
  local-only traffic.
- **Major-version migration:** Require an explicit config version, actionable
  startup errors, a migration guide, and a rollback drill.

## Primary documentation

- [OpenRouter Auto Router](https://openrouter.ai/docs/guides/routing/routers/auto-router)
- [OpenRouter API reference](https://openrouter.ai/docs/api/reference/overview)
- [OpenRouter streaming](https://openrouter.ai/docs/api/reference/streaming)
- [llama.cpp server documentation](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)
