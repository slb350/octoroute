//! Deterministic request sampling for shadow-only semantic observations.

use super::config::is_probability;
use sha2::{Digest, Sha256};

/// Stable sampler keyed by the bounded request ID rather than prompt content.
#[derive(Clone, Copy)]
pub(crate) struct DeterministicSampler {
    rate: f64,
}

impl DeterministicSampler {
    pub(crate) fn new(rate: f64) -> Self {
        debug_assert!(is_probability(rate));
        Self { rate }
    }

    pub(crate) fn includes(self, request_id: &str) -> bool {
        if self.rate == 0.0 {
            return false;
        }
        if self.rate == 1.0 {
            return true;
        }
        let digest = Sha256::digest(request_id.as_bytes());
        let bucket = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"));
        (bucket as f64) < self.rate * (u64::MAX as f64 + 1.0)
    }
}
