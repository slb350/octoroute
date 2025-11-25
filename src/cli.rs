//! Command-line interface for Octoroute
//!
//! Provides argument parsing and subcommand handling for the Octoroute binary.

use clap::{Parser, Subcommand};

/// Intelligent multi-model router for self-hosted LLMs
#[derive(Parser)]
#[command(name = "octoroute")]
#[command(version)]
#[command(about = "Intelligent multi-model router for self-hosted LLMs")]
#[command(
    long_about = "Octoroute intelligently routes LLM requests to optimal model endpoints \
    based on task complexity, using rule-based, LLM-based, or hybrid routing strategies."
)]
pub struct Cli {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml", global = true)]
    pub config: String,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Generate a template configuration file
    Config {
        /// Output file path (prints to stdout if not specified)
        #[arg(short, long)]
        output: Option<String>,
    },
}

/// Generate template configuration content
pub fn generate_config_template() -> &'static str {
    r#"# Octoroute Configuration
# =========================
#
# This file configures the HTTP server, model endpoints, routing strategy,
# and observability settings for Octoroute.
#
# For full documentation, see: https://github.com/slb350/octoroute

# ─────────────────────────────────────────────────────────────────────────────
# SERVER CONFIGURATION
# ─────────────────────────────────────────────────────────────────────────────

[server]
# IP address to bind to (0.0.0.0 for all interfaces, 127.0.0.1 for localhost only)
host = "0.0.0.0"

# Port to listen on
port = 3000

# Default request timeout in seconds (can be overridden per-tier in [timeouts])
request_timeout_seconds = 30

# ─────────────────────────────────────────────────────────────────────────────
# MODEL TIERS
# ─────────────────────────────────────────────────────────────────────────────
#
# Configure endpoints for each model tier. Each tier can have multiple endpoints
# for load balancing and failover. Octoroute will route requests based on:
#
#   - FAST (8B models): Simple tasks, casual chat, quick Q&A
#   - BALANCED (30B models): Coding, analysis, explanations
#   - DEEP (120B+ models): Complex reasoning, creative writing, research
#
# Endpoint fields:
#   - name: Model identifier (for OpenAI-compatible APIs)
#   - base_url: API base URL (must end with /v1 for OpenAI-compatible APIs)
#   - max_tokens: Maximum tokens for generation
#   - temperature: Sampling temperature (0.0-2.0)
#   - weight: Load balancing weight (higher = more traffic)
#   - priority: Selection priority (higher = tried first)

# Fast tier - 8B class models
[[models.fast]]
name = "your-8b-model"
base_url = "http://your-server:port/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

# Add additional fast endpoints for load balancing:
# [[models.fast]]
# name = "your-8b-model"
# base_url = "http://another-server:port/v1"
# max_tokens = 4096
# temperature = 0.7
# weight = 1.0
# priority = 1

# Balanced tier - 30B class models
[[models.balanced]]
name = "your-30b-model"
base_url = "http://your-server:port/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

# Deep tier - 120B+ class models
[[models.deep]]
name = "your-120b-model"
base_url = "http://your-server:port/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1

# ─────────────────────────────────────────────────────────────────────────────
# ROUTING CONFIGURATION
# ─────────────────────────────────────────────────────────────────────────────

[routing]
# Routing strategy:
#   - "rule": Fast pattern-based routing (~<1ms latency)
#   - "llm": Intelligent LLM-powered routing (~250ms latency)
#   - "hybrid": Rule-based first, LLM fallback (recommended)
strategy = "hybrid"

# Default importance level for requests that don't specify one
# Options: "low", "normal", "high", "critical"
default_importance = "normal"

# Which tier to use for LLM-based routing decisions
# The router model analyzes requests and selects the optimal target tier
router_tier = "balanced"

# ─────────────────────────────────────────────────────────────────────────────
# OBSERVABILITY
# ─────────────────────────────────────────────────────────────────────────────

[observability]
# Log level: "trace", "debug", "info", "warn", "error"
log_level = "info"

# Prometheus metrics are always available at /metrics on the server port
# For production, consider using a reverse proxy to restrict access

# ─────────────────────────────────────────────────────────────────────────────
# TIMEOUTS (Optional)
# ─────────────────────────────────────────────────────────────────────────────
#
# Per-tier timeout overrides in seconds.
# If not specified, server.request_timeout_seconds is used.

[timeouts]
fast = 15       # 8B models respond quickly
balanced = 30   # 30B models - moderate speed
deep = 60       # 120B+ models need more time
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        // Clap's built-in verification for the CLI structure
        Cli::command().debug_assert();
    }

    #[test]
    fn default_config_path() {
        let cli = Cli::parse_from(["octoroute"]);
        assert_eq!(cli.config, "config.toml");
        assert!(cli.command.is_none());
    }

    #[test]
    fn custom_config_path() {
        let cli = Cli::parse_from(["octoroute", "--config", "custom.toml"]);
        assert_eq!(cli.config, "custom.toml");
    }

    #[test]
    fn config_subcommand() {
        let cli = Cli::parse_from(["octoroute", "config"]);
        assert!(matches!(
            cli.command,
            Some(Command::Config { output: None })
        ));
    }

    #[test]
    fn config_subcommand_with_output() {
        let cli = Cli::parse_from(["octoroute", "config", "-o", "my-config.toml"]);
        assert!(matches!(
            cli.command,
            Some(Command::Config { output: Some(ref path) }) if path == "my-config.toml"
        ));
    }

    #[test]
    fn template_is_valid_toml() {
        let template = generate_config_template();
        // Should parse without errors
        let result: Result<toml::Value, _> = toml::from_str(template);
        assert!(
            result.is_ok(),
            "Template should be valid TOML: {:?}",
            result.err()
        );
    }

    #[test]
    fn template_has_all_sections() {
        let template = generate_config_template();
        assert!(template.contains("[server]"));
        assert!(template.contains("[[models.fast]]"));
        assert!(template.contains("[[models.balanced]]"));
        assert!(template.contains("[[models.deep]]"));
        assert!(template.contains("[routing]"));
        assert!(template.contains("[observability]"));
        assert!(template.contains("[timeouts]"));
    }
}
