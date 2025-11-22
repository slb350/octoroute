//! Routing performance benchmarks
//!
//! Measures performance of non-I/O routing logic components (excludes network calls).
//!
//! ## Expected Performance Characteristics
//!
//! - Metadata creation: Sub-microsecond (typically 0.5-1.5μs, builder pattern overhead)
//! - Config parsing: Single-digit microseconds (one-time startup cost, acceptable up to 100μs)
//! - Token estimation: Single-digit nanoseconds (simple character counting, highly optimized)
//!
//! **Note**: Actual measurements vary with compiler version, CPU architecture, and system load.
//! Run `cargo bench --features metrics` or `just bench` to measure on your system.
//!
//! Run with: `cargo bench --features metrics` or `just bench`

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use octoroute::{
    config::Config,
    router::{Importance, RouteMetadata, TaskType},
};

/// Benchmark metadata creation using builder pattern
///
/// Measures the cost of constructing RouteMetadata with various configurations.
fn bench_metadata_creation(c: &mut Criterion) {
    let test_cases = vec![
        (
            "minimal",
            (100, Importance::Normal, TaskType::QuestionAnswer),
        ),
        (
            "with_high_importance",
            (500, Importance::High, TaskType::Code),
        ),
        (
            "with_creative",
            (1000, Importance::Normal, TaskType::CreativeWriting),
        ),
        (
            "with_deep_analysis",
            (2000, Importance::High, TaskType::DeepAnalysis),
        ),
    ];

    let mut group = c.benchmark_group("metadata_creation");

    for (name, (tokens, importance, task_type)) in test_cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &(tokens, importance, task_type),
            |b, (t, i, tt)| {
                b.iter(|| {
                    RouteMetadata::new(*t)
                        .with_importance(*i)
                        .with_task_type(*tt)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark configuration parsing and validation
///
/// Measures the cost of loading and validating configuration.
/// This operation is called ONCE during server startup, so even 10ms is acceptable.
/// Typical range: 5-20μs depending on config size and validation complexity.
fn bench_config_parsing(c: &mut Criterion) {
    let toml_str = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:11434/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1234/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:8080/v1"
max_tokens = 16384

[routing]
strategy = "rule"
"#;

    c.bench_function("config_parsing", |b| {
        b.iter(|| {
            let config: Config = toml::from_str(toml_str).unwrap();
            // Note: validate() is called automatically by Config::from_file()
            // Here we're just benchmarking parsing
            config
        });
    });
}

/// Benchmark RouteMetadata builder pattern
///
/// Measures the cost of constructing metadata with the builder API.
fn bench_metadata_builder(c: &mut Criterion) {
    c.bench_function("metadata_builder", |b| {
        b.iter(|| {
            RouteMetadata::new(500)
                .with_importance(Importance::High)
                .with_task_type(TaskType::Code)
        });
    });
}

/// Benchmark token estimation heuristic
///
/// Measures the cost of the simple token counting algorithm.
fn bench_token_estimation(c: &mut Criterion) {
    let prompts = vec![
        ("short", "What is Rust?"),
        (
            "medium",
            "Explain how ownership and borrowing work in Rust, and why they prevent data races at compile time.",
        ),
        (
            "long",
            "Write a comprehensive tutorial on async programming in Rust, covering futures, tokio, async/await syntax, pinning, and common patterns. Include code examples and explain the relationship between Future, Poll, and Waker.",
        ),
    ];

    let mut group = c.benchmark_group("token_estimation");

    for (name, prompt) in prompts {
        group.bench_with_input(BenchmarkId::from_parameter(name), &prompt, |b, p| {
            b.iter(|| RouteMetadata::estimate_tokens(p));
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_metadata_creation,
    bench_config_parsing,
    bench_metadata_builder,
    bench_token_estimation,
);
criterion_main!(benches);
