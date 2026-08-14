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
        `-- semantic mode
              |
              +-- disabled --------------------> local admission
              +-- shadow decision -------------> local admission
              `-- enforced decision
                    +-- cloud ------------------> OpenRouter `openrouter/auto`
                    `-- local ------------------> local admission
                    |
                    +-- Octoroute semaphore
                    +-- llama.cpp health and free slot
                    `-- exact input tokens + output budget + safety margin
                |
                `-- Strix chat completions
```

When semantic routing is enabled, Octoroute uses Strix itself for a bounded
success forecast, so a prompt selected for local work has not first been
disclosed to a cloud classifier. Strix returns a probability, capability
boundary, closed rule, and short crux; deterministic Octoroute policy selects
the destination from that forecast. `shadow` is the default mode and records
the policy outcome without acting on it; `disabled` skips forecasting;
`enforced` lets the policy outcome select local or cloud. OpenRouter Auto
performs cloud model/provider selection only after Octoroute chooses cloud.

## Request path

1. Reject oversized headers.
2. Authenticate exactly one bearer credential.
3. Enforce the configured fixed-window rate and request-concurrency limits.
4. Read the body with a hard byte bound.
5. Parse only the routing envelope while retaining the complete JSON object.
6. Resolve model intent and `X-Octoroute-Privacy`.
7. Skip semantic routing when cloud is explicit, local capabilities are
   incompatible, or configuration defaults automatic work to cloud.
8. For shadow or enforced mode, reserve an idle Strix slot and request a
   constrained success forecast with thinking disabled, then apply the
   configured deterministic threshold policy. Disabled mode skips forecasting.
9. In shadow mode, record the bounded outcome and continue local admission.
   In enforced mode, send a cloud decision or safe classifier failure to
   `openrouter/auto`.
10. For a local path or forced-local request, acquire a non-blocking local
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
model pass through. Routing inspects message roles and typed-content shapes
without rewriting them; malformed shapes fail closed, and historical tool
messages require the configured local tool capability.

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
- Enforced semantic-routing failures send automatic traffic to OpenRouter
  with the bounded `router_failure` reason. Shadow failures do not select a
  destination when local capacity was successfully reserved.
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
