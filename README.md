# Octoroute

Octoroute is a local-first, OpenAI-compatible LLM gateway for a personal
Strix llama.cpp server and OpenRouter.

```text
OpenAI client
     |
     v
 Octoroute
   |    \
   |     `-- OpenRouter `openrouter/auto` --> cloud model/provider
   |
   `-- compatible work --> Strix answer
          |
          `-- optional local semantic decision: shadow or enforced
```

Octoroute owns the local-versus-cloud decision. OpenRouter owns cloud model
and provider selection. The routing decision stays on the local network.

## What it does

- Exposes `POST /v1/chat/completions` and `GET /v1/models`.
- Preserves unknown request fields and forwards response/SSE bytes opaquely.
- Keeps malformed message/content shapes away from local inference and
  requires verified local tool capability for tool-call history.
- Keeps compatible `auto` work local by default while observing bounded
  semantic decisions on Strix in shadow mode.
- Can disable semantic routing entirely or explicitly enforce it so work that
  needs stronger intelligence routes to OpenRouter `openrouter/auto`.
- Also uses OpenRouter when Strix lacks a requested capability, is busy or
  unhealthy, or cannot fit the exact prompt plus output budget.
- Accepts exact OpenRouter slugs such as
  `deepseek/deepseek-v4-flash`.
- Guarantees that `model: local`, the exact local alias, and
  `X-Octoroute-Privacy: local-only` never fall back to cloud.
- Falls back from an automatic local attempt only before the first response
  body byte is committed.
- Enforces bearer authentication, request/header limits, fixed-window rate
  limiting, per-credential concurrency, and a global cloud concurrency limit.
- Exposes bounded-cardinality Prometheus metrics and health endpoints.

## Quick start

Requirements:

- Rust 1.90 or newer
- A llama.cpp server exposing `/health`, `/slots`, and
  `/v1/chat/completions/input_tokens`
- An OpenRouter API key

Copy `.env.example` to an ignored `.env` beside `config.toml`, then fill the
two required values:

```dotenv
OCTOROUTE_API_KEY=generate-a-long-random-client-secret
OPENROUTER_API_KEY=your-openrouter-key
```

Other provider credentials may remain in `.env`; Octoroute v2 reads only the
environment variable names referenced by `config.toml`.

Generate or inspect the v2 configuration:

```bash
cargo run -- config
cargo run -- config --output config.toml
```

The repository configuration targets the live Strix contract:

```toml
config_version = 2

[server]
host = "0.0.0.0"
port = 8081

[upstreams.local]
kind = "llama_cpp"
name = "strix"
base_url = "http://127.0.0.1:8080"
model = "strixtea"
context_window = 65536
max_in_flight = 1

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
auto_model = "openrouter/auto"
cost_quality_tradeoff = 9

[routing]
semantic_mode = "shadow"
decision_timeout_ms = 30000
local_success_threshold = 0.50
boundary_threshold_step = 0.10
```

`semantic_mode` is `disabled`, `shadow`, or `enforced`. Shadow is the default:
it records the forecast-derived outcome without letting that outcome select
the destination. The local model forecasts `p_local_success` and a closed
capability boundary; Octoroute—not the model—applies the configured threshold.
`boundary_threshold_step` raises that threshold for uncertain, unsupported,
and unmatched forecasts, with two steps for unsupported forecasts. Enforced
mode should be enabled only after its judgment is
validated against representative labeled traffic. Shadow and enforced modes
add one local forecasting inference—about 760–1500 ms in the measured Strix
profile—to compatible `auto` requests.

The forecast prompt includes the versioned
`octoroute-strix-capability-card/v1`. It identifies the configured local alias,
lists only capabilities enabled in configuration, and records measured local
limitations without exposing URLs, credentials, prompts, or runtime state.

This profile is for running Octoroute on Strix. The `config.laptop.toml`
profile instead binds Octoroute to laptop loopback and uses
`http://strix.local:8080` as the local upstream.

Start the gateway on Strix:

```bash
cargo run --release -- --config config.toml
```

Start the gateway on the laptop:

```bash
cargo run --release -- --config config.laptop.toml
```

Point an OpenAI-compatible client at `http://strix.local:8081/v1` for the
Strix deployment or `http://127.0.0.1:8081/v1` for the laptop deployment.
Use `OCTOROUTE_API_KEY` as the client API key.

## Model intent

| `model` value | Behavior | Cloud fallback |
| --- | --- | --- |
| `auto` | Capable Strix or stronger cloud | Before commitment |
| `local` | Force Strix | Never |
| `strixtea` | Force the exact configured local alias | Never |
| `cloud` | Force OpenRouter Auto | OpenRouter-managed only |
| `openrouter/auto` | Force OpenRouter Auto | OpenRouter-managed only |
| `provider/model` | Force that OpenRouter model | OpenRouter-managed only |

Example:

```bash
curl http://strix.local:8081/v1/chat/completions \
  -H "Authorization: Bearer $OCTOROUTE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [{"role": "user", "content": "Explain this Rust error"}],
    "stream": true
  }'
```

For a request that must remain local:

```bash
curl http://strix.local:8081/v1/chat/completions \
  -H "Authorization: Bearer $OCTOROUTE_API_KEY" \
  -H "X-Octoroute-Privacy: local-only" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [{"role": "user", "content": "Private prompt"}]
  }'
```

If local admission fails, this returns an error rather than contacting a
cloud service.

## Gateway response headers

- `X-Octoroute-Destination: local|cloud`
- `X-Octoroute-Reason`: bounded route reason such as `local_capable`,
  `cloud_quality`, `local_busy`, or `local_early_failure`
- `X-Octoroute-Upstream: strix|openrouter`
- `X-Request-Id`

Octoroute never rewrites OpenRouter’s returned `model`; callers see the model
that actually answered.

## Operations

| Endpoint | Authentication | Purpose |
| --- | --- | --- |
| `POST /v1/chat/completions` | Bearer | Routed completion/SSE |
| `GET /v1/models` | Bearer | Virtual and local model IDs |
| `GET /health/live` | No | Process liveness |
| `GET /health/ready` | No | Aggregated Strix/OpenRouter readiness |
| `GET /health` | No | Readiness alias |
| `GET /metrics` | Bearer | Prometheus exposition |

See:

- [Architecture](docs/architecture.md)
- [API reference](docs/api-reference.md)
- [Configuration](docs/configuration.md)
- [V1-to-v2 migration](docs/migration-v2.md)
- [Security](docs/security.md)
- [Deployment](docs/deployment.md)
- [Observability](docs/observability.md)
- [Development](docs/development.md)

## Development

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The active routing decision record is
[docs/plans/intelligent-auto-routing.md](docs/plans/intelligent-auto-routing.md).

## Scope

Octoroute v2 intentionally does not provide direct Anthropic, Google,
OpenAI, or DeepSeek adapters. OpenRouter is the single cloud boundary. It
also does not execute tools, rewrite prompts, persist conversations, or
provide a UI.

License: MIT.
