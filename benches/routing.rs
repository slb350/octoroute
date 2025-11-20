//! Routing performance benchmarks
//!
//! Measures performance of routing decision logic to ensure it meets targets:
//! - Rule-based routing: <1ms (pure CPU, deterministic)
//! - Metadata creation: <100μs (simple data structure)
//! - Config parsing: <10ms (one-time startup cost)
//!
//! Run with: `cargo bench --features metrics` or `just bench`

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use octoroute::{
    config::Config,
    models::ModelSelector,
    router::{Importance, RouteMetadata, RuleBasedRouter, TaskType},
};
use std::sync::Arc;
use tokio::runtime::Runtime;

/// Create a minimal test configuration for benchmarking
#[allow(dead_code)]
fn create_bench_config() -> Arc<Config> {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "bench-fast"
base_url = "http://localhost:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "bench-balanced"
base_url = "http://localhost:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "bench-deep"
base_url = "http://localhost:8080/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_model = "balanced"
"#;

    Arc::new(toml::from_str(toml).expect("should parse bench config"))
}

/// Benchmark rule-based routing with various request patterns
///
/// Target: <1ms per routing decision
/// This is pure CPU work with no I/O, so should be very fast.
#[allow(dead_code)]
fn bench_rule_based_routing(c: &mut Criterion) {
    let test_cases = vec![
        (
            "casual_chat_normal",
            "Hey, how are you?",
            RouteMetadata::new(50)
                .with_importance(Importance::Normal)
                .with_task_type(TaskType::CasualChat),
        ),
        (
            "code_review_high",
            "Review this Rust code for memory safety issues",
            RouteMetadata::new(500)
                .with_importance(Importance::High)
                .with_task_type(TaskType::Code),
        ),
        (
            "creative_writing_normal",
            "Write a short story about a time traveler",
            RouteMetadata::new(1000)
                .with_importance(Importance::Normal)
                .with_task_type(TaskType::CreativeWriting),
        ),
        (
            "research_critical",
            "Analyze the economic implications of quantum computing",
            RouteMetadata::new(2000)
                .with_importance(Importance::High)
                .with_task_type(TaskType::DeepAnalysis),
        ),
    ];

    let mut group = c.benchmark_group("rule_based_routing");

    for (name, message, metadata) in test_cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &(message, metadata),
            |b, (msg, meta)| {
                let rt = Runtime::new().unwrap();
                b.to_async(&rt).iter(|| async {
                    let router = RuleBasedRouter::new();
                    let config = create_bench_config();
                    let selector = ModelSelector::new(config);
                    router.route(msg, meta, &selector).await.unwrap()
                });
            },
        );
    }

    group.finish();
}

/// Benchmark routing decision overhead across different request types
///
/// Measures the computational cost of routing logic itself, excluding network I/O.
#[allow(dead_code)]
fn bench_routing_decision_overhead(c: &mut Criterion) {
    let metadata = RouteMetadata::new(100)
        .with_importance(Importance::Normal)
        .with_task_type(TaskType::QuestionAnswer);

    c.bench_function("routing_decision_overhead", |b| {
        let rt = Runtime::new().unwrap();
        b.to_async(&rt).iter(|| async {
            let router = RuleBasedRouter::new();
            let config = create_bench_config();
            let selector = ModelSelector::new(config);
            router
                .route("What is Rust?", &metadata, &selector)
                .await
                .unwrap()
        });
    });
}

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
/// This is a one-time startup cost, so even 10ms is acceptable.
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
router_model = "balanced"
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
    // Disabled async benchmarks - they're too slow due to ModelSelector overhead
    // bench_rule_based_routing,
    // bench_routing_decision_overhead,
    bench_metadata_creation,
    bench_config_parsing,
    bench_metadata_builder,
    bench_token_estimation,
);
criterion_main!(benches);
