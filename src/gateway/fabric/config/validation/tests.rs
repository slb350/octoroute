//! Field-validator tests.
//!
//! These cover the security-relevant validators directly rather than through a
//! full configuration parse. The mutation sweep showed the whole of
//! `validate_url` surviving - including the credentials-in-URL check, which had
//! no test at all - because every existing fixture only ever supplied a valid
//! URL.

use super::fields::{
    validate_command, validate_env_name, validate_local_member_url, validate_model, validate_name,
    validate_revision, validate_url,
};
use crate::gateway::fabric::FabricConfigError;

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
