# Octoroute Development Tasks
# See https://github.com/casey/just for syntax reference

# Default recipe - show available commands
default:
    @just --list

# Run all tests (unit + integration)
test:
    @echo "Running all tests..."
    cargo test --all-features

# Run only unit tests (lib tests)
test-unit:
    @echo "Running unit tests..."
    cargo test --lib --all-features

# Run only integration tests
test-integration:
    @echo "Running integration tests..."
    cargo test --test '*' --all-features

# Run tests with nextest (if installed)
test-nextest:
    @echo "Running tests with nextest..."
    cargo nextest run --all-features

# Run benchmarks with criterion
bench:
    @echo "Running benchmarks..."
    cargo bench --all-features

# Run specific benchmark
bench-router:
    @echo "Running router benchmarks..."
    cargo bench --bench routing

# Start dev server with debug logging
run:
    @echo "Starting Octoroute server..."
    RUST_LOG=octoroute=debug,tower_http=debug cargo run

# Start server with specific config
run-config CONFIG:
    @echo "Starting server with config: {{CONFIG}}"
    RUST_LOG=octoroute=debug cargo run -- --config {{CONFIG}}

# Run clippy and format check (zero warnings policy)
check:
    @echo "Running clippy..."
    cargo clippy --all-targets --all-features -- -D warnings
    @echo "Checking formatting..."
    cargo fmt --all -- --check

# Format code
fmt:
    @echo "Formatting code..."
    cargo fmt --all

# Run clippy with auto-fix
clippy-fix:
    @echo "Running clippy with auto-fix..."
    cargo clippy --all-targets --all-features --fix --allow-dirty

# Build optimized release binary
build-release:
    @echo "Building release binary..."
    cargo build --release

# Build with metrics feature enabled
build-metrics:
    @echo "Building with metrics feature..."
    cargo build --release --features metrics

# Build all features
build-all:
    @echo "Building with all features..."
    cargo build --all-features

# Clean build artifacts
clean:
    @echo "Cleaning build artifacts..."
    cargo clean

# Check project (clippy + fmt + test)
ci: check test
    @echo "CI checks passed!"

# Full validation (clippy + fmt + test + bench)
validate: check test bench
    @echo "Full validation passed!"

# Watch tests (requires cargo-watch)
watch:
    @echo "Watching for changes..."
    cargo watch -x 'test --all-features'

# Generate documentation
docs:
    @echo "Generating documentation..."
    cargo doc --all-features --no-deps --open

# Show project statistics with tokei (if installed)
stats:
    @echo "Project statistics:"
    @tokei

# Run the example config checker
example-config:
    @echo "Validating example config..."
    cargo run --example config_validation
