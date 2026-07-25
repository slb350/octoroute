# Architecture

## Responsibility boundary

Octoroute is a gateway and a local/cloud router:

```text
client
  |
  v
authentication + bounds + rate/concurrency limits
  |
  v
minimal request facts (model, stream, capabilities, privacy)
  |
  +-- explicit cloud / incompatible / local unavailable --> OpenRouter
  |
  `-- local candidate
        |
        +-- Octoroute semaphore
        +-- llama.cpp health
        +-- llama.cpp free slot
        `-- exact input tokens + output budget + safety margin
                |
                `-- Strix chat completions
```

OpenRouter Auto Beta performs cloud task/model/provider selection. Octoroute
does not reproduce that algorithm or send prompts to a classifier before
deciding whether local work is possible.

## Request path

1. Reject oversized headers.
2. Authenticate exactly one bearer credential.
3. Enforce the configured fixed-window rate and request-concurrency limits.
4. Read the body with a hard byte bound.
5. Parse only the routing envelope while retaining the complete JSON object.
6. Resolve model intent and `X-Octoroute-Privacy`.
7. Skip all Strix probes when cloud is explicit, local capabilities are
   incompatible, or configuration defaults automatic work to cloud.
8. For a local candidate:
   - acquire a non-blocking local permit;
   - read cached `/health`;
   - call `/slots?fail_on_no_slot=1`;
   - call `/v1/chat/completions/input_tokens`;
   - include `max_completion_tokens`, `max_tokens`, or the configured default
     output reservation in the context calculation.
9. Dispatch to one upstream with its own credential.
10. Buffer the first upstream body chunk. Before this commit point, eligible
    automatic local failures may spill to cloud.
11. Stream all remaining bytes with backpressure. The upstream and concurrency
    permits live until the body completes or is dropped.

## Schema fidelity

`GatewayRequest` keeps the original bytes and a complete JSON object. Octoroute
patches only:

- the destination `model`;
- the server-owned OpenRouter `auto-router` plugin fields for automatic cloud
  routing.

Unknown fields, message content blocks, OpenRouter plugins, `session_id`,
streaming comments, data frames, `[DONE]`, usage, cost, and actual response
model pass through.

## Privacy and fallback

Forced-local intent is defined by:

- `model: local`;
- the configured local alias (`strixtea`);
- `X-Octoroute-Privacy: local-only`.

Those paths never return a cloud destination. Automatic local attempts may
fall back only on a pre-commit connection/body failure or retryable local 5xx.
After one body byte is client-visible, upstream switching is impossible.

## Modules

| Module | Responsibility |
| --- | --- |
| `gateway/config` | Versioned parsing, validation, secret resolution |
| `gateway/env` | Process-over-dotenv secret layering without global mutation |
| `gateway/auth` | Fixed-length hashed constant-time bearer verification |
| `gateway/request` | Minimal facts and schema-preserving model mutation |
| `gateway/routing` | Typed intent, privacy, decision, and bounded reasons |
| `gateway/local` | llama.cpp health/slot/token admission and permit lifecycle |
| `gateway/openrouter` | Auto Router plugin and model policy mutation |
| `gateway/transport` | Credential isolation and pre-commit streaming state |
| `gateway/service` | Limits, routing, fallback, response headers |
| `gateway/http` | Axum endpoints |
| `gateway/metrics` | Bounded Prometheus registry |

## Failure posture

- Configuration and secret failures stop startup.
- Probe transport, status, and schema failures are fail-closed for local
  admission.
- Explicit local returns an error when unavailable; it never silently spills.
- OpenRouter failures are returned without an Octoroute retry to another cloud
  model. Provider fallback belongs to OpenRouter.
- Prompt bodies and credentials are never included in safe error messages.

## Current live contract

Verified on 2026-07-22:

- Strix alias: `strixtea`
- model file: `Agents-A1-Q8_0.gguf`
- context: 65,536
- parallel slots: 1
- `/health`: `{"status":"ok"}`
- `/slots?fail_on_no_slot=1`: 200 when idle, 503 when busy
- `/v1/chat/completions/input_tokens`: `{input_tokens, object}`
- OpenRouter Auto Beta non-streaming and SSE both return the actual selected
  model and usage/cost.
