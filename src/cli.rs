//! Command-line interface for Octoroute.

use clap::{Parser, Subcommand};

/// OpenAI-compatible inference fabric for local pools and configured providers.
#[derive(Parser)]
#[command(name = "octoroute")]
#[command(version)]
#[command(about = "OpenAI-compatible tiered inference fabric")]
#[command(
    long_about = "Octoroute exposes stable virtual models backed by local llama.cpp pools and \
    ordered provider chains while preserving OpenAI chat-completion schemas and strict \
    local-only privacy."
)]
pub struct Cli {
    /// Path to the v3 configuration file.
    #[arg(short, long, default_value = "config.toml", global = true)]
    pub config: String,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Generate a v3 template configuration file.
    Config {
        /// Output file path (prints to stdout if not specified).
        #[arg(short, long)]
        output: Option<String>,
    },
}

/// Generate template configuration content.
pub fn generate_config_template() -> &'static str {
    include_str!("../config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::fabric::FabricConfig;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
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
    fn template_is_the_valid_v3_schema() {
        let template = generate_config_template();
        let config = FabricConfig::from_toml(template).expect("valid v3 template");

        assert!(template.contains("config_version = 3"));
        assert!(template.contains("[[fabric.local_pools]]"));
        assert!(template.contains("[[fabric.providers]]"));
        assert!(template.contains("[[routing.routes]]"));
        assert!(template.contains("api_key_env = \"OCTOROUTE_API_KEY\""));
        assert!(template.contains("api_key_env = \"OPENROUTER_API_KEY\""));
        assert!(!template.contains("sk-or-"));
        assert_eq!(config.server.port, 8081);
        assert_eq!(config.default_model, "auto-route");
        assert_eq!(config.local_pools["workers"].model, "coding-worker-model");
        assert_eq!(
            config.local_pools["workers"].model_revision,
            "example-worker-revision"
        );
        assert_eq!(config.local_pools["workers"].members.len(), 3);
        assert_eq!(config.providers["openrouter"].model, "openrouter/auto");
    }
}
