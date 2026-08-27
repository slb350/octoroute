# Observability

Octoroute exposes bounded logs, request/response identity headers, readiness,
and Prometheus text without inspecting streamed content.

## Logs

`observability.log_level` accepts `trace`, `debug`, `info`, `warn`, or `error`.
The runtime logs startup/shutdown, bounded route identities, admission states,
pre-commit failures, and committed statuses.

Safe fields include:

- generated request ID;
- configured route, pool, member, provider, and model revision;
- bounded admission/fallback state;
- HTTP status and failure phase.

Prompts, responses, credentials, credential-command output, and arbitrary
provider errors are not log fields.

## Correlation headers

`X-Octoroute-Request-Id` is generated for every request and is the primary log
join key. `X-Request-Id` preserves an allowlisted upstream request ID when one
exists; otherwise it receives the gateway ID.

Successful routed responses also identify the configured route and selected
target. These values are bounded by configuration validation.

## Metrics

`GET /metrics` requires bearer authentication and currently exposes bootstrap
v3 gauges:

```text
octoroute_fabric_runtime_info{config_version="3",provider_runtime="open_ai"} 1
octoroute_fabric_pool_enabled{pool="workers"} 1
octoroute_fabric_provider_enabled{provider="openrouter"} 1
```

Pool and provider labels come only from validated configuration. Prompt text,
model output, credentials, session IDs, and provider error strings must never
become labels.

Detailed bounded counters and latency histograms remain an implementation item;
see [runtime status](v3-runtime-status.md).

## Liveness and readiness

`GET /health/live` confirms the process is serving the v3 runtime.

`GET /health/ready` and `/health` return per-pool and per-provider states. Pool
readiness probes eligible members concurrently. Provider readiness is currently
non-probing and reflects enabled, adapter compatibility, and available permits.

The process reports ready when at least one pool or provider runtime is ready.
An OpenAI provider may therefore appear ready before its lazy credential is
first resolved; credential/authentication probes are a tracked next boundary.

## Streaming failures

The first upstream body chunk is obtained before commitment. A failure before
that point can be mapped to configured fallback policy. Errors after commitment
remain stream failures and cannot switch targets. Surrounding proxies should
retain request IDs when recording those failures.
