use reqwest::{Client, RequestBuilder, Url, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};

pub(crate) const LOCAL_CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";

/// Build the one pooled client every Octoroute upstream shares.
///
/// Redirects are refused rather than followed. Provider credentials travel in a
/// custom `x-api-key` header on the Anthropic protocol, which reqwest does not
/// strip on a cross-origin redirect, so a hijacked or misconfigured endpoint
/// answering 3xx would otherwise receive the key; reqwest would also rewrite the
/// POST to a GET against a host the operator never configured. A 3xx instead
/// reaches the route executor as an ordinary pre-commit failure.
pub(crate) fn build() -> Result<Client, reqwest::Error> {
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .build()
}

pub(crate) fn endpoint_url(base: &Url, path: &str) -> Option<Url> {
    base.join(path).ok()
}

pub(crate) fn authorized(
    request: RequestBuilder,
    api_key: Option<&SecretString>,
) -> RequestBuilder {
    match api_key {
        Some(api_key) => request.bearer_auth(api_key.expose_secret()),
        None => request,
    }
}
