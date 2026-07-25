use super::auth::{AuthError, BearerAuthenticator};
use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use secrecy::SecretString;

fn authenticator() -> BearerAuthenticator {
    BearerAuthenticator::new(SecretString::from("correct-secret"))
}

#[test]
fn accepts_exact_bearer_credential() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer correct-secret"),
    );

    assert_eq!(authenticator().authorize(&headers), Ok(()));
}

#[test]
fn bearer_scheme_is_case_insensitive() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("bEaReR correct-secret"),
    );

    assert_eq!(authenticator().authorize(&headers), Ok(()));
}

#[test]
fn missing_credential_is_distinct_from_invalid_credential() {
    assert_eq!(
        authenticator().authorize(&HeaderMap::new()),
        Err(AuthError::Missing)
    );
}

#[test]
fn rejects_wrong_or_malformed_credentials_without_echoing_them() {
    for value in [
        "Bearer wrong-secret",
        "Basic correct-secret",
        "Bearer",
        "Bearer ",
        "Bearer correct-secret extra",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(value).expect("valid header fixture"),
        );

        let error = authenticator()
            .authorize(&headers)
            .expect_err("credential must be rejected");
        assert!(matches!(error, AuthError::Invalid));
        assert!(!error.to_string().contains(value));
        assert!(!format!("{error:?}").contains("correct-secret"));
    }
}

#[test]
fn authenticator_debug_output_redacts_the_secret() {
    let debug = format!("{:?}", authenticator());

    assert!(!debug.contains("correct-secret"));
    assert!(debug.contains("REDACTED"));
}
