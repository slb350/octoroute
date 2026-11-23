//! Shared bounded reader for upstream HTTP response bodies.

use bytes::{Bytes, BytesMut};

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

/// Read a complete response without buffering more than `limit` bytes,
/// retaining a zero-copy path when the body arrives in a single chunk.
pub(super) async fn read(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, BoundedResponseError> {
    let mut single: Option<Bytes> = None;
    let mut coalesced: Option<BytesMut> = None;
    let mut len = 0usize;
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|source| BoundedResponseError::Read {
                source,
                after_body: len > 0,
            })?;
        let Some(chunk) = chunk else {
            return Ok(match (coalesced, single) {
                (Some(body), _) => body.freeze(),
                (None, Some(first)) => first,
                (None, None) => Bytes::new(),
            });
        };
        len = len.saturating_add(chunk.len());
        if len > limit {
            return Err(BoundedResponseError::TooLarge);
        }
        if let Some(body) = &mut coalesced {
            body.extend_from_slice(&chunk);
        } else if let Some(first) = single.take() {
            let hint = response
                .content_length()
                .and_then(|content_len| usize::try_from(content_len).ok())
                .map_or(len, |content_len| content_len.min(limit).max(len));
            let mut body = BytesMut::with_capacity(hint);
            body.extend_from_slice(&first);
            body.extend_from_slice(&chunk);
            coalesced = Some(body);
        } else {
            single = Some(chunk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundedResponseError, read};
    use bytes::Bytes;
    use futures_util::stream;
    use std::convert::Infallible;

    fn streaming_response(chunks: &'static [&'static [u8]]) -> reqwest::Response {
        let body = reqwest::Body::wrap_stream(stream::iter(
            chunks
                .iter()
                .map(|chunk| Ok::<_, Infallible>(Bytes::from_static(chunk))),
        ));
        axum::http::Response::builder()
            .status(200)
            .body(body)
            .expect("test response")
            .into()
    }

    #[tokio::test]
    async fn read_handles_empty_single_and_multi_chunk_boundaries() {
        assert_eq!(
            read(streaming_response(&[]), 4).await.ok(),
            Some(Bytes::new())
        );
        assert_eq!(
            read(streaming_response(&[b"abcd"]), 4).await.ok(),
            Some(Bytes::from_static(b"abcd"))
        );
        assert!(matches!(
            read(streaming_response(&[b"abcde"]), 4).await,
            Err(BoundedResponseError::TooLarge)
        ));
        assert_eq!(
            read(streaming_response(&[b"ab", b"cd", b"ef"]), 6)
                .await
                .ok(),
            Some(Bytes::from_static(b"abcdef"))
        );
        assert!(matches!(
            read(streaming_response(&[b"ab", b"cd", b"efg"]), 6).await,
            Err(BoundedResponseError::TooLarge)
        ));
    }
}
