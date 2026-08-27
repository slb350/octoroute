# Configuration

Octoroute accepts only `config_version = 3`. The parser rejects unknown fields,
invalid cross-references, mixed privacy targets, and provider-to-local fallback
order before the listener is bound. Parse errors report a location without
echoing source values.

The canonical example and generated template is [`config.toml`](../config.toml).

## Server

```toml
config_version = 3

[server]
host = "0.0.0.0"
port = 8081
api_key_env = "OCTOROUTE_API_KEY"
max_request_bytes = 8388608
max_header_bytes = 32768
max_in_flight = 64
requests_per_minute = 120
```

`api_key_env` names the inbound bearer secret. The process environment takes
precedence over the optional `.env` beside the selected config file. Raw
credentials are never valid TOML fields.

## Local pools

Each pool describes equivalent llama.cpp endpoints with one model identity and
capability contract:

```toml
[[fabric.local_pools]]
name = "workers"
enabled = true
model = "coding-worker-model"
model_revision = "immutable-release-id"
context_window = 131072
context_safety_tokens = 2048
default_max_output_tokens = 16384
capabilities = ["chat", "stream", "tools", "structured_output", "reasoning"]
strategy = "least_loaded"
default_reasoning_effort = "medium"

[[fabric.local_pools.members]]
name = "worker-0"
base_url = "http://worker-0.local:8000"
max_in_flight = 1
priority = 100
```

Supported capabilities are `chat`, `stream`, `tools`, `structured_output`,
`image_input`, `audio_input`, `video_input`, and `reasoning`. Every pool must
include `chat`. Change `model_revision` whenever the loaded weights change.

Each member may also set `enabled` and `api_key_env`. Local URLs may use HTTP or
HTTPS but cannot contain credentials, queries, or fragments. The runtime
derives `/health`, `/slots?fail_on_no_slot=1`,
`/v1/chat/completions/input_tokens`, and `/v1/chat/completions` from the base.

Admission requires all of the following:

1. the pool and member are enabled;
2. the request uses only declared capabilities;
3. health and slot probes succeed;
4. a member concurrency permit is available;
5. input tokens + output reservation + safety tokens fit the context window.

The selected member permit remains held through the complete response body.

## Providers

### OpenAI-compatible HTTP

```toml
[[fabric.providers]]
name = "openrouter"
enabled = true
kind = "http"
endpoint = "https://openrouter.ai/api/v1"
protocol = "open_ai"
model = "openrouter/auto"
api_key_env = "OPENROUTER_API_KEY"
max_in_flight = 4
timeout_ms = 1800000
readiness_ttl_ms = 30000
readiness_timeout_ms = 30000
priority = 30
temperature = 0.2
profile = "open_router_auto"
```

HTTP endpoints must use HTTPS. Configure exactly one credential source:

```toml
api_key_env = "PROVIDER_API_KEY"
```

or a non-empty argv without shell interpretation:

```toml
api_key_command = ["secret-tool", "lookup", "service", "provider"]
```

Credential commands run only after selection with a cleared environment (plus
`PATH`), null stdin/stderr, a five-second deadline, and a 4 KiB output bound.

Optional request defaults are `reasoning_effort`, `temperature`, and
`max_tokens`. Client-supplied non-null values win. `profile =
"open_router_auto"` installs Octoroute's `auto-router` plugin policy and removes
client-supplied `allowed_models` from that plugin.

### Anthropic-compatible HTTP

```toml
[[fabric.providers]]
name = "kimi"
kind = "http"
endpoint = "https://api.kimi.com/coding/v1"
protocol = "anthropic"
model = "k3"
api_key_env = "KIMI_API_KEY"
max_tokens = 200000
max_in_flight = 2
timeout_ms = 1800000
```

The adapter derives `messages`, authenticates with `x-api-key` plus
`anthropic-version: 2023-06-01`, and requires a configured `max_tokens` default.
A valid client `max_completion_tokens` or `max_tokens` overrides that default.
Text messages, system/developer instructions, function tools and tool history,
reasoning effort, sampling controls, stop sequences, non-streaming responses,
and incremental SSE are translated explicitly.

Requests for multimodal output, images/audio/video, log probabilities, multiple
choices, or JSON-schema/object response formats are incompatible and may fall
forward only when the route allows `incompatible`.

### Provider readiness

Every provider accepts `readiness_ttl_ms` and `readiness_timeout_ms`. Defaults
are 30 seconds; the maximum TTL is one hour and the maximum probe timeout is
five minutes. HTTP probes resolve the lazy credential and issue a body-free
authenticated `GET` to the derived `models` URL. Codex probes run
`doctor --json`. Probe state is cached and concurrent refreshes coalesce.

### Codex CLI

```toml
[[fabric.providers]]
name = "codex"
kind = "codex_cli"
model = "gpt-5.6-sol"
executable = "codex"
max_in_flight = 1
timeout_ms = 1800000
reasoning_effort = "xhigh"
```

Codex CLI entries do not accept endpoint, protocol, credential, sampling,
token, or profile fields. `executable` is accepted only for this kind and
defaults to `codex`; it is a direct executable path, not a shell command.

The runtime requires the official CLI to be logged in through ChatGPT. Each
request runs ephemerally in an empty read-only workspace with user config,
rules, tools, web, apps, hooks, memories, and subagents disabled. Output must
match the bounded structured adapter contract. A streaming OpenAI request is
returned as one complete SSE chunk plus `[DONE]`, not token-by-token output.

## Virtual routes

```toml
[routing]
default_model = "auto-route"

[[routing.routes]]
model = "auto-route"
privacy = "cloud_allowed"
steps = ["pool:workers", "provider:zai", "provider:openrouter"]
default_reasoning_effort = "medium"
fallback_on = ["busy", "unhealthy", "context_overflow", "incompatible", "rate_limited", "precommit_failure"]
```

Targets use `pool:name` or `provider:name`. Every target must exist. Local pools
must precede providers so a disclosed request can never return to local
execution.

Privacy values:

- `local_only`: steps may reference pools only;
- `cloud_allowed`: local pools may precede providers;
- `cloud_only`: steps may reference providers only.

The request header `X-Octoroute-Privacy: local-only` further narrows any route
to local steps before admission. Repeated, malformed, or unknown privacy values
are rejected.

Fallback values are `busy`, `unhealthy`, `context_overflow`, `incompatible`,
`rate_limited`, and `precommit_failure`. Omitting `fallback_on` enables the full
closed set; specify an explicit subset when a route should stop sooner.

`model: auto` resolves to `routing.default_model`. Other model values must
match a configured route exactly.

## Observability

```toml
[observability]
log_level = "info"
```

Valid levels are `trace`, `debug`, `info`, `warn`, and `error`.
