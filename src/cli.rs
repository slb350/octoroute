//! Command-line interface for Octoroute.
//!
//! Provides argument parsing and subcommand handling for the Octoroute binary.

use clap::{Parser, Subcommand};

/// Local-first OpenAI-compatible gateway with OpenRouter cloud fallback.
#[derive(Parser)]
#[command(name = "octoroute")]
#[command(version)]
#[command(about = "Local-first OpenAI-compatible LLM gateway")]
#[command(
    long_about = "Octoroute intelligently routes work a local llama.cpp model can handle well \
    to that model and sends harder work to OpenRouter Auto, while preserving OpenAI \
    chat-completion schemas."
)]
pub struct Cli {
    /// Path to configuration file.
    #[arg(short, long, default_value = "config.toml", global = true)]
    pub config: String,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Generate a template configuration file.
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
        assert!(template.contains("config_version = 2"));
        assert!(template.contains("[server]"));
        assert!(template.contains("[upstreams.local]"));
        assert!(template.contains("[upstreams.openrouter]"));
        assert!(template.contains("[routing]"));
        assert!(template.contains("[observability]"));
    }

    #[test]
    fn template_deserializes_to_valid_config() {
        use crate::gateway::config::{Environment, GatewayConfig};

        struct TemplateEnvironment;

        impl Environment for TemplateEnvironment {
            fn get(&self, name: &str) -> Option<String> {
                match name {
                    "OCTOROUTE_API_KEY" | "OPENROUTER_API_KEY" => Some("test-secret".to_string()),
                    _ => None,
                }
            }
        }

        let template = generate_config_template();
        let config = GatewayConfig::from_toml(template, &TemplateEnvironment)
            .expect("template must be a valid v2 config");
        assert_eq!(config.local().model(), "strixtea");
        assert_eq!(config.openrouter().auto_model(), "openrouter/auto");
        assert_eq!(
            config.routing().semantic_mode(),
            crate::gateway::config::SemanticRoutingMode::Shadow
        );
    }
}
