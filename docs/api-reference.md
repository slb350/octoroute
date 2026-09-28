# API reference

Octoroute exposes an OpenAI-compatible v3 surface. Unless noted otherwise,
protected endpoints require:

```http
Authorization: Bearer <OCTOROUTE_API_KEY>
```

## `POST /v1/chat/completions`

The body must be a JSON object with a bounded virtual `model` identifier and a
non-empty `messages` array. `stream`, when present and non-null, must be a
boolean. Model identifiers use 1-128 ASCII letters, digits, dots, underscores,
or hyphens.

Octoroute parses only the fields needed for validation, local capability
admission, token budgeting, and provider defaults. Unknown fields and message
content remain intact in the gateway; local and generic OpenAI-compatible
dispatch preserve them, while explicit Anthropic and Codex adapters translate
only their documented compatible subset.

`model` resolves as follows:

- `auto` selects the configured default virtual route;
- any other value must exactly name a configured route.

Optional request privacy:

```http
X-Octoroute-Privacy: local-only
```

This header removes provider steps before dispatch. If no local target remains,
the request fails without resolving a provider credential or sending prompt
data outside the local network.

### Local capability inference

Local admission recognizes chat, streaming, tools/tool history, structured
output, image/audio/video input, and reasoning controls. Unknown or malformed
message/content shapes fail closed as locally incompatible and can proceed only
to a provider when both the route privacy and fallback policy allow it.

The local output reservation reads `n_predict` first, then
`max_completion_tokens`, then `max_tokens`. The body reaches llama.cpp
unchanged, and llama.cpp lets an explicit `n_predict` override the value derived
from `max_tokens`, so budgeting any other way would reserve one number while the
member generates another. When none is present, the selected pool's
`default_max_output_tokens` reserves output context. llama.cpp documents
`n_predict: -1` as unlimited, which no context reservation can cover. An
enabled local pool whose capabilities cover the request therefore rejects a
negative `n_predict` with `400` (`invalid_token_budget`), and the route ends
there instead of falling forward to a later provider step. A route that
reaches no such pool never budgets `n_predict`. An OpenAI-compatible provider
receives it unchanged, as it does any unknown field. The Anthropic adapter has
no mapping for it and refuses the request as `incompatible`. `n_predict: 0`,
which llama.cpp documents as evaluating the prompt without generating, is a
real zero-token budget.

### Success response headers

Every routed response includes bounded route identity:

| Header | Meaning |
| --- | --- |
| `X-Octoroute-Destination` | `local` or `cloud` |
| `X-Octoroute-Reason` | `local_pool` or `provider` |
| `X-Octoroute-Route` | Selected virtual route |
| `X-Octoroute-Target` | `pool:name` or `provider:name` |
| `X-Octoroute-Upstream` | Selected pool/member or provider |
| `X-Octoroute-Pool` | Local pool, when local |
| `X-Octoroute-Member` | Local member, when local |
| `X-Octoroute-Model-Revision` | Local model revision, when local |
| `X-Octoroute-Provider` | Provider name, when cloud |
| `X-Octoroute-Request-Id` | Gateway-generated UUID |
| `X-Request-Id` | Safe upstream ID when supplied, otherwise gateway UUID |

Local and OpenAI-compatible HTTP response bytes are forwarded opaquely after
the first upstream body chunk is buffered. Anthropic Messages responses and
SSE events are translated into OpenAI Chat Completions shapes. Codex CLI output
is validated and returned as a non-streaming completion or a single completion
chunk followed by `[DONE]`; the CLI adapter does not expose token-by-token
streaming. Codex routes reject multi-choice requests before prompt disclosure.

### Errors

Errors use the OpenAI-compatible envelope:

```json
{
  "error": {
    "message": "bounded operator-safe message",
    "type": "invalid_request_error",
    "code": "bounded_code"
  }
}
```

Representative statuses:

- `400` for invalid JSON, envelope, privacy, route, or token budget, and when
  the governing rejection is a local pool that cannot serve the request
  (`local_incompatible`) or whose context window it exceeds
  (`local_context_overflow`);
- `401` for missing or invalid bearer authentication;
- `408` when the request body does not arrive within
  `server.request_body_timeout_ms`;
- `413` for request bodies above the configured limit;
- `429` for inbound rate or concurrency limits;
- `431` for headers above the configured limit;
- `502` for a selected upstream failure before commitment when fallback is not
  allowed or no later step exists, and when a local member or provider rejects
  Octoroute's own credential (`local_credential_rejected`,
  `provider_credential_rejected`), so a client never mistakes an upstream `401`
  for its own bearer failing;
- `503` when no eligible target is available: disabled, busy, unhealthy, unable
  to count input tokens, missing or refused its credential
  (`local_unauthenticated`, `provider_unauthenticated`), or a provider without a
  compatible adapter (`provider_incompatible`).

When a route ends on an admission rejection, either because it ran out of
steps or because a step refused for a trigger outside the route's
`fallback_on`, the status and code come from the most significant admission
rejection the route collected, not from the last step tried: a missing or
refused operator credential outranks a request the caller must fix, which
outranks capacity or health. Within one tier the first rejection wins.

Other upstream statuses are committed responses and reach the client as the
upstream sent them. OpenAI-compatible and local error bodies pass through
unchanged; Anthropic error bodies are rebuilt into this envelope, keeping the
upstream `error.message` (truncated to 2 KiB) and its `error.type` as
`upstream_type`. Gateway-generated errors never include request bodies,
credentials, or raw provider responses.

## `GET /v1/models`

Authenticated. Returns `auto` plus every configured virtual route:

```json
{
  "object": "list",
  "data": [
    {"id": "auto", "object": "model", "created": 0, "owned_by": "octoroute"}
  ]
}
```

## `GET /health/live`

Unauthenticated process liveness:

```json
{"status":"ok","config_version":3}
```

## `GET /health/ready` and `GET /health`

Unauthenticated aggregate readiness. Every caller receives the status code and
an aggregate `status`:

```json
{"status":"ready","config_version":3}
```

The per-target breakdown names every configured pool and provider, so it is
added only when the request carries the gateway bearer:

```json
{
  "status": "degraded",
  "config_version": 3,
  "pools": {"workers": "unavailable"},
  "providers": {"openrouter": "ready"},
  "provider_runtime": "complete"
}
```

The HTTP status is `200` when at least one pool or provider reports `ready`,
otherwise `503`. The aggregate `status` is:

- `ready`: at least one target is `ready` and every other is `ready`, `busy`,
  or `disabled`;
- `degraded`: at least one target is `ready` while another reports a value
  other than `ready`, `busy`, or `disabled` - for example a dead local fleet
  covered by billed cloud capacity;
- `not_ready`: no target is ready, which includes a fleet where every target
  is `busy`.

Only targets that some route can reach are reported. The whole snapshot is
cached for five seconds, so an anonymous caller cannot turn readiness requests
into probes, credential commands, or Codex diagnostics at request rate.

Pool values are `ready`, `disabled`, `busy`, `unavailable`,
`token_count_unavailable`, or `unauthenticated`. A pool is checked member by
member in selection order: cached health, a free `/slots` entry, and a small
token-count request. It is `ready` as soon as one member passes all three.
Otherwise it reports, in this order of precedence, `unauthenticated` if any
member rejected Octoroute's credential, `busy` if any member was busy,
`token_count_unavailable` if any member could not count tokens, and
`unavailable` otherwise.

Provider values are `ready`, `disabled`, `busy`, `unavailable`, or
`unauthenticated` (with `incompatible` retained as a closed state). A provider
with no free permit reports `busy` without probing. Enabled HTTP providers
resolve their credential and issue a credential-bearing `GET` to the provider's
derived `models` URL:

- `2xx`, `405`, and `429` report `ready`: the endpoint answered;
- `401`, `403`, and `407` report `unauthenticated` and discard the cached
  credential; a credential that cannot be resolved at all also reports
  `unauthenticated`;
- `404` is ambiguous - a provider without a models listing, or a wrong base
  path - so Octoroute sends one more `GET`, to the provider's inference URL
  (`chat/completions`, or `messages` for the Anthropic protocol). A `401`,
  `403`, or `407` there reports `unauthenticated`; another `404`, a `5xx`, or no
  answer reports `unavailable`; any other status reports `ready`, meaning
  reachable rather than authenticated;
- every other status, including `400` and `5xx`, and a timeout or transport
  failure report `unavailable`.

Codex providers run bounded `codex doctor --json`. A CLI that is not logged in
through ChatGPT, or whose diagnostic output does not match the contract, reports
`unauthenticated`; a missing executable, a timeout, a non-zero exit, or another
process failure reports `unavailable`.

Provider results are cached per provider for `readiness_ttl_ms`, concurrent
refreshes coalesce, and each refresh is bounded by `readiness_timeout_ms`. A
dispatch that fails before commitment, or answers with a `5xx` or a credential
rejection, discards that provider's cached result so the next pass probes
again.

Readiness sends no prompt or request body, but it can resolve provider
credentials and execute the Codex diagnostic, so operators should restrict
network access to this endpoint.

## `GET /metrics`

Authenticated Prometheus text exposition
(`text/plain; version=0.0.4; charset=utf-8`) with these families:

- `octoroute_fabric_runtime_info{config_version,provider_runtime}` gauge;
- `octoroute_fabric_pool_enabled{pool}` and
  `octoroute_fabric_provider_enabled{provider}` gauges;
- `octoroute_fabric_pool_admissions_total{pool,state}`;
- `octoroute_fabric_pool_fallbacks_total{pool,trigger}`, the signal that local
  capacity is spilling to the next route step;
- `octoroute_fabric_provider_admissions_total{provider,state}`;
- `octoroute_fabric_provider_responses_total{provider,outcome}`;
- `octoroute_fabric_provider_fallbacks_total{provider,trigger}`;
- `octoroute_fabric_provider_probes_total{provider,state}`;
- `octoroute_fabric_routing_duration_seconds` histogram: admission work for one
  route step, excluding its upstream call, observed for rejected steps too;
- `octoroute_fabric_unknown_upstream_types_total{adapter}`: upstream content
  blocks, events, and deltas skipped as unrecognized.

Every configured pool or provider is rendered with every value of its closed
label, including zero values. Label values come only from validated
configuration and closed enums. [Observability](observability.md) explains how
to read each family.

## Security headers

Every route adds `nosniff`, frame denial, no-referrer, restrictive permissions
policy, and a deny-all content security policy. A gateway request ID is added
even when a handler returns before routing.
