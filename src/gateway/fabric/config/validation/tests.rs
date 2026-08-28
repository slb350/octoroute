//! Field-validator tests.
//!
//! These cover the security-relevant validators directly rather than through a
//! full configuration parse. The mutation sweep showed the whole of
//! `validate_url` surviving - including the credentials-in-URL check, which had
//! no test at all - because every existing fixture only ever supplied a valid
//! URL.

use super::fields::{
    line_column, validate_command, validate_env_name, validate_executable,
    validate_local_member_url, validate_log_level, validate_model, validate_name,
    validate_revision, validate_u32_range, validate_url, validate_usize_range,
};
use super::{
    DEFAULT_MEMBER_MAX_IN_FLIGHT, DEFAULT_PROVIDER_MAX_IN_FLIGHT,
    DEFAULT_PROVIDER_READINESS_TIMEOUT_MS,
};
use crate::gateway::fabric::{FabricConfig, FabricConfigError};

const FIELD: &str = "fabric.providers.endpoint";

/// A credential, query, or fragment in an endpoint URL is rejected. Each clause
/// is asserted separately so replacing any `||` with `&&` fails.
#[test]
fn endpoint_urls_reject_credentials_queries_and_fragments() {
    for rejected in [
        "https://user@api.example.com/v1/",
        "https://user:secret@api.example.com/v1/",
        "https://:secret@api.example.com/v1/",
        "https://api.example.com/v1/?key=secret",
        "https://api.example.com/v1/#fragment",
    ] {
        validate_url(FIELD, rejected, true).expect_err(&format!("`{rejected}` must be rejected"));
    }
    validate_url(FIELD, "https://api.example.com/v1/", true).expect("a clean HTTPS URL is valid");
}

/// An error message must never echo the offending URL, which may carry the
/// credential that made it invalid.
#[test]
fn url_errors_never_echo_the_offending_value() {
    let error = validate_url(FIELD, "https://user:hunter2@api.example.com/v1/", true)
        .expect_err("credentials are rejected");
    let rendered = error.to_string();
    assert!(!rendered.contains("hunter2"), "error echoed the credential");
    assert!(!rendered.contains("user"), "error echoed the username");
}

/// Provider endpoints are HTTPS-only; local members may use plain HTTP.
#[test]
fn https_is_required_only_where_the_boundary_demands_it() {
    validate_url(FIELD, "http://api.example.com/v1/", true)
        .expect_err("provider endpoints must be HTTPS");
    validate_url(FIELD, "http://127.0.0.1:8080/", false).expect("local HTTP is permitted");
    for rejected in ["ftp://example.com/", "file:///etc/passwd", "not-a-url"] {
        validate_url(FIELD, rejected, false).expect_err(&format!("`{rejected}` must be rejected"));
    }
}

/// The path is normalized to a trailing slash so endpoint joining is stable.
#[test]
fn endpoint_paths_are_normalized_with_a_trailing_slash() {
    let url = validate_url(FIELD, "https://api.example.com/v1", true).expect("valid URL");
    assert_eq!(url.path(), "/v1/");
    let already = validate_url(FIELD, "https://api.example.com/v1/", true).expect("valid URL");
    assert_eq!(already.path(), "/v1/");
}

/// `local-only` promises the request does not leave the trusted network, and a
/// member address is the only thing that promise rests on.
#[test]
fn local_members_must_be_on_a_trusted_address() {
    const MEMBER: &str = "fabric.local_pools.members.base_url";
    for trusted in [
        "http://127.0.0.1:8080/",
        "http://10.0.0.4:8080/",
        "http://172.16.5.9:8080/",
        "http://192.168.1.20:8080/",
        "http://169.254.10.1:8080/",
        "http://[::1]:8080/",
        "http://[fd00::1]:8080/",
        "http://localhost:8080/",
        "http://strix.local:8080/",
        "http://worker.internal:8080/",
    ] {
        let url = validate_url(MEMBER, trusted, false).expect("parses");
        validate_local_member_url(MEMBER, &url)
            .unwrap_or_else(|_| panic!("`{trusted}` is a trusted address"));
    }
    for public in [
        "http://93.184.216.34:8080/",
        "http://api.example.com:8080/",
        "http://[2606:2800:220:1:248:1893:25c8:1946]:8080/",
        "http://8.8.8.8:8080/",
    ] {
        let url = validate_url(MEMBER, public, false).expect("parses");
        validate_local_member_url(MEMBER, &url)
            .expect_err(&format!("`{public}` must not satisfy local-only"));
    }
}

/// Bounded identifiers: each validator's length limit is exact and its charset
/// is closed. Asserting the boundary on both sides pins `>` against `>=`/`==`,
/// and asserting a rejected charset pins `||` against `&&`.
#[test]
fn bounded_identifier_validators_enforce_exact_limits() {
    type Validator = fn(&str, &str) -> Result<(), FabricConfigError>;

    for (name, validate, limit) in [
        ("validate_name", validate_name as Validator, 128_usize),
        ("validate_revision", validate_revision as Validator, 128),
        ("validate_model", validate_model as Validator, 512),
    ] {
        let accepts = |value: &str| validate(FIELD, value).is_ok();
        assert!(
            accepts(&"a".repeat(limit)),
            "{name}: exactly {limit} bytes must be accepted"
        );
        assert!(
            !accepts(&"a".repeat(limit + 1)),
            "{name}: {} bytes must be rejected",
            limit + 1
        );
        assert!(!accepts("has space"), "{name}: whitespace is rejected");
        assert!(!accepts(""), "{name}: empty is rejected");
        assert!(!accepts("   "), "{name}: blank is rejected");
    }
}

/// `validate_name` accepts only alphanumerics, dot, underscore, and hyphen.
/// `validate_revision` and `validate_model` accept any visible ASCII.
#[test]
fn name_and_revision_charsets_differ_as_documented() {
    for accepted in ["worker-0", "pool.name", "model_v2", "abc123"] {
        validate_name(FIELD, accepted).unwrap_or_else(|_| panic!("`{accepted}` is a valid name"));
    }
    for rejected in [
        "worker/0",
        "pool:name",
        "model@v2",
        "sp ace",
        "tab\there",
        "n\u{00e9}e",
    ] {
        validate_name(FIELD, rejected).expect_err(&format!("`{rejected}` is not a valid name"));
    }
    // A revision may carry punctuation a name may not, but still no whitespace.
    validate_revision(FIELD, "sha256:abc/def+1").expect("visible ASCII is a valid revision");
    validate_revision(FIELD, "has space").expect_err("whitespace is rejected");
    validate_revision(FIELD, "n\u{00e9}e").expect_err("non-ASCII is rejected");
}

/// Environment variable names: first byte is a letter or underscore, the rest
/// alphanumeric or underscore.
#[test]
fn environment_variable_names_follow_shell_rules() {
    for accepted in ["OCTOROUTE_API_KEY", "_PRIVATE", "A1", "a_b_c"] {
        validate_env_name(FIELD, accepted)
            .unwrap_or_else(|_| panic!("`{accepted}` is a valid env name"));
    }
    for rejected in [
        "1LEADING_DIGIT",
        "HAS-HYPHEN",
        "HAS.DOT",
        "HAS SPACE",
        "",
        "$VAR",
    ] {
        validate_env_name(FIELD, rejected)
            .expect_err(&format!("`{rejected}` is not a valid env name"));
    }
}

/// Credential command argv is bounded on argument count, per-argument size, and
/// total size, and rejects empty or control-bearing arguments.
#[test]
fn credential_command_argv_is_bounded_on_every_axis() {
    let ok = |command: &[&str]| {
        validate_command(
            FIELD,
            &command.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        )
        .is_ok()
    };

    assert!(ok(&["op", "read", "op://vault/item"]));
    assert!(!ok(&[]), "an empty argv is rejected");

    // Argument count: 32 is the limit.
    let at_limit = vec!["a"; 32];
    let over_limit = vec!["a"; 33];
    assert!(ok(&at_limit), "exactly 32 arguments are accepted");
    assert!(!ok(&over_limit), "33 arguments are rejected");

    // Per-argument size: 4096 bytes is the limit.
    let arg_at = "a".repeat(4096);
    let arg_over = "a".repeat(4097);
    assert!(ok(&["op", &arg_at]), "a 4096-byte argument is accepted");
    assert!(!ok(&["op", &arg_over]), "a 4097-byte argument is rejected");

    // Total size: 16384 bytes across all arguments.
    let four_k = "a".repeat(4096);
    let four_args = vec![four_k.as_str(); 4];
    assert!(ok(&four_args), "16384 bytes total is accepted");
    let five_args = vec![four_k.as_str(); 5];
    assert!(!ok(&five_args), "20480 bytes total is rejected");

    // Argument content.
    assert!(!ok(&["op", ""]), "an empty argument is rejected");
    assert!(!ok(&["op", "   "]), "a blank argument is rejected");
    assert!(
        !ok(&["op", "has\nnewline"]),
        "control characters are rejected"
    );
    assert!(!ok(&["op", "has\0null"]), "a NUL is rejected");
}

/// The Codex executable path is bounded and rejects control characters.
#[test]
fn executable_paths_are_bounded_and_control_free() {
    validate_executable(FIELD, "codex").expect("a plain name is valid");
    validate_executable(FIELD, "/usr/local/bin/codex").expect("an absolute path is valid");
    validate_executable(FIELD, &"a".repeat(4096)).expect("exactly 4096 bytes is accepted");
    validate_executable(FIELD, &"a".repeat(4097)).expect_err("4097 bytes is rejected");
    for rejected in ["", "   ", "codex\nrm -rf /", "codex\0", "codex\ttab"] {
        validate_executable(FIELD, rejected)
            .expect_err(&format!("`{rejected:?}` must be rejected"));
    }
}

/// `log_level` is a closed set; anything else is a configuration error rather
/// than a silent fallback.
#[test]
fn log_level_accepts_only_the_five_documented_values() {
    for accepted in ["trace", "debug", "info", "warn", "error"] {
        validate_log_level(accepted).unwrap_or_else(|_| panic!("`{accepted}` is valid"));
    }
    for rejected in ["INFO", "warning", "verbose", "", "off", "fatal"] {
        validate_log_level(rejected).expect_err(&format!("`{rejected}` must be rejected"));
    }
}

/// Numeric ranges are inclusive at both ends and reject zero.
#[test]
fn numeric_ranges_are_inclusive_and_reject_zero() {
    validate_usize_range(FIELD, 1, 10).expect("the lower bound is accepted");
    validate_usize_range(FIELD, 10, 10).expect("the upper bound is accepted");
    validate_usize_range(FIELD, 0, 10).expect_err("zero is rejected");
    validate_usize_range(FIELD, 11, 10).expect_err("above the maximum is rejected");

    validate_u32_range(FIELD, 1, 10).expect("the lower bound is accepted");
    validate_u32_range(FIELD, 10, 10).expect("the upper bound is accepted");
    validate_u32_range(FIELD, 0, 10).expect_err("zero is rejected");
    validate_u32_range(FIELD, 11, 10).expect_err("above the maximum is rejected");
}

/// Parse errors report a 1-indexed line and column and never echo the document,
/// which may contain the credential that made it invalid.
#[test]
fn parse_errors_locate_the_fault_without_echoing_the_document() {
    // The same fault, once on line 1 and once on line 3. Both report the same
    // column, which is what pins the column arithmetic: it is counted from the
    // start of its own line, not from the start of the document.
    const FAULT: &str = "[server\n";
    let first = FabricConfig::from_toml(FAULT).expect_err("malformed TOML");
    let FabricConfigError::Parse { line, column } = first else {
        panic!("expected a parse error, got {first:?}");
    };
    assert_eq!((line, column), (1, 8));

    let later = FabricConfig::from_toml(&format!(
        "config_version = 3\nkey = \"do-not-echo\"\n{FAULT}"
    ))
    .expect_err("malformed TOML");
    let FabricConfigError::Parse { line, column } = later else {
        panic!("expected a parse error, got {later:?}");
    };
    assert_eq!(
        (line, column),
        (3, 8),
        "the line advances but the column restarts"
    );
    assert!(
        !format!("{later}").contains("do-not-echo"),
        "a parse error must never echo the document"
    );
}

/// `validate_model` and `validate_revision` accept visible ASCII only. The
/// interesting rejections are bytes that are neither graphic nor whitespace -
/// NUL, DEL, and any non-ASCII byte - because a charset test using only spaces
/// cannot distinguish the intended check from a broken one.
#[test]
fn visible_ascii_validators_reject_non_graphic_bytes() {
    for value in ["gpt-4o", "sha256:abc/def+1", "model@v2", "~!#$%^&*()_+"] {
        validate_model(FIELD, value).unwrap_or_else(|_| panic!("`{value}` is visible ASCII"));
        validate_revision(FIELD, value).unwrap_or_else(|_| panic!("`{value}` is visible ASCII"));
    }
    for value in [
        "model\u{0000}",
        "model\u{007f}",
        "mod\u{00e9}l",
        "model\u{200b}",
        "model space",
        "model\ttab",
        "model\nnewline",
    ] {
        validate_model(FIELD, value).expect_err("not visible ASCII");
        validate_revision(FIELD, value).expect_err("not visible ASCII");
    }
}

/// An environment variable name is bounded like every other identifier here.
/// Without a bound a 100KB name validates and is then interpolated into the
/// runtime error an unresolvable credential produces.
#[test]
fn environment_variable_names_are_length_bounded() {
    validate_env_name(FIELD, &"A".repeat(128)).expect("exactly 128 bytes is accepted");
    validate_env_name(FIELD, &"A".repeat(129)).expect_err("129 bytes is rejected");
    validate_env_name(FIELD, &"A".repeat(100_000)).expect_err("an unbounded name is rejected");
}

/// Link-local is trusted in both address families or in neither. 169.254/16 and
/// fe80::/10 are the same class of address - unroutable past the local link -
/// so trusting one and rejecting the other is an accident, not a policy.
#[test]
fn link_local_addresses_are_trusted_in_both_families() {
    const MEMBER: &str = "fabric.local_pools.members.base_url";
    for trusted in [
        "http://169.254.10.1:8080/",
        "http://[fe80::1]:8080/",
        "http://[fe80::1ff:fe23:4567:890a]:8080/",
        "http://[febf::1]:8080/",
    ] {
        let url = validate_url(MEMBER, trusted, false).expect("parses");
        validate_local_member_url(MEMBER, &url)
            .unwrap_or_else(|_| panic!("`{trusted}` is link-local and must be trusted"));
    }
    // fec0::/10 was site-local and is deprecated, not link-local: the mask is
    // fe80::/10, not fe00::/8.
    for rejected in ["http://[fec0::1]:8080/", "http://[fe00::1]:8080/"] {
        let url = validate_url(MEMBER, rejected, false).expect("parses");
        validate_local_member_url(MEMBER, &url)
            .expect_err(&format!("`{rejected}` is not link-local"));
    }
}

/// `line_column` slices the document at a byte index supplied by the TOML
/// parser. No parser span lands mid-character today, so the guard is cheap
/// insurance rather than a fix: a mid-character index must clamp, not panic.
#[test]
fn line_column_never_slices_across_a_character_boundary() {
    // `é` is two bytes; index 2 is inside it.
    let input = "a\u{00e9}b\nsecond";
    assert_eq!(line_column(input, 2), (1, 2));
    assert_eq!(line_column(input, 0), (1, 1));
    assert_eq!(line_column(input, input.len()), (2, 7));
    assert_eq!(
        line_column(input, usize::MAX),
        (2, 7),
        "an out-of-range index clamps to the end of the document"
    );
}

#[test]
fn omitted_fields_resolve_to_the_documented_runtime_defaults() {
    let config = FabricConfig::from_toml(
        r#"
config_version = 3
[server]
host = "127.0.0.1"
port = 8081
api_key_env = "OCTOROUTE_API_KEY"
[fabric]
[[fabric.local_pools]]
name = "local"
model = "model"
model_revision = "revision"
context_window = 65536
capabilities = ["chat"]
[[fabric.local_pools.members]]
name = "member"
base_url = "http://127.0.0.1:8000"
[[fabric.providers]]
name = "cloud"
kind = "http"
endpoint = "https://api.example.com/v1"
protocol = "open_ai"
model = "model"
api_key_env = "PROVIDER_API_KEY"
[routing]
default_model = "route"
[[routing.routes]]
model = "route"
privacy = "cloud_allowed"
steps = ["pool:local", "provider:cloud"]
"#,
    )
    .expect("configuration using every assigned default");

    assert_eq!(
        (
            config.server.max_request_bytes,
            config.server.max_header_bytes,
            config.server.max_in_flight,
            config.server.requests_per_minute,
        ),
        (8_388_608, 32_768, 64, 120)
    );
    let pool = &config.local_pools["local"];
    assert_eq!(
        (
            pool.context_safety_tokens,
            pool.default_max_output_tokens,
            pool.timeout_ms,
        ),
        (2_048, 16_384, 1_800_000)
    );
    assert_eq!(pool.members[0].priority, 100);
    let provider = &config.providers["cloud"];
    assert_eq!(
        (provider.timeout_ms, provider.readiness_ttl_ms),
        (1_800_000, 30_000)
    );
    for (field, actual, expected) in [
        (
            "member max_in_flight",
            pool.members[0].max_in_flight as u64,
            DEFAULT_MEMBER_MAX_IN_FLIGHT as u64,
        ),
        (
            "provider max_in_flight",
            provider.max_in_flight as u64,
            DEFAULT_PROVIDER_MAX_IN_FLIGHT as u64,
        ),
        (
            "provider readiness_timeout_ms",
            provider.readiness_timeout_ms,
            DEFAULT_PROVIDER_READINESS_TIMEOUT_MS,
        ),
    ] {
        assert_eq!(actual, expected, "omitted {field}");
    }
    assert_eq!(config.observability.log_level, "info");
}
