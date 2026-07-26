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
  `-- automatic local candidate
        |
        `-- constrained semantic decision on Strix
              |
              +-- needs stronger intelligence --> OpenRouter `openrouter/auto`
              |
              `-- locally capable
                    |
                    +-- Octoroute semaphore
                    +-- llama.cpp health and free slot
                    `-- exact input tokens + output budget + safety margin
                |
                `-- Strix chat completions
```

Octoroute decides whether the task is suitable for the configured local model.
It uses Strix itself for that bounded semantic decision, so a prompt selected
for local work has not first been disclosed to a cloud classifier. OpenRouter
Auto performs cloud model/provider selection only after Octoroute chooses
cloud.

## Request path

1. Reject oversized headers.
2. Authenticate exactly one bearer credential.
3. Enforce the configured fixed-window rate and request-concurrency limits.
4. Read the body with a hard byte bound.
5. Parse only the routing envelope while retaining the complete JSON object.
6. Resolve model intent and `X-Octoroute-Privacy`.
7. Skip semantic routing when cloud is explicit, local capabilities are
   incompatible, or configuration defaults automatic work to cloud.
8. For an automatic compatible request, reserve an idle Strix slot and request
   a constrained `local` or `cloud` JSON decision with thinking disabled.
9. Send a cloud decision to `openrouter/auto`. If the semantic decision fails
   or local capacity is unavailable, fail safely to cloud.
10. For a local decision or forced-local request, acquire a non-blocking local
    permit, verify health and a free slot, obtain exact input tokens, and
    include the requested or default output reservation in the safe context
    calculation.
11. Dispatch to one upstream with its own credential.
12. Buffer the first upstream body chunk. Before this commit point, eligible
    automatic local failures may spill to cloud.
13. Stream all remaining bytes with backpressure. The upstream and concurrency
    permits live until the body completes or is dropped.

## Schema fidelity

`GatewayRequest` keeps the complete JSON object. Octoroute patches only:

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
| `gateway/intelligence` | Local semantic task-suitability decision |
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
- Semantic routing failures send automatic traffic to OpenRouter with the
  bounded `router_failure` reason.
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
- OpenRouter Auto non-streaming and SSE both return the actual selected
  model and usage/cost.
