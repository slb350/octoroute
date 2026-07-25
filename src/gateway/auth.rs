//! Inbound gateway bearer authentication.

use axum::http::{HeaderMap, header::AUTHORIZATION};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::fmt;
use subtle::ConstantTimeEq;
use thiserror::Error;

/// Validates the private bearer credential accepted by Octoroute.
pub struct BearerAuthenticator {
    expected_digest: [u8; 32],
}

impl BearerAuthenticator {
    /// Construct an authenticator from a redacted configured secret.
    pub fn new(expected: SecretString) -> Self {
        Self {
            expected_digest: Sha256::digest(expected.expose_secret().as_bytes()).into(),
        }
    }

    /// Validate exactly one RFC 6750-style bearer header.
    pub fn authorize(&self, headers: &HeaderMap) -> Result<(), AuthError> {
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return Err(AuthError::Missing);
        };
        if values.next().is_some() {
            return Err(AuthError::Invalid);
        }

        let value = value.to_str().map_err(|_| AuthError::Invalid)?;
        let mut parts = value.split_ascii_whitespace();
        let scheme = parts.next().ok_or(AuthError::Invalid)?;
        let credential = parts.next().ok_or(AuthError::Invalid)?;
        if parts.next().is_some() || !scheme.eq_ignore_ascii_case("bearer") {
            return Err(AuthError::Invalid);
        }

        let supplied = Sha256::digest(credential.as_bytes());
        if bool::from(self.expected_digest.ct_eq(&supplied)) {
            Ok(())
        } else {
            Err(AuthError::Invalid)
        }
    }
}

impl fmt::Debug for BearerAuthenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BearerAuthenticator")
            .field("expected_digest", &"[REDACTED]")
            .finish()
    }
}

/// Authentication failures safe to return without credential details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AuthError {
    /// No Authorization header was supplied.
    #[error("bearer authentication is required")]
    Missing,
    /// The header was malformed or did not match.
    #[error("invalid bearer credential")]
    Invalid,
}
