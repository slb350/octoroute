# Octoroute 🦑

**Intelligent multi-model router for self-hosted LLMs**

[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Octoroute is a smart HTTP API that sits between your applications and your homelab's fleet of local LLMs. It automatically routes requests to the optimal model (8B, 30B, or 120B) based on task complexity, reducing compute costs while maintaining quality.

Think of it as a load balancer, but instead of distributing requests evenly, it sends simple queries to small models and complex reasoning tasks to larger ones.

---

## Why Octoroute?

Running multiple LLM sizes on your homelab is powerful, but routing requests manually is tedious:

- **Manual routing is error-prone**: You always use the 120B model "just in case," wasting compute.
- **Simple heuristics aren't enough**: "Short prompts → small model" misses nuance.
- **LangChain is Python-only**: You want native Rust performance and type safety.

**Octoroute solves this with:**

✅ **Intelligent routing** - Rule-based + LLM-powered decision making
✅ **Zero-cost rules** - Fast pattern matching for obvious cases (<1ms)
✅ **Homelab-first** - Built for local Ollama, LM Studio, llama.cpp deployments
✅ **Rust native** - Type-safe, async, low overhead
✅ **Observable** - Track every routing decision with structured logs

---

## Quick Start

### Prerequisites

- Rust 1.85+ (Edition 2024)
- At least one local LLM endpoint (Ollama, LM Studio, llama.cpp, etc.)
- Optional: Multiple model sizes (8B, 30B, 120B) for intelligent routing

### Installation

```bash
# Clone the repository
git clone https://github.com/slb350/octoroute.git
cd octoroute

# Build the project
cargo build --release

# Run the server
./target/release/octoroute
```

### Configuration

Create a `config.toml` in the project root:

```toml
[server]
host = "0.0.0.0"
port = 3000

[models.fast]
name = "qwen3-8b-instruct"
base_url = "http://localhost:11434/v1"  # Ollama
max_tokens = 4096

[models.balanced]
name = "qwen3-30b-instruct"
base_url = "http://localhost:1234/v1"   # LM Studio
max_tokens = 8192

[models.deep]
name = "gpt-oss-120b"
base_url = "http://localhost:8080/v1"   # llama.cpp
max_tokens = 16384

[routing]
strategy = "hybrid"  # rule, llm, hybrid, tool
```

### Usage

Send a chat request:

```bash
curl -X POST http://localhost:3000/chat \
  -H "Content-Type: application/json" \
  -d '{
    "prompt": "Explain quantum computing in simple terms",
    "importance": "normal",
    "task_type": "question_answer"
  }'
```

Response:

```json
{
  "response": "Quantum computing is...",
  "model_used": "balanced_30b",
  "routing_strategy": "rule",
  "processing_time_ms": 1234
}
```

---

## How It Works

### Routing Strategies

Octoroute supports **three routing strategies**:

#### 1. Rule-Based (Fast)

Pattern matching on request metadata:

- **Casual chat** + **<256 tokens** → 8B model
- **Deep analysis** or **high importance** → 120B model
- Everything else → 30B model

**Latency**: <1ms (no LLM overhead)

#### 2. LLM-Based (Intelligent)

Uses a 30B "router brain" to analyze the request and choose the optimal model.

**Latency**: ~100-500ms (router invocation)

#### 3. Hybrid (Recommended)

Try rules first (fast path), fall back to LLM for ambiguous cases.

**Latency**: <1ms for rule matches, ~100-500ms for LLM fallback

---

## Observability

Octoroute provides three levels of observability to help you understand routing decisions and system performance:

### Level 1: Structured Logs (Always Available)

Built-in structured logging via `tracing`:

```bash
# Set log level via environment variable
RUST_LOG=info cargo run

# Available levels: trace, debug, info, warn, error
RUST_LOG=octoroute=debug cargo run
```

**What you get:**
- Request metadata (prompt length, importance, task type)
- Routing decisions (which strategy was used, which model was selected)
- Health check status updates
- Error traces with full context

### Level 2: Metrics (Optional - Prometheus Export)

Enable with the `metrics` feature flag for time-series metrics:

```bash
# Build with metrics support
cargo build --release --features metrics

# Run and expose /metrics endpoint
./target/release/octoroute
```

**Available metrics:**
- `octoroute_requests_total{tier, strategy}` - Request counts by tier (fast/balanced/deep) and routing strategy (rule/llm)
- `octoroute_routing_duration_ms{strategy}` - Routing decision latency histogram (buckets: 0.1ms to 1000ms)
- `octoroute_model_invocations_total{tier}` - Model invocations by tier

**Prometheus scraping config:**

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'octoroute'
    static_configs:
      - targets: ['localhost:3000']
    metrics_path: '/metrics'
    scrape_interval: 15s
```

**Why Direct Prometheus?** We use the `prometheus` crate directly for simplicity and homelab-friendliness:
- Works with existing Prometheus/Grafana setups out of the box
- No intermediate abstraction layers - just Prometheus
- Mature, stable crate with broad ecosystem support
- Future-proof: OpenTelemetry traces can be added separately if needed for distributed tracing

### Level 3: Distributed Traces (Future)

Coming in a future release: Full request tracing showing routing flow, retries, and model execution timing.

---

## Architecture

```
┌─────────────────┐
│ Your App        │
└────────┬────────┘
         │ HTTP POST /chat
         ▼
┌─────────────────────────────────┐
│ Octoroute API (Axum + Tokio)   │
│  ┌────────────────────────────┐ │
│  │ Router (Rule/LLM/Hybrid)   │ │
│  └──────────┬─────────────────┘ │
│             │                   │
│             ▼ Model Selection   │
│  ┌────────────────────────────┐ │
│  │ open-agent-sdk Client      │ │
│  └──────────┬─────────────────┘ │
└─────────────┼───────────────────┘
              │
              ▼
┌──────────────────────────────────┐
│ Local Model Servers              │
│  8B (Ollama) | 30B (LM Studio)  │
│  120B (llama.cpp)                │
└──────────────────────────────────┘
```

Built on:
- **[open-agent-sdk](https://github.com/slb350/open-agent-sdk-rust)**: Rust SDK for local LLM orchestration
- **[Axum](https://github.com/tokio-rs/axum)**: Ergonomic web framework
- **[Tokio](https://tokio.rs)**: Async runtime

---

## API Reference

### `POST /chat`

Submit a chat request for intelligent routing.

**Request**:

```json
{
  "prompt": "Your question or task",
  "importance": "low" | "normal" | "high",
  "task_type": "casual_chat" | "code" | "creative_writing" | "deep_analysis" | "document_summary" | "question_answer",
  "model": null | "fast_8b" | "balanced_30b" | "deep_120b",
  "temperature": 0.7
}
```

**Response**:

```json
{
  "response": "Generated text",
  "model_used": "balanced_30b",
  "routing_strategy": "rule",
  "token_count": 1234,
  "processing_time_ms": 567
}
```

### `GET /health`

Health check endpoint.

**Response**: `200 OK` with body `"OK"`

### `GET /models`

List available models and their status.

**Response**:

```json
{
  "models": [
    {
      "name": "fast_8b",
      "endpoint": "http://localhost:11434",
      "healthy": true
    }
  ]
}
```

---

## Configuration Reference

See `config.toml.example` for full configuration options:

- **Server settings**: Host, port, timeouts
- **Model endpoints**: Names, URLs, token limits
- **Routing strategy**: Rule, LLM, hybrid, or tool-based
- **Observability**: Log level, metrics

---

## Development

### Prerequisites

```bash
# Install Rust toolchain
rustup toolchain install stable
rustup component add rustfmt clippy

# Install development tools
cargo install just cargo-nextest
```

### Build

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# Build with metrics support (Prometheus)
cargo build --release --features metrics
```

### Test

```bash
# Run all tests
cargo test

# Run with nextest (faster)
cargo nextest run

# Run integration tests
cargo test --test integration
```

### Format & Lint

```bash
# Format code
cargo fmt

# Lint with clippy
cargo clippy --all-targets --all-features -- -D warnings

# Or use justfile
just check
```

### Run locally

```bash
# With cargo
cargo run

# Or use release binary
./target/release/octoroute

# With environment variables
RUST_LOG=debug cargo run
```

---

## Project Status

**Current Phase**: Phase 5 Complete - Production Ready! 🚀

**Roadmap**:

- [x] Project setup and design
- [x] Phase 1: Rule-based router + HTTP server
- [x] Phase 2a: Model integration with `open-agent-sdk` (round-robin selection)
- [x] Phase 2b: Weighted load balancing
- [x] Phase 2c: Priority-based selection with health checking
- [x] Phase 3: LLM-based hybrid routing
- [x] Phase 5: Production Polish & Observability
- [ ] Phase 4: Tool-based routing (experimental - future)

**Features implemented**:
- ✅ HTTP API with `/chat`, `/health`, `/models`, `/metrics` endpoints
- ✅ Multi-tier model selection (fast/balanced/deep)
- ✅ Rule-based + LLM-based hybrid routing
- ✅ Priority-based routing with weighted distribution
- ✅ Health checking with automatic endpoint recovery
- ✅ Retry logic with request-scoped exclusion
- ✅ Timeout enforcement (global + per-tier overrides)
- ✅ Prometheus metrics (optional, behind `metrics` feature)
- ✅ Performance benchmarks (Criterion)
- ✅ CI/CD pipeline (GitHub Actions)
- ✅ Comprehensive config validation
- ✅ Development tooling (justfile with 20+ recipes)
- ✅ **270 tests passing** (203 lib + 67 integration)
- ✅ **Zero clippy warnings**
- ✅ **Zero tech debt**

---

## Use Cases

### 1. CLI Assistant with Cost Optimization

Route simple commands to 8B, complex reasoning to 120B:

```python
import requests

def ask_llm(prompt, importance="normal"):
    response = requests.post("http://localhost:3000/chat", json={
        "prompt": prompt,
        "importance": importance
    })
    return response.json()["response"]

# Uses 8B model (fast)
ask_llm("What's the weather like?")

# Uses 120B model (intelligent routing)
ask_llm("Design a distributed consensus algorithm", importance="high")
```

### 2. Multi-User Homelab Server

Share your LLM fleet with family/friends, automatically balancing load:

- Bob's casual question → 8B
- Alice's code review → 30B
- Charlie's essay writing → 120B

### 3. Development Workflow Automation

Integrate with IDE/scripts to route tasks intelligently:

```bash
# Quick code explanation (8B)
curl -X POST http://localhost:3000/chat -d '{"prompt":"Explain this function"}'

# Deep code review (120B)
curl -X POST http://localhost:3000/chat -d '{"prompt":"Review for security issues", "importance":"high"}'
```

---

## Performance

**Routing latency** (tested on M2 Mac):

| Strategy | Latency | Notes |
|----------|---------|-------|
| Rule-based | <1ms | Pure CPU, no LLM |
| LLM-based | ~250ms | With 30B router model |
| Hybrid | <1ms (rule hit) | Best of both worlds |

**Throughput**: Limited by model inference, not routing overhead.

---

## Contributing

Contributions welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

**Areas for contribution**:

- Additional routing strategies (e.g., RL-based)
- Streaming response support (SSE/WebSocket)
- Caching layer for repeated prompts
- Web UI for routing visualization
- More comprehensive benchmarks
- LLM-based routing implementation

---

## FAQ

### Q: Why not just use LangChain?

**A**: LangChain is Python-only and has significant overhead. Octoroute is Rust-native, type-safe, and designed specifically for local/self-hosted LLMs with minimal latency.

### Q: Can I use this with cloud APIs (OpenAI, Anthropic)?

**A**: Technically yes (they're OpenAI-compatible), but Octoroute is optimized for local deployments. Cloud APIs already handle routing internally.

### Q: What models are supported?

**A**: Any OpenAI-compatible endpoint (Ollama, LM Studio, llama.cpp, vLLM, etc.). Tested with Qwen, Llama, Mistral families.

### Q: Does this support streaming responses?

**A**: Not in v0.1, but planned for future releases. Currently accumulates full response before returning.

### Q: How does LLM-based routing work?

**A**: A 30B model analyzes your prompt + metadata and outputs one of: `FAST_8B`, `BALANCED_30B`, `DEEP_120B`. This decision is then used to route the actual request.

### Q: How do I monitor Octoroute in production?

**A**: Octoroute provides three observability levels:
1. **Structured logs** (always enabled): Use `RUST_LOG=info` to see routing decisions and health status
2. **Metrics** (optional): Build with `--features metrics` to expose Prometheus metrics at `/metrics`
3. **Traces** (future): Distributed tracing showing full request flow (coming soon)

For homelab deployments, we recommend Prometheus + Grafana for metrics visualization.

### Q: Why direct Prometheus instead of OpenTelemetry?

**A**: We chose the direct `prometheus` crate (v0.14) for simplicity and homelab-friendliness:
- **Simplicity**: No intermediate abstraction layers - just Prometheus
- **Homelab-friendly**: Works with existing Prometheus/Grafana setups out of the box, no OTEL collector required
- **Stability**: Mature, actively maintained library
- **Future-proof**: OpenTelemetry traces can be added separately if needed for distributed tracing

The `/metrics` endpoint works with your existing Prometheus scraper without any additional infrastructure.

---

## License

MIT License - see [LICENSE](LICENSE) for details.

---

## Acknowledgments

- Built on top of [open-agent-sdk-rust](https://github.com/slb350/open-agent-sdk-rust)
- Inspired by LangChain's router chains
- Thanks to the Rust, Tokio, and Axum communities

---

**Made with 🦑 for homelab enthusiasts**

*Route smarter, compute less.*
