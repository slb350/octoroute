use reqwest::{Client, RequestBuilder};
use secrecy::{ExposeSecret, SecretString};

pub(crate) fn build() -> Result<Client, reqwest::Error> {
    Client::builder().no_proxy().build()
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
