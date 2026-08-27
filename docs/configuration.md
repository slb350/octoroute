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
name = "local-model"
base_url = "http://127.0.0.1:8080"
model = "local-model"
model_revision = "example-local-revision"
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
local_success_threshold = 0.50
boundary_threshold_step = 0.10
shadow_sample_rate = 1.0
session_latch_enabled = false
session_latch_ttl_ms = 900000
session_latch_max_entries = 1024
session_latch_evidence_threshold = 2

[observability]
log_level = "info"
```

This is the local model deployment profile. Port 8081 is used because local model already
serves Gitea on port 3000. For development from another LAN host, change the
local base URL to `http://local-model.local:8080`.

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
- `model_revision` is a required immutable release identifier for the loaded
  weights. It is limited to 128 visible ASCII bytes without whitespace and must
  change whenever the weights change under the same alias; it becomes part of
  the capability-card fingerprint and calibration dataset identity.
- `context_safety_tokens + default_max_output_tokens` must leave input
  capacity.
- `max_in_flight` is Octoroute’s non-blocking semaphore. Match it to safe
  llama.cpp parallel capacity.
- Probe paths must be same-origin absolute paths.
- `health_cache_ttl_ms` and `probe_timeout_ms` must be positive.
- `first_byte_timeout_ms`, when set, must be positive. Configure it only from
  measured local model prompt-processing behavior; omission means no invented local
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

- `disabled`: skip semantic forecasting and proceed directly to local admission;
- `shadow` (default): forecast and record the bounded policy outcome, but never
  let that outcome select cloud; local availability and context admission
  remain authoritative;
- `enforced`: honor the destination selected by deterministic forecast policy.

`decision_timeout_ms` bounds semantic decisions in shadow and enforced modes.
`local_success_threshold` is the minimum forecast probability for a supported
request to remain local. `boundary_threshold_step` adds one step for uncertain
or unmatched forecasts and two steps for unsupported forecasts. Both values
must be finite probabilities, and the strictest threshold must not exceed one.
In shadow mode, a forecasting failure does not select cloud when local capacity
was already reserved. In enforced mode, a timeout, invalid decision, or local
routing-model failure sends automatic traffic safely to OpenRouter. Explicit
local and local-only requests always bypass semantic routing.

`shadow_sample_rate` is a finite probability from `0.0` through `1.0` and
defaults to `1.0`. It applies only to compatible automatic traffic in shadow
mode. Octoroute hashes the server-generated request ID for a deterministic
decision without hashing prompt content or retaining sampling state. Skipped
requests continue through normal local admission and do not produce semantic
forecast or decision metrics. `octoroute_semantic_sampling_total{outcome}`
records the bounded `sampled` or `skipped` outcome. Enforced mode always runs
the forecaster regardless of this setting. Keep the value at `1.0` for
benchmark and calibration collection.

`session_latch_enabled` is an optional enforced-mode refinement and is false by
default. When enabled, `session_latch_evidence_threshold` consecutive cloud
forecasts with the closed `unsupported`/`known_local_limit` evidence pair
latch compatible automatic requests carrying the same valid `session_id` to
cloud. `session_latch_ttl_ms` bounds both pending evidence and active latches;
`session_latch_max_entries` bounds the in-memory table. Session IDs are limited
to 128 non-control bytes for policy use and stored only as SHA-256 hashes. A
non-hard forecast clears pending evidence. Explicit local and `local-only`
requests always bypass the latch. Valid ranges are 1000-86400000 ms, 1-10000
entries, and an evidence threshold of 2-10.

`fallback_before_commit` only applies to automatic requests initially
admitted locally. Forced-local privacy is never weakened.

## Validation and redaction

Unknown fields fail startup. Raw `api_key` fields fail parsing. URL
credentials, invalid header values, empty secrets, invalid paths, and unsafe
limits fail startup. Debug output uses `SecretString` redaction.
