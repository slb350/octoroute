# Octoroute

Octoroute is a local-first, OpenAI-compatible LLM gateway for a personal
Strix llama.cpp server and OpenRouter.

```text
OpenAI client
     |
     v
 Octoroute
   |    \
   |     `-- OpenRouter Auto Beta --> cloud model/provider
   |
   `-- Strix llama.cpp (`strixtea`) when compatible, healthy, and idle
```

Octoroute owns the local-versus-cloud decision. OpenRouter owns cloud model
and provider selection. A cloud classifier is not called before local work.

## What it does

- Exposes `POST /v1/chat/completions` and `GET /v1/models`.
- Preserves unknown request fields and forwards response/SSE bytes opaquely.
- Routes `auto` requests locally only when Strix supports the requested
  capabilities, has a free slot, and the exact prompt plus output budget fits
  the configured context window.
- Uses OpenRouter `openrouter/auto-beta` for everything else.
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
auto_model = "openrouter/auto-beta"
cost_quality_tradeoff = 9
```

This profile is for running Octoroute on Strix. When developing on another
host, change the local base URL to `http://strix.local:8080`.

Start the gateway on Strix:

```bash
cargo run --release -- --config config.toml
```

Then point an OpenAI-compatible client at `http://strix.local:8081/v1` and use
`OCTOROUTE_API_KEY` as its API key.

## Model intent

| `model` value | Behavior | Cloud fallback |
| --- | --- | --- |
| `auto` | Eligible idle Strix, else OpenRouter | Before commitment |
| `local` | Force Strix | Never |
| `strixtea` | Force the exact configured local alias | Never |
| `cloud` | Force OpenRouter Auto Beta | OpenRouter-managed only |
| `openrouter/auto-beta` | Force OpenRouter Auto | OpenRouter-managed only |
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
  `local_busy`, or `local_early_failure`
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

The implementation plan and decision record are in
[docs/plans/local-cloud-routing-gateway.md](docs/plans/local-cloud-routing-gateway.md).

## Scope

Octoroute v2 intentionally does not provide direct Anthropic, Google,
OpenAI, or DeepSeek adapters. OpenRouter is the single cloud boundary. It
also does not execute tools, rewrite prompts, persist conversations, or
provide a UI.

License: MIT.
