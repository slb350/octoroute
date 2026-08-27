# Octoroute

Octoroute v3 is an OpenAI-compatible inference fabric. Clients use stable
virtual model names while Octoroute selects an eligible local llama.cpp pool or
an ordered provider target.

```text
OpenAI-compatible client
          |
          v
     Octoroute v3
       /      \
local pools   provider chain
```

The routing policy is explicit configuration, not prompt classification.
`X-Octoroute-Privacy: local-only` narrows a route before admission, so a
local-only request cannot resolve provider credentials or disclose its prompt
to a provider.

## Current runtime

- `POST /v1/chat/completions` with schema-preserving request forwarding.
- `GET /v1/models`, liveness, readiness, and Prometheus endpoints.
- Named virtual routes with ordered local-pool and provider steps.
- Exact local context/capability checks and least-loaded member selection.
- Per-member, per-provider, and inbound concurrency limits.
- Lazy, isolated provider credentials from environment variables or bounded
  argv commands.
- OpenAI-compatible HTTP dispatch for z.ai, OpenRouter, direct OpenAI, and
  similarly shaped endpoints.
- An explicit OpenRouter Auto profile owned by Octoroute.
- Closed fallback triggers and a held first byte, preventing target changes
  after response commitment.

Anthropic-compatible HTTP and Codex CLI providers are accepted by the schema
but currently report `incompatible`; their dedicated adapters are the next
runtime boundaries.

## Quick start

Requirements:

- Rust 1.90 or newer.
- At least one configured local llama.cpp member or OpenAI-compatible provider.
- An inbound gateway secret.

Copy `.env.example` to `.env` beside `config.toml` and set the inbound key plus
credentials for providers you intend to use:

```dotenv
OCTOROUTE_API_KEY=generate-a-long-random-client-secret
OPENROUTER_API_KEY=your-openrouter-key
ZAI_API_KEY=your-zai-key
```

Provider credentials are resolved only when their route step is selected.

Generate or inspect the v3 template:

```bash
cargo run -- config
cargo run -- config --output my-config.toml
```

Start the gateway:

```bash
cargo run --release -- --config config.toml
```

For a workstation deployment using `http://local-model.local:8080`, use
`config.laptop.toml`.

## Virtual models

The repository template exposes:

| Model | Route contract |
| --- | --- |
| `auto` | Alias for the configured `routing.default_model` |
| `worker` | Local worker pool only |
| `supervisor` | Optional local supervisor, then configured providers |
| `local` | Local pools only |
| `cloud-sota` | Provider-only escalation |

Virtual names and physical targets are configuration. A client never needs a
pool member URL or provider credential.

Example request:

```bash
curl http://127.0.0.1:8081/v1/chat/completions \
  -H "Authorization: Bearer $OCTOROUTE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "worker",
    "messages": [{"role": "user", "content": "Explain this Rust error"}],
    "stream": true
  }'
```

Force any cloud-eligible route to remain local:

```bash
curl http://127.0.0.1:8081/v1/chat/completions \
  -H "Authorization: Bearer $OCTOROUTE_API_KEY" \
  -H "X-Octoroute-Privacy: local-only" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [{"role": "user", "content": "Private prompt"}]
  }'
```

If no local target can accept the request, Octoroute returns an error without
contacting a provider.

## Fallback contract

Each route opts into a subset of these bounded triggers:

- `busy`
- `unhealthy`
- `context_overflow`
- `incompatible`
- `rate_limited`
- `precommit_failure`

Local targets must precede provider targets. A rate limit falls forward only
when `rate_limited` is configured; a transport or upstream server failure falls
forward only when `precommit_failure` is configured. Authentication failures
and other committed provider responses are returned to the client.

## Response headers

Successful routed responses include bounded identity such as:

- `X-Octoroute-Destination: local|cloud`
- `X-Octoroute-Route`
- `X-Octoroute-Target: pool:name|provider:name`
- `X-Octoroute-Pool` and `X-Octoroute-Member` for local work
- `X-Octoroute-Provider` for provider work
- `X-Octoroute-Model-Revision` for local work
- `X-Octoroute-Request-Id` and `X-Request-Id`

Unknown request fields and upstream response/SSE bytes remain opaque except for
destination-specific model/default policy fields.

## Operations

| Endpoint | Authentication | Purpose |
| --- | --- | --- |
| `POST /v1/chat/completions` | Bearer | Routed completion or SSE |
| `GET /v1/models` | Bearer | Virtual model IDs |
| `GET /health/live` | No | Process liveness |
| `GET /health/ready` | No | Pool/provider admission snapshot |
| `GET /health` | No | Readiness alias |
| `GET /metrics` | Bearer | Bounded Prometheus exposition |

See [configuration](docs/configuration.md), [API reference](docs/api-reference.md),
[architecture](docs/architecture.md), [security](docs/security.md), and
[runtime status](docs/v3-runtime-status.md).

## Development

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo doc --locked --all-features --no-deps
```

The implementation plan and remaining adapter work are tracked in
[the v3 design](docs/plans/octoroute-v3-tiered-inference-fabric.md).

License: MIT.
