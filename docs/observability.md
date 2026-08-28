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

`GET /metrics` requires bearer authentication and exposes fixed-cardinality v3
gauges and counters:

```text
octoroute_fabric_runtime_info{config_version="3",provider_runtime="complete"} 1
octoroute_fabric_pool_enabled{pool="workers"} 1
octoroute_fabric_provider_enabled{provider="openrouter"} 1
octoroute_fabric_provider_admissions_total{provider="openrouter",state="admitted"} 0
octoroute_fabric_provider_responses_total{provider="openrouter",outcome="success"} 0
octoroute_fabric_provider_fallbacks_total{provider="openrouter",trigger="rate_limited"} 0
octoroute_fabric_provider_probes_total{provider="openrouter",state="ready"} 0
octoroute_fabric_pool_admissions_total{pool="workers",state="admitted"} 0
octoroute_fabric_pool_fallbacks_total{pool="workers",trigger="busy"} 0
octoroute_fabric_routing_duration_seconds_bucket{le="0.05"} 0
octoroute_fabric_anthropic_unknown_types_total 0
octoroute_fabric_codex_unknown_events_total 0
```

`octoroute_fabric_pool_fallbacks_total` is the signal that local capacity is
spilling to cloud. Without it the first sign of a degraded local fleet is the
provider bill. `octoroute_fabric_routing_duration_seconds` covers admission and
the health, slot, and token-count probes, up to the moment a lease is held; it
excludes the upstream call.

The two `unknown` counters record content blocks, events, and deltas Octoroute
skipped as unrecognized. A rising count means an upstream added something this
release does not translate.

Pool and provider labels come only from validated configuration. Prompt text,
model output, credentials, session IDs, and provider error strings must never
become labels.

Admission states, response outcomes, fallback triggers, and probe states are
closed enums. Every configured provider/enumerated-value pair is emitted even
when its value is zero, so series cardinality is bounded at startup.

## Liveness and readiness

`GET /health/live` confirms the process is serving the v3 runtime.

`/health/ready` and `/health` are unauthenticated so an orchestrator can probe
them, and return the status code plus an aggregate `status` of `ready`,
`degraded`, or `not_ready`. `degraded` means some configured target is
unavailable while others still serve - the case where a dead local fleet is
being covered by billed cloud capacity. The per-pool and per-provider breakdown
names every configured target, so it is returned only to an authenticated
caller. Readiness answers are cached briefly: a readiness pass spawns
`codex doctor` and sends credentialed `/models` probes, and the endpoints are
reachable by anyone who can reach the port.

`GET /health/ready` and `/health` return per-pool and per-provider states. Pools
and providers are checked concurrently; each pool checks eligible members in
selection order, with a member's health and slot probes concurrent when its
health cache is stale. Enabled HTTP providers resolve their credential and
perform a body-free authenticated reachability request;
Codex providers perform `codex doctor --json` and require ChatGPT-managed auth.
Each provider result is cached for `readiness_ttl_ms`, refreshes coalesce, and
the operation is bounded by `readiness_timeout_ms`. A provider with no available
permit reports `busy` immediately.

The process reports ready when at least one pool or provider runtime is ready.
Readiness is unauthenticated and contains no prompt data, but a cache refresh
can resolve a provider credential or launch the Codex diagnostic. Restrict it
to operator networks and avoid polling faster than the configured TTL.

## Streaming failures

The first upstream body chunk is obtained before commitment. A failure before
that point can be mapped to configured fallback policy. Errors after commitment
remain stream failures and cannot switch targets. Surrounding proxies should
retain request IDs when recording those failures.
