//! Built-in starting points for cloud APIs and subscription-backed providers.
//!
//! These values mirror the provider contracts already exercised by Drep. They are
//! configuration defaults, not a replacement for live provider/model discovery.

use super::{ProviderKind, ProviderProfile, ProviderProtocol, ReasoningEffort};

/// Static provider defaults suitable for rendering into v3 configuration.
#[derive(Debug, Clone, Copy)]
pub struct ProviderPreset {
    pub key: &'static str,
    pub kind: ProviderKind,
    pub endpoint: Option<&'static str>,
    pub protocol: Option<ProviderProtocol>,
    pub model: &'static str,
    pub api_key_env: Option<&'static str>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub profile: ProviderProfile,
}

/// Named starting points for the initial v3 cloud tier.
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        key: "openrouter",
        kind: ProviderKind::Http,
        endpoint: Some("https://openrouter.ai/api/v1"),
        protocol: Some(ProviderProtocol::OpenAi),
        model: "openrouter/auto",
        api_key_env: Some("OPENROUTER_API_KEY"),
        reasoning_effort: None,
        temperature: Some(0.2),
        max_tokens: None,
        profile: ProviderProfile::OpenRouterAuto,
    },
    ProviderPreset {
        key: "zai",
        kind: ProviderKind::Http,
        endpoint: Some("https://api.z.ai/api/coding/paas/v4"),
        protocol: Some(ProviderProtocol::OpenAi),
        model: "glm-5.3",
        api_key_env: Some("ZAI_API_KEY"),
        reasoning_effort: None,
        temperature: Some(0.2),
        max_tokens: None,
        profile: ProviderProfile::Passthrough,
    },
    ProviderPreset {
        key: "kimi",
        kind: ProviderKind::Http,
        endpoint: Some("https://api.kimi.com/coding/v1"),
        protocol: Some(ProviderProtocol::Anthropic),
        model: "k3",
        api_key_env: Some("KIMI_API_KEY"),
        reasoning_effort: None,
        temperature: None,
        max_tokens: Some(200_000),
        profile: ProviderProfile::Passthrough,
    },
    ProviderPreset {
        key: "openai",
        kind: ProviderKind::Http,
        endpoint: Some("https://api.openai.com/v1"),
        protocol: Some(ProviderProtocol::OpenAi),
        model: "gpt-5.6-sol",
        api_key_env: Some("OPENAI_API_KEY"),
        reasoning_effort: Some(ReasoningEffort::Xhigh),
        temperature: None,
        max_tokens: None,
        profile: ProviderProfile::Passthrough,
    },
    ProviderPreset {
        key: "codex",
        kind: ProviderKind::CodexCli,
        endpoint: None,
        protocol: None,
        model: "gpt-5.6-sol",
        api_key_env: None,
        reasoning_effort: Some(ReasoningEffort::Xhigh),
        temperature: None,
        max_tokens: None,
        profile: ProviderProfile::Passthrough,
    },
];

/// Look up a built-in provider preset by stable key.
pub fn provider_preset(key: &str) -> Option<&'static ProviderPreset> {
    PROVIDER_PRESETS.iter().find(|preset| preset.key == key)
}
