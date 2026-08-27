# API reference

Octoroute implements the OpenAI Chat Completions surface at `/v1`.

## Authentication

Protected endpoints require:

```http
Authorization: Bearer <OCTOROUTE_API_KEY>
```

Missing, repeated, malformed, and incorrect credentials return `401` with
`WWW-Authenticate: Bearer`.

## POST `/v1/chat/completions`

The body must be a JSON object with:

- `model`: non-empty string;
- `messages`: non-empty array;
- `stream`: optional boolean.

Other fields are preserved. Output context admission uses
`max_completion_tokens` first, then `max_tokens`, then
`default_max_output_tokens`. A null optional limit is treated as unset.

Local eligibility requires message objects with supported roles and string or
verified nonempty typed-array content. Only an assistant message with valid
tool calls may omit content or set it to null. Tool-role or assistant
tool-call history requires the local `tools` capability even when the request
omits top-level tool definitions. Malformed message/content shapes and
unsupported typed block names route automatic requests to cloud; forced-local
requests return `400` rather than sending an incompatible body to local model.

### Model values

| Value | Route |
| --- | --- |
| `auto` | Intelligently choose capable local execution or OpenRouter Auto |
| `local` | Force local, never cloud |
| `local-model` | Exact configured local alias, never cloud |
| `cloud` | Force OpenRouter Auto |
| `openrouter/auto` | Force OpenRouter Auto |
| `provider/model` | Force an exact OpenRouter model |

Unknown unqualified names return `400`.

### Privacy

```http
X-Octoroute-Privacy: local-only
```

The header must appear at most once and have exactly that value. Combining it
with cloud intent is a `400`. If local admission fails, Octoroute returns an
error and does not contact OpenRouter.

### Streaming

With `"stream": true`, Octoroute forwards upstream SSE body bytes opaquely,
including comments, data frames, usage chunks, and `[DONE]`. It does not parse
and reconstruct frames.

### Gateway headers

Successful upstream responses include:

- `X-Octoroute-Destination`
- `X-Octoroute-Reason`
- `X-Octoroute-Upstream`
- `X-Octoroute-Request-Id`
- `X-Request-Id`

The Octoroute request ID is always the gateway-generated correlation UUID.
`X-Request-Id` preserves a safe upstream value when present and otherwise
matches it.

Reason values are bounded:

```text
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
cloud_quality
router_failure
session_cloud_latch
```

`cloud_quality` and `router_failure` are emitted only when semantic routing is
`enforced`. In `shadow` mode, classifier outcomes are observable through
metrics but do not replace the actual destination reason.
`session_cloud_latch` is emitted only when the optional enforced-mode session
latch is active for automatic traffic; explicit local and `local-only`
requests bypass the latch.

### Errors

Gateway-created errors use:

```json
{
  "error": {
    "message": "safe explanation",
    "type": "invalid_request_error",
    "code": "routing_error"
  }
}
```

Common statuses:

| Status | Meaning |
| --- | --- |
| 400 | Invalid body, model, privacy, capability, or output budget |
| 401 | Authentication failed |
| 413 | Body exceeds `max_request_bytes` |
| 429 | Rate, request-concurrency, or cloud-concurrency limit |
| 431 | Headers exceed `max_header_bytes` |
| 502 | Upstream failed before commitment and fallback was unavailable |
| 503 | Forced-local request cannot currently be admitted |

Raw upstream non-2xx statuses and bodies pass through after safe header
filtering unless an automatic local 5xx triggers pre-commit cloud fallback.

## GET `/v1/models`

Requires bearer authentication. Returns OpenAI model objects for:

- `auto`
- `local`
- `cloud`
- the configured local alias

## Health

- `GET /health/live`: process liveness
- `GET /health/ready`: concurrent local and OpenRouter readiness
- `GET /health`: alias of readiness

Health endpoints are not authenticated.

## GET `/metrics`

Requires bearer authentication and returns Prometheus text exposition.
