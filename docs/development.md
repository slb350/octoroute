# Development Guide

Developer guide for testing, contributing, and building Octoroute.

---

## Table of Contents

1. [Getting Started](#getting-started)
2. [Development Workflow](#development-workflow)
3. [Testing Strategy](#testing-strategy)
4. [Benchmarking](#benchmarking)
5. [Code Quality](#code-quality)
6. [Contributing Guidelines](#contributing-guidelines)

---

## Getting Started

### Prerequisites

- **Rust**: 1.90.0 or later (MSRV)
- **Cargo**: Included with Rust
- **Git**: For version control

**Optional**:
- **just**: Task runner (alternative to `cargo` commands)
- **cargo-watch**: Auto-rebuild on file changes
- **cargo-nextest**: Faster test runner

### Installation

```bash
# Install Rust via rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install optional tools
cargo install just
cargo install cargo-watch
cargo install cargo-nextest
```

### Clone Repository

```bash
git clone https://github.com/slb350/octoroute.git
cd octoroute
```

### Build

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

# Or use just (only release builds available)
just build-release
```

### Run

```bash
# Must run from directory containing config.toml
cargo run

# Or use just (runs with debug logging)
just run

# Note: Configuration file must be named config.toml in current directory
```

---

## Development Workflow

### Task Automation (justfile)

The `justfile` provides convenient development commands:

```bash
# Run clippy and format checks (no tests)
just check

# Run all tests
just test

# Run only unit tests
just test-unit

# Run only integration tests
just test-integration

# Watch for changes and rebuild
just watch

# Auto-fix clippy warnings
just clippy-fix

# Format code
just fmt

# Run benchmarks
just bench

# Generate documentation
just docs

# Clean build artifacts
just clean

# Complete CI check (clippy + tests only)
just ci
```

### Watch Mode

Auto-rebuild on file changes:

```bash
# Watch and rebuild
just watch

# Watch and run tests
cargo watch -x test

# Watch and run specific test
cargo watch -x "test test_rule_router"
```

### IDE Setup

**Visual Studio Code**:
- Install `rust-analyzer` extension
- Enable clippy: Add to `.vscode/settings.json`:
  ```json
  {
    "rust-analyzer.checkOnSave.command": "clippy"
  }
  ```

**IntelliJ IDEA / CLion**:
- Install Rust plugin
- Enable clippy in settings

---

## Testing Strategy

### Test Organization

```
tests/
├── unit tests (in src/ alongside code)
│   ├── src/router/*.rs           # Router unit tests
│   ├── src/models/*.rs           # Model selection tests
│   └── src/config/*.rs           # Configuration tests
│
└── integration tests (in tests/ directory)
    ├── chat_integration.rs
    ├── retry_logic.rs
    ├── concurrent_routing.rs
    ├── timeout_enforcement.rs
    ├── stream_interruption.rs
    ├── background_health_checks.rs
    └── ... (see tests/ directory for full list)
```

### Running Tests

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_rule_router

# Run tests matching pattern
cargo test router::

# Run tests in specific file
cargo test --test chat_integration

# Run with nextest (faster)
cargo nextest run
```

### Unit Tests

Located alongside code using `#[cfg(test)]` modules.

**Example**:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_router_casual_chat() {
        let router = RuleBasedRouter;
        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        let decision = router.route(&meta);
        assert_eq!(decision, Some(RoutingDecision::new(ModelTier::Fast)));
    }
}
```

**Coverage**:
- Router decision logic
- Configuration parsing and validation
- Error handling and conversion
- Health check state transitions
- Model selection algorithms

### Integration Tests

Located in `tests/` directory.

**Example**:

```rust
#[tokio::test]
async fn test_chat_endpoint_with_rule_routing() {
    let config = Config::from_toml(TEST_CONFIG).unwrap();
    let app_state = AppState::new(config).await.unwrap();

    let request = ChatRequest {
        prompt: "Hello!".to_string(),
        importance: Importance::Low,
        task_type: TaskType::CasualChat,
        tier: None,
    };

    let response = chat_handler(State(app_state), Json(request))
        .await
        .unwrap();

    assert_eq!(response.0.model_tier(), ModelTier::Fast);
    assert_eq!(response.0.routing_strategy(), "rule");
}
```

**Coverage**:
- End-to-end request → routing → model invocation → response
- Retry logic with health-aware selection
- Timeout handling and enforcement
- Concurrent request handling
- Error responses and status codes

### Test Coverage

Octoroute maintains extensive test coverage across all components:
- Comprehensive unit tests for all modules
- Integration tests for end-to-end request flows
- Zero clippy warnings
- All tests passing

Run `cargo test --all` to verify current test count and results.

**Generate coverage report** (requires `cargo-tarpaulin`):

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Html
```

---

## Benchmarking

### Running Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench routing

# Or use just
just bench
```

### Benchmark Organization

Located in `benches/routing.rs`:

- **Metadata creation**: RouteMetadata construction performance
- **Config parsing**: config.toml parsing latency
- **Token estimation**: Token counting performance
- **Rule routing**: Rule-based routing latency

### Example Benchmark

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_rule_routing(c: &mut Criterion) {
    let router = RuleBasedRouter;
    let meta = RouteMetadata {
        token_estimate: 500,
        importance: Importance::Normal,
        task_type: TaskType::QuestionAnswer,
    };

    c.bench_function("rule_routing", |b| {
        b.iter(|| {
            router.route(black_box(&meta))
        })
    });
}

criterion_group!(benches, benchmark_rule_routing);
criterion_main!(benches);
```

### Performance Targets

- Metadata creation: <1 microsecond
- Config parsing: <10 microseconds
- Token estimation: <100 nanoseconds
- Rule routing: <1 millisecond

Run `cargo bench` to verify current performance metrics.

---

## Code Quality

### Formatting

Octoroute uses `rustfmt` with default settings.

```bash
# Check formatting
cargo fmt --all -- --check

# Auto-format code
cargo fmt --all

# Or use just
just fmt
```

### Linting

Octoroute enforces zero clippy warnings.

```bash
# Run clippy
cargo clippy --all-targets --all-features

# Run clippy with auto-fix
cargo clippy --all-targets --all-features --fix

# Or use just
just clippy
just clippy-fix
```

**Clippy Configuration** (`clippy.toml`):

```toml
# Cognitive complexity threshold
cognitive-complexity-threshold = 30

# Disallowed names (prevent placeholder variables)
disallowed-names = ["foo", "bar", "baz", "tmp"]

# MSRV for clippy
msrv = "1.90"
```

### Documentation

All public APIs must be documented:

```bash
# Build documentation
cargo doc --no-deps

# Build and open documentation
cargo doc --no-deps --open

# Or use just
just doc
just doc-open
```

**Documentation Standards**:
- All public functions have doc comments
- Examples provided for complex APIs
- Panic conditions documented
- Safety requirements for `unsafe` code

### CI Checks

GitHub Actions runs these checks on all PRs:

1. **Format Check**: `cargo fmt --all -- --check`
2. **Clippy**: `cargo clippy --all-targets --all-features -- -D warnings`
3. **Tests**: `cargo test --all-features` (stable + MSRV)
4. **Benchmark Compilation**: `cargo bench --no-run`
5. **Documentation**: `cargo doc --all-features --no-deps`

**Local CI Check**:

```bash
# Run all CI checks locally
just ci
```

---

## Contributing Guidelines

### Workflow

1. **Fork the repository** on GitHub
2. **Create a feature branch**: `git checkout -b feature/my-feature`
3. **Make changes**: Write code, tests, and docs
4. **Run checks**: `just ci` (format, clippy, tests, bench, docs)
5. **Commit**: Use conventional commit messages
6. **Push**: `git push origin feature/my-feature`
7. **Create PR**: Open pull request on GitHub

### Commit Message Format

Use conventional commits:

```
type(scope): Brief description (max 72 chars)

Detailed explanation:
- What changed and why
- Architectural decisions
- Breaking changes if any

Testing:
- X tests added
- All Y tests passing
```

**Types**:
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation only
- `test`: Test additions or fixes
- `refactor`: Code refactoring
- `perf`: Performance improvement
- `chore`: Maintenance tasks

**Examples**:

```
feat(router): Add LLM-based routing fallback

Implemented LlmBasedRouter using balanced tier (30B) for routing
decisions. Hybrid router now falls back to LLM when no rule matches.

Testing:
- 15 tests added for LLM router
- All 234 tests passing
```

```
fix(health): Immediate recovery on successful requests

Health checker now calls mark_success() after every successful
endpoint query, enabling rapid recovery from transient failures.

Testing:
- 3 integration tests added
- All tests passing
```

### Code Review

Pull requests must:
- ✓ Pass all CI checks (format, clippy, tests, benchmarks)
- ✓ Add tests for new functionality
- ✓ Update documentation
- ✓ Follow code style guidelines
- ✓ Include clear commit messages

### Testing Requirements

**New Features**:
- Unit tests for core logic
- Integration tests for end-to-end behavior
- Benchmark if performance-critical

**Bug Fixes**:
- Regression test that fails without the fix
- Passes with the fix

**Refactoring**:
- Existing tests continue to pass
- No behavior changes

### Documentation Requirements

**Code Documentation**:
- Public APIs have doc comments
- Complex logic has inline comments
- Non-obvious decisions explained

**User Documentation**:
- Update relevant docs (API, configuration, etc.)
- Add examples for new features
- Update architecture docs if design changes

---

## Project Structure

Understanding the codebase:

```
octoroute/
├── src/
│   ├── main.rs                    # Axum server entrypoint
│   ├── lib.rs                     # Library root
│   │
│   ├── config.rs                  # Configuration (ModelConfig, RoutingConfig, etc.)
│   │
│   ├── router/                    # Routing strategies
│   │   ├── mod.rs                # Router enum, RouteMetadata, Importance, TaskType
│   │   ├── rule_based.rs         # Rule-based router
│   │   ├── llm_based.rs          # LLM-powered router
│   │   └── hybrid.rs             # Hybrid router
│   │
│   ├── models/                    # Model management
│   │   ├── mod.rs
│   │   ├── client.rs             # ModelClient wrapper
│   │   ├── selector/             # Model selection
│   │   ├── health.rs             # Health checking
│   │   └── endpoint_name.rs      # Type-safe endpoint IDs
│   │
│   ├── handlers/                  # HTTP handlers
│   │   ├── mod.rs
│   │   ├── chat.rs               # POST /chat
│   │   ├── health.rs             # GET /health
│   │   ├── models.rs             # GET /models
│   │   └── metrics.rs            # GET /metrics
│   │
│   ├── middleware/                # Axum middleware
│   │   ├── mod.rs
│   │   └── request_id.rs         # Request ID generation and propagation
│   │
│   ├── metrics.rs                 # Prometheus metrics
│   ├── error.rs                   # AppError types
│   └── telemetry.rs              # Tracing setup
│
├── tests/                         # Integration tests
│   ├── chat_integration.rs
│   ├── retry_logic.rs
│   ├── concurrent_routing.rs
│   ├── timeout_enforcement.rs
│   ├── stream_interruption.rs
│   ├── background_health_checks.rs
│   └── ... (19 integration test files total)
│
├── benches/                       # Benchmarks
│   └── routing.rs
│
├── docs/                          # Documentation
│   ├── architecture.md
│   ├── api-reference.md
│   ├── configuration.md
│   ├── observability.md
│   ├── development.md
│   └── deployment.md
│
├── Cargo.toml                     # Dependencies
├── rust-toolchain.toml            # Rust version pinning
├── clippy.toml                    # Clippy configuration
├── justfile                       # Task automation
└── README.md                      # Project overview
```

---

## Troubleshooting Development Issues

### Tests Failing

```bash
# Run with verbose output
cargo test -- --nocapture

# Run single test
cargo test test_name -- --nocapture

# Check for ignored tests
cargo test -- --ignored
```

### Clippy Warnings

```bash
# Show detailed warnings
cargo clippy --all-targets --all-features

# Auto-fix warnings
cargo clippy --all-targets --all-features --fix
```

### Build Errors

```bash
# Clean build
cargo clean
cargo build

# Update dependencies
cargo update
```

### Benchmark Errors

```bash
# Check benchmarks compile
cargo bench --no-run

# Run specific benchmark
cargo bench routing
```

---

## Release Process

1. **Update version** in `Cargo.toml`
2. **Update CHANGELOG.md** with release notes
3. **Run full CI**: `just ci`
4. **Create git tag**: `git tag v0.1.0`
5. **Push tag**: `git push origin v0.1.0`
6. **Create GitHub release** with release notes
7. **(Optional) Publish to crates.io**: `cargo publish`

---

## Getting Help

- **Issues**: [GitHub Issues](https://github.com/slb350/octoroute/issues)
- **Discussions**: [GitHub Discussions](https://github.com/slb350/octoroute/discussions)
- **Documentation**: See `/docs` directory

---

## License

Octoroute is licensed under the MIT License. See `LICENSE` file for details.
