//! Shared bounded reader for upstream HTTP response bodies.

use bytes::Bytes;

/// Why a bounded upstream response could not be read.
pub(super) enum BoundedResponseError {
    /// The response stream failed, with whether any body bytes arrived first.
    Read {
        source: reqwest::Error,
        after_body: bool,
    },
    /// The response exceeded the caller's configured ceiling.
    TooLarge,
}

/// Read a complete response without buffering more than `limit` bytes.
pub(super) async fn read(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, BoundedResponseError> {
    let mut body = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|source| BoundedResponseError::Read {
                source,
                after_body: !body.is_empty(),
            })?;
        let Some(chunk) = chunk else {
            return Ok(Bytes::from(body));
        };
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(BoundedResponseError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
}
