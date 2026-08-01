# Configuration

Octoroute accepts only `config_version = 2`. A v1 tier configuration returns
an actionable migration error.

Secrets are names, not values, in TOML. Resolution order is:

1. process environment;
2. optional `.env` beside the selected config file.

Loading `.env` does not mutate process-global environment state.
Start from `.env.example`. Other provider variables may coexist in `.env`;
Octoroute reads only names referenced by the active configuration.

## Complete example

```toml
config_version = 2

[server]
host = "0.0.0.0"
port = 8081
api_key_env = "OCTOROUTE_API_KEY"
max_request_bytes = 8388608
max_header_bytes = 32768
max_in_flight = 32
requests_per_minute = 120

[upstreams.local]
kind = "llama_cpp"
name = "strix"
base_url = "http://127.0.0.1:8080"
model = "strixtea"
context_window = 65536
context_safety_tokens = 1024
default_max_output_tokens = 4096
max_in_flight = 1
health_cache_ttl_ms = 1000
probe_timeout_ms = 2000
# Optional and intentionally omitted by default:
# first_byte_timeout_ms = 45000
capabilities = ["chat", "stream"]
health_path = "/health"
slots_path = "/slots?fail_on_no_slot=1"
input_tokens_path = "/v1/chat/completions/input_tokens"

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
auto_model = "openrouter/auto"
cost_quality_tradeoff = 9
allowed_models = []
app_title = "Octoroute"
max_in_flight = 8
health_cache_ttl_ms = 10000
probe_timeout_ms = 3000

[routing]
default = "prefer_local"
fallback_before_commit = true
semantic_mode = "shadow"
decision_timeout_ms = 30000

[observability]
log_level = "info"
```

This is the Strix deployment profile. Port 8081 is used because Strix already
serves Gitea on port 3000. For development from another LAN host, change the
local base URL to `http://strix.local:8080`.

## Server

- `host` must be an IP address.
- `port` must be nonzero.
- `api_key_env` is required even on loopback because cloud routing can spend
  money.
- `max_request_bytes` is 1–64 MiB.
- `max_header_bytes`, `max_in_flight`, and `requests_per_minute` must be
  positive and bounded.

## Local llama.cpp

- `base_url` accepts HTTP or HTTPS, has no credentials/query/fragment, and is
  normalized with a trailing slash.
- `model` is the exact alias sent to llama.cpp.
- `context_safety_tokens + default_max_output_tokens` must leave input
  capacity.
- `max_in_flight` is Octoroute’s non-blocking semaphore. Match it to safe
  llama.cpp parallel capacity.
- Probe paths must be same-origin absolute paths.
- `health_cache_ttl_ms` and `probe_timeout_ms` must be positive.
- `first_byte_timeout_ms`, when set, must be positive. Configure it only from
  measured Strix prompt-processing behavior; omission means no invented local
  first-byte deadline.

Capabilities are a closed enum:

```text
chat
stream
tools
structured_output
image_input
audio_input
video_input
reasoning
```

Only enable a capability after verifying the live model/server contract.
OpenRouter-only plugins and non-text output always route cloud.

`api_key_env` is optional for a protected local llama.cpp server.

## OpenRouter

- `base_url` must use HTTPS.
- `auto_model` defaults to `openrouter/auto`.
- `cost_quality_tradeoff` is an integer from 0 through 10.
- `allowed_models` accepts OpenRouter wildcard patterns. Empty means the
  configured Auto Router pool is unrestricted.
- Octoroute’s Auto Router fields override conflicting client fields while
  preserving unrelated plugins and unknown options.
- `max_in_flight` is the global cloud concurrency ceiling.
- Readiness uses authenticated `GET /api/v1/key` with a cached result.

## Routing

`default` is:

- `prefer_local`: apply the configured semantic mode to compatible automatic
  work, then use local capacity or cloud accordingly;
- `cloud`: use OpenRouter unless the caller explicitly forces local.

`semantic_mode` is:

- `disabled`: skip classification and proceed directly to local admission;
- `shadow` (default): classify and record the bounded outcome, but never let
  that outcome select cloud; local availability and context admission remain
  authoritative;
- `enforced`: honor `local` or `cloud` classifier decisions.

`decision_timeout_ms` bounds semantic decisions in shadow and enforced modes.
In shadow mode, a classifier failure does not select cloud when local capacity
was already reserved. In enforced mode, a timeout, invalid decision, or local
routing-model failure sends automatic traffic safely to OpenRouter. Explicit
local and local-only requests always bypass semantic routing.

`fallback_before_commit` only applies to automatic requests initially
admitted locally. Forced-local privacy is never weakened.

## Validation and redaction

Unknown fields fail startup. Raw `api_key` fields fail parsing. URL
credentials, invalid header values, empty secrets, invalid paths, and unsafe
limits fail startup. Debug output uses `SecretString` redaction.
