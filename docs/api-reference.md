# API reference

Octoroute exposes an OpenAI-compatible v3 surface. Unless noted otherwise,
protected endpoints require:

```http
Authorization: Bearer <OCTOROUTE_API_KEY>
```

## `POST /v1/chat/completions`

The body must be a JSON object with a non-empty string `model` and a non-empty
`messages` array. `stream`, when present and non-null, must be a boolean.

Octoroute parses only the fields needed for validation, local capability
admission, token budgeting, and provider defaults. Unknown fields and message
content remain schema-preserving.

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

`max_completion_tokens` takes precedence over `max_tokens`; when neither is
present, the selected pool's configured default reserves output context.

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

The response body and SSE stream are forwarded opaquely. Octoroute buffers only
the first upstream body chunk so fallback remains possible before commitment.

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

- `400` for invalid JSON, envelope, privacy, route, capability, or token budget;
- `401` for missing or invalid bearer authentication;
- `413` for request bodies above the configured limit;
- `429` for inbound rate or concurrency limits;
- `431` for headers above the configured limit;
- `502` for a selected upstream failure before commitment when fallback is not
  allowed or no later step exists;
- `503` when no eligible target is available, disabled, busy, unhealthy, or
  adapter-incompatible.

Errors never include request bodies, credentials, or raw provider responses.

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

Unauthenticated bounded admission snapshot. Local pools are actively probed;
provider state is currently non-probing and reflects enabled, adapter, and
permit state.

The status is `200` when at least one pool or provider runtime reports ready,
otherwise `503`.

## `GET /metrics`

Authenticated Prometheus text exposition for configured pool/provider enablement
and runtime identity. Label values come only from validated configuration.

## Security headers

Every route adds `nosniff`, frame denial, no-referrer, restrictive permissions
policy, and a deny-all content security policy. A gateway request ID is added
even when a handler returns before routing.
