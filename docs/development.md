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
RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps
cargo audit --deny warnings
./scripts/mutants-run.sh
```

The repository has no `benches/` directory and no `[[bench]]` target, so there
is no benchmark step. Ordinary CI starts mutation work only for added, modified,
deleted, or renamed tests. Inline tests rerun every mutant in their owning
source files; integration tests, fixtures, snapshots, and ambiguous mappings
fall back to the complete sweep. Production-only revisions stop after the fast
policy preflight. Manual dispatch and the monthly run on the fifth day always
sweep the tree. A failed run retains only its bounded mutation repair evidence;
the following day's `Monthly Octoroute Mutation Repair` automation fixes
survivors through a branch-protected PR and auto-merges only after all gates are
green. `just mutants` remains the
explicit local sweep and offloads to `homelab-1.local` when that host is
reachable, falling back to a local run with a warning when it is not. That host
is shared with self-hosted CI runners, so the sweep is capped at
`CPUQuota=500%` of its 16 cores; raise it with
`OCTOROUTE_MUTANTS_CPUQUOTA` when you know the box is idle.

`just check` runs clippy and the formatting check. `just test` runs the tests,
`just mutants` the mutation sweep, and `just validate` all of them. The focused
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

The `fabric` module owns all runtime routing and dispatch. `auth`, `env`, `http_client`, and `request`
are protocol and security primitives shared by that one implementation.

## Testing boundaries

Unit and integration coverage should preserve these invariants:

- unknown request fields survive destination model rewriting;
- malformed or unsupported content is not admitted locally;
- local pool admission distinguishes disabled, incompatible, busy, unhealthy,
  context-overflow, and token-count-unavailable states, asserted against the
  production admission path;
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
Anthropic-compatible providers go through the explicit message, tool,
reasoning, error, and streaming translator, because the two wire formats differ.

## Adding a command provider

Execution providers must be compiled known kinds, never arbitrary shell
commands. The Codex adapter requires a filtered child environment, ChatGPT-
managed authentication, ephemeral non-interactive execution, bounded timeout
and output, disabled unrelated capabilities, and structured event parsing.

## Documentation

Update the generated template, configuration/API docs, runtime status, and
public integration tests in the same change as a new user-visible contract.
