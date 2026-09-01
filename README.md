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

The routing policy comes from configuration alone; Octoroute never classifies a
prompt to choose a route.
`X-Octoroute-Privacy: local-only` narrows a route before admission, so a
local-only request cannot resolve provider credentials or disclose its prompt
to a provider.

## Current runtime

- `POST /v1/chat/completions` with schema-preserving request forwarding.
- `GET /v1/models`, liveness, readiness, and Prometheus endpoints.
- Named virtual routes with ordered local-pool and provider steps.
- Exact local context/capability checks and least-loaded member selection.
- Per-member, per-provider, and inbound concurrency limits, plus an inbound per-minute request rate limit.
- Lazy, isolated provider credentials from environment variables or bounded
  argv commands.
- OpenAI-compatible HTTP dispatch for z.ai, OpenRouter, direct OpenAI, and
  similarly shaped endpoints.
- Explicit Anthropic Messages translation for text, tools, reasoning,
  non-streaming responses, and incremental SSE.
- Locked-down Codex CLI dispatch with ChatGPT-managed authentication, an
  allowlisted child environment, ephemeral read-only execution, and bounded
  structured output.
- An explicit OpenRouter Auto profile owned by Octoroute.
- Cached, bounded provider authentication/reachability probes and fixed-label Prometheus metrics for pool and provider admissions, responses, fallbacks, probes, and routing duration.
- Closed fallback triggers and a held first byte, preventing target changes after response commitment; an optional `first_byte_timeout_ms` bounds how long a hung upstream holds its permits before the route falls forward.

## Quick start

Requirements:

- Rust 1.90 or newer (MSRV). Development is pinned to 1.97.1 by
  `rust-toolchain.toml`; CI is authoritative for MSRV compatibility.
- At least one configured local llama.cpp member or enabled provider.
- The official Codex CLI logged in with ChatGPT when a `codex_cli` provider
  should be ready.
- An inbound gateway secret.

Copy `.env.example` to `.env` beside `config.toml` and set the inbound key plus
credentials for providers you intend to use:

```dotenv
OCTOROUTE_API_KEY=generate-a-long-random-client-secret
OPENROUTER_API_KEY=your-openrouter-key
KIMI_API_KEY=your-kimi-key
ZAI_API_KEY=your-zai-key
OPENAI_API_KEY=your-openai-key
```

Provider credentials are resolved when their route step is selected or when an
operator calls readiness and the provider's cached probe has expired. A
local-only request path itself never resolves them.

Generate or inspect the v3 template:

```bash
cargo run -- config
cargo run -- config --output my-config.toml
```

Start the gateway:

```bash
cargo run --release -- --config config.toml
```

For a same-workstation deployment using `http://127.0.0.1:8080`, use
`config.laptop.toml`.

## Virtual models

The repository template exposes:

| Model | Route contract |
| --- | --- |
| `auto` | Alias for the configured `routing.default_model` |
| `auto-route` | Local pools first, then providers |
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
- `unauthenticated`

Local targets must precede provider targets. A rate limit falls forward only when `rate_limited` is configured; a transport or upstream server failure falls forward only when `precommit_failure` is configured. A missing or rejected upstream credential is returned to the client unless the route opts into `unauthenticated`; that trigger is never in the default set, because falling forward on an expired key silently redirects traffic and spend to the next step. Other committed provider responses are returned to the client.

## Response headers

Successful routed responses include this bounded identity:

- `X-Octoroute-Destination: local|cloud`
- `X-Octoroute-Reason: local_pool|provider`
- `X-Octoroute-Upstream: pool/member` for local work, or the provider name
- `X-Octoroute-Route`
- `X-Octoroute-Target: pool:name|provider:name`
- `X-Octoroute-Pool` and `X-Octoroute-Member` for local work
- `X-Octoroute-Provider` for provider work
- `X-Octoroute-Model-Revision` for local work
- `X-Octoroute-Request-Id` and `X-Request-Id`

Unknown request fields remain intact inside the gateway. Local and generic
OpenAI-compatible HTTP dispatch preserve them except for destination-owned
model/default policy fields; Anthropic and Codex use their explicit adapters.

## Operations

| Endpoint | Authentication | Purpose |
| --- | --- | --- |
| `POST /v1/chat/completions` | Bearer | Routed completion or SSE |
| `GET /v1/models` | Bearer | Virtual model IDs |
| `GET /health/live` | No | Process liveness |
| `GET /health/ready` | No | Aggregate readiness; per-target detail needs the bearer |
| `GET /health` | No | Readiness alias |
| `GET /metrics` | Bearer | Bounded Prometheus exposition |

See [configuration](docs/configuration.md), [API reference](docs/api-reference.md),
[architecture](docs/architecture.md), [observability](docs/observability.md),
[security](docs/security.md), [deployment](docs/deployment.md), and
[development](docs/development.md).

## Development

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --no-default-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps
cargo audit --deny warnings
./scripts/mutants-run.sh
```

Ordinary CI starts mutation work only when tests are added, modified, deleted,
or renamed. Inline tests rerun every mutant in their owning source files;
integration tests, fixtures, snapshots, and ambiguous mappings conservatively
fall back to the full sweep. Production-only revisions skip mutation. Manual
dispatch and the monthly run on the fifth day always sweep the complete tree.
On the following day, the shared `Monthly Mutation Repair` automation repairs
survivors on a branch and enables auto-merge only after every required check is
green. The command above remains the explicit local equivalent.

The implementation and merge contract are tracked in
[the v3 design](docs/plans/octoroute-v3-tiered-inference-fabric.md).

License: MIT.
