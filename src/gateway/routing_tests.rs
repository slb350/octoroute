use super::routing::{ModelIntent, ModelIntentError, PrivacyDirective, PrivacyDirectiveError};
use axum::http::{HeaderMap, HeaderValue};

#[test]
fn resolves_virtual_local_and_cloud_models_without_guessing() {
    let local = "puzzle-75b";
    let cases = [
        ("auto", ModelIntent::Auto),
        ("local", ModelIntent::Local),
        ("cloud", ModelIntent::CloudAuto),
        ("puzzle-75b", ModelIntent::Local),
        ("openrouter/auto", ModelIntent::CloudAuto),
        (
            "anthropic/claude-sonnet-4.6",
            ModelIntent::CloudModel("anthropic/claude-sonnet-4.6".to_string()),
        ),
    ];

    for (model, expected) in cases {
        assert_eq!(
            ModelIntent::resolve(model, local, "openrouter/auto").expect("known model intent"),
            expected
        );
    }
}

#[test]
fn rejects_unknown_unqualified_models() {
    let error = ModelIntent::resolve("made-up-model", "puzzle-75b", "openrouter/auto")
        .expect_err("unknown model must be rejected");

    assert!(matches!(
        error,
        ModelIntentError::UnknownModel(ref model) if model == "made-up-model"
    ));
}

#[test]
fn parses_local_only_privacy_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );

    assert_eq!(
        PrivacyDirective::from_headers(&headers).expect("valid privacy header"),
        PrivacyDirective::LocalOnly
    );
}

#[test]
fn rejects_unknown_or_repeated_privacy_headers() {
    let mut unknown = HeaderMap::new();
    unknown.insert("x-octoroute-privacy", HeaderValue::from_static("trust-me"));
    assert_eq!(
        PrivacyDirective::from_headers(&unknown),
        Err(PrivacyDirectiveError::Invalid)
    );

    let mut repeated = HeaderMap::new();
    repeated.append(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    repeated.append(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    assert_eq!(
        PrivacyDirective::from_headers(&repeated),
        Err(PrivacyDirectiveError::Invalid)
    );
}
