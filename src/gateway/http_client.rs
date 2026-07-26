use reqwest::{Client, RequestBuilder, Url};
use secrecy::{ExposeSecret, SecretString};

pub(crate) const LOCAL_CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";

pub(crate) fn build() -> Result<Client, reqwest::Error> {
    Client::builder().no_proxy().build()
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
