//! Shared field validators and safe configuration error construction.

use super::{
    MAX_COMMAND_ARGUMENT_BYTES, MAX_COMMAND_ARGUMENTS, MAX_COMMAND_BYTES, MAX_ENV_NAME_BYTES,
    MAX_MODEL_BYTES,
};
use crate::gateway::fabric::FabricConfigError;
use reqwest::Url;
use std::net::IpAddr;

pub(super) fn validate_url(
    field: &str,
    value: &str,
    https_only: bool,
) -> Result<Url, FabricConfigError> {
    let mut url = Url::parse(value).map_err(|_| invalid(field, "must be an absolute URL"))?;
    let valid_scheme = if https_only {
        url.scheme() == "https"
    } else {
        matches!(url.scheme(), "http" | "https")
    };
    if !valid_scheme || url.host_str().is_none() {
        return Err(invalid(
            field,
            if https_only {
                "must be an absolute HTTPS URL"
            } else {
                "must be an absolute HTTP or HTTPS URL"
            },
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            field,
            "must not include credentials, query, or fragment",
        ));
    }
    if !url.path().ends_with('/') {
        let normalized = format!("{}/", url.path());
        url.set_path(&normalized);
    }
    Ok(url)
}

/// Reject a local member that is not on loopback, a private range, or `.local`.
///
/// `X-Octoroute-Privacy: local-only` promises the request does not leave the
/// operator's trust boundary. Nothing else enforces that promise about a member
/// address, so a member on a public address would satisfy `local-only` while
/// sending prompts to the internet in plaintext.
pub(super) fn validate_local_member_url(field: &str, url: &Url) -> Result<(), FabricConfigError> {
    let host = url
        .host_str()
        .ok_or_else(|| invalid(field, "must name a host"))?;
    // A URL renders an IPv6 host bracketed; the address itself is inside.
    let literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'));
    let trusted = match literal.unwrap_or(host).parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        // Loopback, a unique local address (fc00::/7), or a link-local address
        // (fe80::/10). Link-local is trusted in both families or in neither:
        // 169.254/16 and fe80::/10 are the same class of address, unroutable
        // past the local link, so a member on one is no more exposed than a
        // member on the other.
        Ok(IpAddr::V6(address)) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
        Err(_) => {
            let domain = host.to_ascii_lowercase();
            domain == "localhost"
                || domain.ends_with(".localhost")
                || domain.ends_with(".local")
                || domain.ends_with(".internal")
                || domain.ends_with(".home.arpa")
        }
    };
    if !trusted {
        return Err(invalid(
            field,
            "must be a loopback, private-range, or `.local` address so `local-only` \
             requests cannot leave the trusted network",
        ));
    }
    Ok(())
}

pub(crate) fn validate_name(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid(
            field,
            "must use at most 128 ASCII letters, digits, dots, underscores, or hyphens",
        ));
    }
    Ok(())
}

pub(super) fn validate_revision(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    if value.len() > 128
        // `is_ascii_graphic` is 0x21..=0x7E, which already excludes every
        // whitespace byte, so no separate whitespace test is needed.
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(invalid(
            field,
            "must use at most 128 visible ASCII bytes without whitespace",
        ));
    }
    Ok(())
}

pub(super) fn validate_nonempty(field: &str, value: &str) -> Result<(), FabricConfigError> {
    if value.trim().is_empty() {
        Err(invalid(field, "must not be empty"))
    } else {
        Ok(())
    }
}

pub(super) fn validate_model(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    if value.len() > MAX_MODEL_BYTES
        // `is_ascii_graphic` is 0x21..=0x7E, which already excludes every
        // whitespace byte, so no separate whitespace test is needed.
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(invalid(
            field,
            format!("must use at most {MAX_MODEL_BYTES} visible ASCII bytes without whitespace"),
        ));
    }
    Ok(())
}

/// An environment variable name, bounded like every other identifier here.
///
/// The bound is not cosmetic: an unresolvable credential reports the variable
/// name it looked for, so an unbounded name is unbounded text in a runtime
/// error and a log line.
pub(super) fn validate_env_name(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(invalid(field, "must not be empty"));
    };
    if value.len() > MAX_ENV_NAME_BYTES
        || !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(invalid(
            field,
            format!(
                "must be a valid environment variable name of at most {MAX_ENV_NAME_BYTES} bytes"
            ),
        ));
    }
    Ok(())
}

pub(super) fn validate_command(field: &str, command: &[String]) -> Result<(), FabricConfigError> {
    let total_bytes = command.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len())
    });
    if command.is_empty()
        || command.len() > MAX_COMMAND_ARGUMENTS
        || total_bytes > MAX_COMMAND_BYTES
        || command.iter().any(|argument| {
            argument.trim().is_empty()
                || argument.len() > MAX_COMMAND_ARGUMENT_BYTES
                || argument.chars().any(char::is_control)
        })
    {
        return Err(invalid(
            field,
            format!(
                "must contain 1..={MAX_COMMAND_ARGUMENTS} non-empty arguments, each at most {MAX_COMMAND_ARGUMENT_BYTES} bytes without control characters, and at most {MAX_COMMAND_BYTES} bytes total"
            ),
        ));
    }
    Ok(())
}

/// A first-byte deadline, when set, must fit inside the total deadline.
///
/// Stated once: both local pools and providers carry the pair, and a rule split
/// across two call sites is a rule that can be changed in one of them.
pub(super) fn validate_first_byte_timeout(
    field: &str,
    timeout_ms: u64,
    first_byte_timeout_ms: Option<u64>,
) -> Result<(), FabricConfigError> {
    match first_byte_timeout_ms {
        Some(first_byte_timeout_ms) => validate_u64_range(field, first_byte_timeout_ms, timeout_ms),
        None => Ok(()),
    }
}

pub(super) fn validate_executable(field: &str, executable: &str) -> Result<(), FabricConfigError> {
    if executable.trim().is_empty()
        || executable.len() > 4096
        || executable.chars().any(char::is_control)
    {
        return Err(invalid(
            field,
            "must be a non-empty path of at most 4096 bytes without control characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_log_level(value: &str) -> Result<(), FabricConfigError> {
    if matches!(value, "trace" | "debug" | "info" | "warn" | "error") {
        Ok(())
    } else {
        Err(invalid(
            "observability.log_level",
            "must be trace, debug, info, warn, or error",
        ))
    }
}

pub(crate) fn safe_parse_error(input: &str, error: toml::de::Error) -> FabricConfigError {
    let (line, column) = error
        .span()
        .map(|span| line_column(input, span.start))
        .unwrap_or((1, 1));
    FabricConfigError::Parse { line, column }
}

pub(super) fn line_column(input: &str, byte_index: usize) -> (usize, usize) {
    // The index comes from a TOML parser span. No span it produces today lands
    // mid-character, but slicing on one panics, so the boundary is clamped
    // rather than assumed: a startup panic is a worse diagnostic than an
    // approximate column.
    let mut end = byte_index.min(input.len());
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &input[..end];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len() + 1, |(_, tail)| tail.len() + 1);
    (line, column)
}

pub(super) fn validate_usize_range(
    field: &str,
    value: usize,
    maximum: usize,
) -> Result<(), FabricConfigError> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("must be between 1 and {maximum}")))
    }
}

pub(super) fn validate_u32_range(
    field: &str,
    value: u32,
    maximum: u32,
) -> Result<(), FabricConfigError> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("must be between 1 and {maximum}")))
    }
}

pub(super) fn validate_u64_range(
    field: &str,
    value: u64,
    maximum: u64,
) -> Result<(), FabricConfigError> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("must be between 1 and {maximum}")))
    }
}

pub(crate) fn invalid(field: impl Into<String>, message: impl Into<String>) -> FabricConfigError {
    FabricConfigError::Invalid {
        field: field.into(),
        message: message.into(),
    }
}
