# Octoroute Development Tasks
# See https://github.com/casey/just for syntax reference

# Default recipe - show available commands
default:
    @just --list

# Run all tests (unit + integration)
test:
    @echo "Running all tests..."
    cargo test --locked --all-targets --all-features

# Run only unit tests (lib tests)
test-unit:
    @echo "Running unit tests..."
    cargo test --locked --lib --all-features

# Run only integration tests
test-integration:
    @echo "Running integration tests..."
    cargo test --locked --test cli_config_command --test gateway_v2

# Run tests with nextest (if installed)
test-nextest:
    @echo "Running tests with nextest..."
    cargo nextest run --locked --all-features

# Start dev server with debug logging
run:
    @echo "Starting Octoroute server..."
    RUST_LOG=octoroute=debug cargo run --locked

# Start server with specific config
run-config CONFIG:
    @echo "Starting server with config: {{CONFIG}}"
    RUST_LOG=octoroute=debug cargo run --locked -- --config {{CONFIG}}

# Run clippy and format check (zero warnings policy)
check:
    @echo "Running clippy..."
    cargo clippy --locked --all-targets --all-features -- -D warnings
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

# Build optimized release binary (metrics always enabled)
build-release:
    @echo "Building release binary..."
    cargo build --locked --release

# Build all features
build-all:
    @echo "Building with all features..."
    cargo build --locked --all-features

# Clean build artifacts
clean:
    @echo "Cleaning build artifacts..."
    cargo clean

# Check project (clippy + fmt + test)
ci: check test
    @echo "CI checks passed!"

# Audit locked dependencies against RustSec
audit:
    cargo audit

# Full validation
validate: check test audit docs
    @echo "Full validation passed!"

# Watch tests (requires cargo-watch)
watch:
    @echo "Watching for changes..."
    cargo watch -x 'test --all-features'

# Generate documentation
docs:
    @echo "Generating documentation..."
    cargo doc --locked --all-features --no-deps

# Show project statistics with tokei (if installed)
stats:
    @echo "Project statistics:"
    @tokei
