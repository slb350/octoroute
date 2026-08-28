//! Provider and route validation.

use super::fields::{
    invalid, validate_command, validate_env_name, validate_executable, validate_first_byte_timeout,
    validate_model, validate_name, validate_u64_range, validate_url, validate_usize_range,
};
use super::{
    DEFAULT_CODEX_EXECUTABLE, DEFAULT_PROVIDER_MAX_IN_FLIGHT, MAX_CONCURRENCY,
    MAX_PROVIDER_READINESS_TIMEOUT_MS, MAX_PROVIDER_READINESS_TTL_MS, MAX_UPSTREAM_TIMEOUT_MS,
    RawProviderConfig, RawVirtualRoute,
};
use crate::gateway::fabric::{
    FabricConfigError, LocalPoolConfig, ProviderConfig, ProviderCredentialConfig, ProviderKind,
    ProviderProfile, ProviderProtocol, ProviderRuntimeConfig, RoutePrivacy, RouteTarget,
    VirtualRoute,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn validate_providers(
    raw_providers: Vec<RawProviderConfig>,
) -> Result<BTreeMap<String, ProviderConfig>, FabricConfigError> {
    let mut providers = BTreeMap::new();
    for raw in raw_providers {
        validate_name("fabric.providers.name", &raw.name)?;
        validate_model("fabric.providers.model", &raw.model)?;
        let max_in_flight = raw.max_in_flight.unwrap_or(DEFAULT_PROVIDER_MAX_IN_FLIGHT);
        validate_usize_range(
            "fabric.providers.max_in_flight",
            max_in_flight,
            MAX_CONCURRENCY,
        )?;
        validate_u64_range(
            "fabric.providers.timeout_ms",
            raw.timeout_ms,
            MAX_UPSTREAM_TIMEOUT_MS,
        )?;
        validate_first_byte_timeout(
            "fabric.providers.first_byte_timeout_ms",
            raw.timeout_ms,
            raw.first_byte_timeout_ms,
        )?;
        validate_u64_range(
            "fabric.providers.readiness_ttl_ms",
            raw.readiness_ttl_ms,
            MAX_PROVIDER_READINESS_TTL_MS,
        )?;
        validate_u64_range(
            "fabric.providers.readiness_timeout_ms",
            raw.readiness_timeout_ms,
            MAX_PROVIDER_READINESS_TIMEOUT_MS,
        )?;
        if raw
            .temperature
            .is_some_and(|temperature| !temperature.is_finite())
        {
            return Err(invalid(
                "fabric.providers.temperature",
                "must be finite when configured",
            ));
        }
        if raw.max_tokens == Some(0) {
            return Err(invalid(
                "fabric.providers.max_tokens",
                "must be greater than zero when configured",
            ));
        }

        let runtime = match raw.kind {
            ProviderKind::Http => {
                if raw.executable.is_some() {
                    return Err(invalid(
                        "fabric.providers.executable",
                        "is accepted only for codex_cli providers",
                    ));
                }
                let endpoint = raw.endpoint.as_deref().ok_or_else(|| {
                    invalid(
                        "fabric.providers.endpoint",
                        "is required for an HTTP provider",
                    )
                })?;
                let protocol = raw.protocol.ok_or_else(|| {
                    invalid(
                        "fabric.providers.protocol",
                        "is required for an HTTP provider",
                    )
                })?;
                let has_env = raw.api_key_env.is_some();
                let has_command = raw.api_key_command.is_some();
                if has_env == has_command {
                    return Err(invalid(
                        "fabric.providers",
                        "HTTP providers require exactly one of api_key_env or api_key_command",
                    ));
                }
                if let Some(name) = raw.api_key_env.as_deref() {
                    validate_env_name("fabric.providers.api_key_env", name)?;
                }
                if let Some(command) = raw.api_key_command.as_deref() {
                    validate_command("fabric.providers.api_key_command", command)?;
                }
                if raw.profile == ProviderProfile::OpenRouterAuto
                    && protocol != ProviderProtocol::OpenAi
                {
                    return Err(invalid(
                        "fabric.providers.profile",
                        "openrouter_auto requires the OpenAI protocol",
                    ));
                }
                if protocol == ProviderProtocol::Anthropic && raw.max_tokens.is_none() {
                    return Err(invalid(
                        "fabric.providers.max_tokens",
                        "is required for Anthropic-compatible providers",
                    ));
                }
                let credential = match (raw.api_key_env, raw.api_key_command) {
                    (Some(name), None) => ProviderCredentialConfig::Environment(name),
                    (None, Some(command)) => ProviderCredentialConfig::Command(command),
                    // The `has_env == has_command` check above already rejected
                    // both and neither.
                    _ => unreachable!("exactly one credential source was validated"),
                };
                ProviderRuntimeConfig::Http {
                    endpoint: validate_url("fabric.providers.endpoint", endpoint, true)?,
                    protocol,
                    credential,
                }
            }
            ProviderKind::CodexCli => {
                if raw.endpoint.is_some()
                    || raw.protocol.is_some()
                    || raw.api_key_env.is_some()
                    || raw.api_key_command.is_some()
                    || raw.temperature.is_some()
                    || raw.max_tokens.is_some()
                    || raw.profile != ProviderProfile::Passthrough
                {
                    return Err(invalid(
                        "fabric.providers",
                        "codex_cli does not accept endpoint, protocol, credential, sampling, token, or profile fields",
                    ));
                }
                validate_executable(
                    "fabric.providers.executable",
                    raw.executable
                        .as_deref()
                        .unwrap_or(DEFAULT_CODEX_EXECUTABLE),
                )?;
                ProviderRuntimeConfig::CodexCli {
                    executable: raw
                        .executable
                        .unwrap_or_else(|| DEFAULT_CODEX_EXECUTABLE.to_string()),
                }
            }
        };

        let name = raw.name.clone();
        let provider = ProviderConfig {
            name: raw.name,
            enabled: raw.enabled,
            runtime,
            model: raw.model,
            max_in_flight,
            timeout_ms: raw.timeout_ms,
            readiness_ttl_ms: raw.readiness_ttl_ms,
            readiness_timeout_ms: raw.readiness_timeout_ms,
            first_byte_timeout_ms: raw.first_byte_timeout_ms,
            reasoning_effort: raw.reasoning_effort,
            temperature: raw.temperature,
            max_tokens: raw.max_tokens,
            profile: raw.profile,
        };
        if providers.insert(name.clone(), provider).is_some() {
            return Err(invalid(
                "fabric.providers.name",
                format!("duplicate provider `{name}`"),
            ));
        }
    }
    Ok(providers)
}

pub(super) fn validate_routes(
    raw_routes: Vec<RawVirtualRoute>,
    pools: &BTreeMap<String, LocalPoolConfig>,
    providers: &BTreeMap<String, ProviderConfig>,
) -> Result<BTreeMap<String, VirtualRoute>, FabricConfigError> {
    let mut routes = BTreeMap::new();
    for raw in raw_routes {
        validate_name("routing.routes.model", &raw.model)?;
        if raw.model == "auto" {
            return Err(invalid(
                "routing.routes.model",
                "`auto` is reserved for the configured default-model alias",
            ));
        }
        if raw.steps.is_empty() {
            return Err(invalid(
                "routing.routes.steps",
                "must include at least one target",
            ));
        }
        // Parse before anything else inspects a step. A raw step is unvalidated
        // operator text that may carry a pasted credential or a newline, and
        // `RouteTarget::from_str` is the one place that never interpolates it.
        // Everything below reports a target by its parsed halves: a static kind
        // and a name that has passed `validate_name`.
        let steps = raw
            .steps
            .iter()
            .map(|step| step.parse::<RouteTarget>())
            .collect::<Result<Vec<_>, _>>()?;

        let mut saw_provider = false;
        for step in &steps {
            match step {
                RouteTarget::LocalPool(name) => {
                    if !pools.contains_key(name) {
                        return Err(invalid(
                            "routing.routes.steps",
                            format!("unknown local pool `{name}`"),
                        ));
                    }
                    if saw_provider {
                        return Err(invalid(
                            "routing.routes.steps",
                            "local pools must precede providers in a route",
                        ));
                    }
                    if raw.privacy == RoutePrivacy::CloudOnly {
                        return Err(invalid(
                            "routing.routes.privacy",
                            "cloud_only routes cannot reference local pools",
                        ));
                    }
                }
                RouteTarget::Provider(name) => {
                    saw_provider = true;
                    if !providers.contains_key(name) {
                        return Err(invalid(
                            "routing.routes.steps",
                            format!("unknown provider `{name}`"),
                        ));
                    }
                    if raw.privacy == RoutePrivacy::LocalOnly {
                        return Err(invalid(
                            "routing.routes.privacy",
                            "local_only routes cannot reference providers",
                        ));
                    }
                }
            }
        }

        // After the cross-reference checks, so a repeated step that names
        // nothing reports the missing pool or provider - the actionable fault -
        // rather than the repetition of it.
        let mut seen = BTreeSet::new();
        for step in &steps {
            let (kind, name) = match step {
                RouteTarget::LocalPool(name) => ("pool", name),
                RouteTarget::Provider(name) => ("provider", name),
            };
            if !seen.insert((kind, name)) {
                return Err(invalid(
                    "routing.routes.steps",
                    format!("duplicate target `{kind}:{name}`"),
                ));
            }
        }

        let model = raw.model.clone();
        let route = VirtualRoute {
            model: raw.model,
            privacy: raw.privacy,
            steps,
            default_reasoning_effort: raw.default_reasoning_effort,
            fallback_on: raw.fallback_on,
        };
        if routes.insert(model.clone(), route).is_some() {
            return Err(invalid(
                "routing.routes.model",
                format!("duplicate virtual model `{model}`"),
            ));
        }
    }
    Ok(routes)
}
