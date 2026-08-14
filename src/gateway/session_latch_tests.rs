use super::session_latch::{SessionKey, SessionLatch};
use std::time::{Duration, Instant};

#[test]
fn repeated_hard_evidence_latches_until_ttl_expiry() {
    let latch = SessionLatch::new(Duration::from_secs(60), 8, 2);
    let now = Instant::now();

    let key = SessionKey::new("session-a");
    latch.record_hard_evidence_at(key, now);
    assert!(!latch.is_latched_at(key, now));
    latch.record_hard_evidence_at(key, now + Duration::from_secs(1));
    assert!(latch.is_latched_at(key, now + Duration::from_secs(1)));
    assert!(!latch.is_latched_at(key, now + Duration::from_secs(61)));
}

#[test]
fn non_hard_observation_clears_pending_evidence_but_not_an_active_latch() {
    let latch = SessionLatch::new(Duration::from_secs(60), 8, 2);
    let now = Instant::now();

    let pending = SessionKey::new("pending");
    latch.record_hard_evidence_at(pending, now);
    latch.clear_pending_at(pending, now + Duration::from_secs(1));
    latch.record_hard_evidence_at(pending, now + Duration::from_secs(2));
    assert!(!latch.is_latched_at(pending, now + Duration::from_secs(2)));

    let active = SessionKey::new("active");
    latch.record_hard_evidence_at(active, now);
    latch.record_hard_evidence_at(active, now + Duration::from_secs(1));
    latch.clear_pending_at(active, now + Duration::from_secs(2));
    assert!(latch.is_latched_at(active, now + Duration::from_secs(2)));
}

#[test]
fn capacity_evicts_the_oldest_hashed_session_entry() {
    let latch = SessionLatch::new(Duration::from_secs(60), 1, 2);
    let now = Instant::now();
    let older = SessionKey::new("older");
    let newer = SessionKey::new("newer");
    for _ in 0..2 {
        latch.record_hard_evidence_at(older, now);
    }
    assert!(latch.is_latched_at(older, now));

    for _ in 0..2 {
        latch.record_hard_evidence_at(newer, now + Duration::from_secs(1));
    }
    assert!(latch.is_latched_at(newer, now + Duration::from_secs(1)));
    assert!(!latch.is_latched_at(older, now + Duration::from_secs(1)));
}
