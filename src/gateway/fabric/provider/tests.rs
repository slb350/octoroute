//! Provider runtime unit tests that need no network or child process.

use super::ProviderAdmissionState;
use super::readiness::CachedReadiness;
use std::time::{Duration, Instant};

/// The readiness cache is what stops a readiness pass fanning out a
/// credentialed probe per caller, and its TTL boundary is exact: a verdict is
/// reusable strictly before the TTL elapses.
///
/// The boundary is not decoration. `readiness_ttl_ms = 0` means "probe every
/// pass", and a `<=` comparison turns that into "probe once, then reuse the
/// first answer for the life of the process" - the caching bug this test
/// exists to catch. Passing `now` in rather than reading the clock makes the
/// instant exactly at the boundary reachable.
#[test]
fn a_cached_verdict_expires_exactly_at_its_ttl() {
    let ttl = Duration::from_millis(30_000);
    let checked_at = Instant::now();
    let cached = CachedReadiness {
        checked_at: Some(checked_at),
        state: ProviderAdmissionState::Ready,
    };

    assert_eq!(
        cached.reusable(checked_at, ttl),
        Some(ProviderAdmissionState::Ready),
        "a verdict taken now is reusable"
    );
    assert_eq!(
        cached.reusable(checked_at + ttl - Duration::from_nanos(1), ttl),
        Some(ProviderAdmissionState::Ready),
        "a verdict is reusable up to the last instant before its TTL"
    );
    assert_eq!(
        cached.reusable(checked_at + ttl, ttl),
        None,
        "a verdict exactly as old as the TTL has expired"
    );
    assert_eq!(
        cached.reusable(checked_at + ttl + Duration::from_millis(1), ttl),
        None,
        "a verdict older than the TTL has expired"
    );
}

/// A zero TTL asks for a probe on every pass, including the first one after a
/// verdict was just recorded.
#[test]
fn a_zero_ttl_never_reuses_a_verdict() {
    let checked_at = Instant::now();
    let cached = CachedReadiness {
        checked_at: Some(checked_at),
        state: ProviderAdmissionState::Ready,
    };

    assert_eq!(cached.reusable(checked_at, Duration::ZERO), None);
}

/// The default cache has never been probed, so there is nothing to reuse - and
/// it must not be mistaken for a verdict taken at process start.
#[test]
fn an_unprobed_cache_has_no_verdict_to_reuse() {
    let cached = CachedReadiness::default();

    assert_eq!(
        cached.reusable(Instant::now(), Duration::from_secs(3600)),
        None
    );
    assert_eq!(cached.state, ProviderAdmissionState::Unavailable);
}
