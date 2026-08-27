# Development

Octoroute supports Rust 1.90 and stable Rust. The repository keeps one v3
runtime; there is no version-dispatch compatibility path.

## Checks

Run the same gates as CI:

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --no-default-features
cargo bench --locked --all-features --no-run
RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps
cargo audit --deny warnings
```

`just check` runs the primary formatting, lint, and test path. The focused
public-contract tests are `cli_config_command` and `gateway_v3`.

## Source layout

```text
src/
  cli.rs
  main.rs
  telemetry.rs
  gateway/
    auth.rs
    env.rs
    http_client.rs
    request.rs
    fabric/
      config.rs
      policy.rs
      local_pool.rs
      provider.rs
      anthropic.rs
      codex.rs
      metrics.rs
      transport.rs
      service.rs
      http.rs
      http_support.rs
```

The `fabric` module owns all runtime routing and dispatch. `auth`, `env`,
`http_client`, and `request` are protocol/security primitives rather than a
second gateway implementation.

## Testing boundaries

Unit and integration coverage should preserve these invariants:

- unknown request fields survive destination model rewriting;
- malformed or unsupported content is not admitted locally;
- local pool selection distinguishes disabled, incompatible, busy, unhealthy,
  and context-overflow states;
- member and provider permits remain held until response bodies are dropped;
- `local-only` removes provider targets before any credential resolution;
- cached readiness resolves credentials only on refresh, coalesces concurrent
  callers, and never includes prompt data;
- Anthropic messages, tools, reasoning, finish reasons, usage, errors, and
  fragmented SSE translate into OpenAI-compatible shapes;
- Codex child environments exclude gateway/provider secrets and every run is
  ephemeral, read-only, bounded, and contract-validated;
- each fallback class requires its matching configured trigger;
- 429 and pre-commit failures do not fall forward indiscriminately;
- committed non-retryable provider responses are returned;
- safe errors do not echo TOML values, prompts, or credentials;
- generated `config.toml` round-trips through `FabricConfig`;
- the complete Axum application forwards local SSE bytes opaquely.

Use WireMock for local/provider contract tests. Use a temporary fake executable
for Codex lifecycle and environment assertions. Tests should assert exact
request bodies and bounded headers, and should use zero-contact or
environment-read assertions for privacy guarantees. The OpenCode-style
Anthropic tool/SSE and Codex end-to-end cases live in `service_tests.rs`.

## Adding an HTTP provider adapter

Keep protocol translation isolated from the registry and executor:

1. validate protocol-specific configuration statically;
2. advertise compatibility only for verified request features;
3. resolve credentials only after selection or a body-free expired readiness
   probe;
4. prepare headers plus the first body chunk before commitment;
5. classify failures into the closed route trigger set;
6. retain permits through the response body;
7. test streaming, tools, reasoning, errors, and secret redaction.

OpenAI-compatible providers share one schema-preserving adapter.
Anthropic-compatible providers use the explicit message, tool, reasoning,
error, and streaming translator rather than pretending wire compatibility.

## Adding a command provider

Execution providers must be compiled known kinds, never arbitrary shell
commands. The Codex adapter requires a filtered child environment, ChatGPT-
managed authentication, ephemeral non-interactive execution, bounded timeout
and output, disabled unrelated capabilities, and structured event parsing.

## Documentation

Update the generated template, configuration/API docs, runtime status, and
public integration tests in the same change as a new user-visible contract.
