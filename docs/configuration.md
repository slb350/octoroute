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
timeout_ms = 1800000
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

Member `priority` is a tiebreaker among equally loaded members, not a preference
that outranks load: a busy `priority = 10` member loses to an idle
`priority = 100` one. It cannot pin traffic to a particular member, and it never
revives one that is unhealthy or out of permits.

A member address must be loopback, private-range, link-local, or a `.local`,
`.localhost`, `.internal`, or `.home.arpa` name. A public address is refused at
startup, which is what makes `local-only` a guarantee rather than a convention.
Carrier-grade NAT (100.64.0.0/10) is not private range, so a member reached over
a mesh network such as Tailscale is refused; give it a `.local` name or a
private-range address instead.

Admission checks the request first, then each member in turn:

1. the request uses only the pool's declared capabilities;
2. the output reservation plus safety tokens leave room in the context window.
   A request that cannot fit whatever its prompt tokenizes to is rejected here,
   before any member is probed and before the prompt is disclosed;
3. the pool and member are enabled;
4. a member concurrency permit is available;
5. health and slot probes succeed;
6. exact input tokens are counted, and input + output + safety fit the window.

Steps 4 and 5 are in that order deliberately: taking the permit first closes the
window in which two of Octoroute's own requests both see the same free `/slots`
entry and both claim it. The cost is that the permit is held across the health
and slot probes (up to 2 seconds) and the token count (`token_count_timeout_ms`,
15 seconds by default). With `max_in_flight = 1`, a second request arriving
inside that window is reported `busy` even though the member is healthy and
idle, and on the default `fallback_on` it spills to the next route step. Raise
`max_in_flight` above 1 on a member that can genuinely serve concurrent
requests, and watch `octoroute_fabric_pool_fallbacks_total{trigger="busy"}` to
see whether the spill is real contention or this window.

The slot check is a snapshot, so it cannot account for other clients of the same
llama.cpp server. If something outside Octoroute shares a member, the pool can
over-admit; the request then queues upstream, bounded by `first_byte_timeout_ms`
if one is configured.

Local dispatch injects the selected pool's `default_reasoning_effort` only when
the pool declares the `reasoning` capability and the caller sent none of
`reasoning_effort`, `reasoning`, or `include_reasoning`. Caller controls win, and
a pool that does not declare `reasoning` sends nothing whatever its
`default_reasoning_effort` says.

The selected member permit remains held through the complete response body.
`timeout_ms` is the complete local chat deadline. Local and provider request
deadlines default to 30 minutes and must be between 1 millisecond and one hour.
Member and provider `max_in_flight` values must be between 1 and 10,000.

Optional `first_byte_timeout_ms`, accepted by both pools and providers, bounds
how long an upstream may take to produce its first body byte. `timeout_ms`
covers the whole response and is legitimately long for a large generation, so
without this a hung upstream holds its member permit and the inbound permit for
that full window before the route can fall forward. It must not exceed
`timeout_ms`. Leave it unset unless you have measured the upstream: Octoroute
invents no deadline of its own.

Pools also accept `token_count_timeout_ms`, the deadline for one
`/v1/chat/completions/input_tokens` call, defaulting to 15 seconds with a
two-minute maximum. Tokenizing a prompt near the request-size ceiling on a busy
server takes materially longer than a health or slot probe.

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
temperature = 0.2
profile = "open_router_auto"
```

HTTP endpoints must use HTTPS. Configure exactly one credential source:

```toml
api_key_env = "PROVIDER_API_KEY"
```

or a bounded argv without shell interpretation:

```toml
api_key_command = ["secret-tool", "lookup", "service", "provider"]
```

Credential commands accept at most 32 arguments, 4 KiB per argument, and 16
KiB total, with no empty argument or control character. They run with a cleared
environment carrying only an allowlist (`PATH`, `HOME`, `TMPDIR`, locale, and
proxy variables, which `op`, `pass`, `gcloud`, and `aws` all need), null
stdin/stderr, a five-second deadline, and a 4 KiB output bound.

A command runs when a provider is selected for a request and when its readiness
is refreshed, not on selection alone. Resolved credentials are cached for five
minutes and discarded when the provider answers 401 or 403, so a rotated key is
picked up without a restart and a command is not spawned per request.

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
Requests for multiple choices, unsupported media, or provider-specific plugins
are rejected as `incompatible` before the CLI receives the prompt.

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

Targets use `pool:name` or `provider:name`. Every target must exist and may
appear only once in a route. Local pools must precede providers so a disclosed
request can never return to local execution. Route order is the provider
preference order; providers do not have a separate priority field.

Privacy values:

- `local_only`: steps may reference pools only;
- `cloud_allowed`: local pools may precede providers;
- `cloud_only`: steps may reference providers only.

The request header `X-Octoroute-Privacy: local-only` further narrows any route
to local steps before admission. Repeated, malformed, or unknown privacy values
are rejected.

The closed set of fallback values has seven members: `busy`, `unhealthy`,
`context_overflow`, `incompatible`, `rate_limited`, `precommit_failure`, and
`unauthenticated`.

Omitting `fallback_on` enables the first six. `unauthenticated` is deliberately
excluded from that default and must be requested explicitly. It fires when a
provider's credential is missing, expired, or rejected, which is an operator
error rather than a capacity condition: falling forward on it turns a dead key
into traffic and spend silently redirected to the next provider, discovered on
the bill. Add it to `fallback_on` only for a route where continuing past a
rejected credential is genuinely what you want.

Specify an explicit subset when a route should stop sooner.

`model: auto` resolves to `routing.default_model`, so `auto` is reserved and
cannot also name a virtual route. Other model values must match a configured
route exactly and use at most 128 ASCII letters, digits, dots, underscores, or
hyphens. Configured upstream model IDs use at most 512 visible ASCII bytes
without whitespace.

## Observability

```toml
[observability]
log_level = "info"
```

Valid levels are `trace`, `debug`, `info`, `warn`, and `error`.
