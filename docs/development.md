# Development

## Toolchain

- Rust edition 2024
- MSRV 1.90
- Axum/Tokio
- reqwest with rustls and streaming

The tracked toolchain file selects the supported compiler automatically:

```bash
cargo test --locked --all-targets --all-features
```

CI remains authoritative for the pinned Rust 1.90 build.

## Workflow

Every behavior change follows:

1. write a failing test;
2. implement the smallest correct typed behavior;
3. format and run the focused tests;
4. refactor while green;
5. run the full suite and Clippy.

Commands:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo doc --no-deps
```

Wiremock tests bind loopback ports. Sandboxed environments must permit local
listeners.

## Test layers

- configuration, secret, auth, request, and policy unit contracts;
- llama.cpp health/slot/token admission tests;
- permit cancellation and panic-unwind tests;
- OpenRouter policy mutation tests;
- credential-isolated transport and opaque stream tests;
- service fallback/privacy/limit tests using a fake transport;
- public Axum integration tests using real reqwest-to-wiremock local traffic;
- live Strix and low-cost OpenRouter canaries before release.

Fixtures use synthetic prompts and test credentials. Never record a personal
prompt or real key.

## Source layout

```text
src/
  calibration.rs
  main.rs
  cli.rs
  telemetry.rs
  gateway/
    auth.rs
    config.rs
    config/validation.rs
    env.rs
    http.rs
    local.rs
    metrics.rs
    openrouter.rs
    request.rs
    routing.rs
    sampling.rs
    session_latch.rs
    service/
    trajectory.rs
    transport.rs
```

Keep source and test files below 600 lines where practical and never above
800 lines. Split by responsibility before adding a second concern.

## Live contracts

For Strix:

```bash
curl http://strix.local:8080/health
curl 'http://strix.local:8080/slots?fail_on_no_slot=1'
curl -H 'Content-Type: application/json' \
  -d '{"model":"strixtea","messages":[{"role":"user","content":"probe"}]}' \
  http://strix.local:8080/v1/chat/completions/input_tokens
```

For OpenRouter, run canaries through Octoroute rather than calling the
provider directly. Keep output limits small and inspect:

- actual response model;
- usage/cost;
- streaming `[DONE]`;
- Octoroute destination/reason/upstream headers.

## Security review checklist

- no raw secret fields or debug leakage;
- auth occurs before body consumption;
- request/header/rate/concurrency limits are active;
- outbound Authorization is chosen by destination, never forwarded;
- cookies and hop-by-hop headers are stripped;
- local-only invariants are tested;
- fallback only happens pre-commit;
- metrics labels are bounded;
- OpenRouter uses HTTPS;
- config and dotenv parser errors omit source values.

## Design record

See [plans/intelligent-auto-routing.md](plans/intelligent-auto-routing.md) for
the v2 decision table and failure semantics. See
[plans/calibrated-semantic-routing.md](plans/calibrated-semantic-routing.md) for
the shipped forecast policy and its still-pending labeled evaluation gate.
