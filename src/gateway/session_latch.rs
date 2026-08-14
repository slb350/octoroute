//! Bounded in-memory cloud-only latches keyed by hashed client session IDs.

use super::config::RoutingConfig;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Mutex,
    time::{Duration, Instant},
};

type SessionHash = [u8; 32];

/// Opaque digest used to correlate a bounded client session without retaining its ID.
#[derive(Clone, Copy)]
pub(crate) struct SessionKey(SessionHash);

impl SessionKey {
    pub(crate) fn new(session_id: &str) -> Self {
        Self(Sha256::digest(session_id.as_bytes()).into())
    }
}

struct LatchEntry {
    hard_evidence: u8,
    expires_at: Instant,
    sequence: u64,
}

#[derive(Default)]
struct LatchState {
    entries: HashMap<SessionHash, LatchEntry>,
    expirations: BTreeSet<(Instant, SessionHash)>,
    order: BTreeSet<(u64, SessionHash)>,
    sequence: u64,
}

/// Optional enforced-mode session policy with no raw identifier retention.
pub(crate) struct SessionLatch {
    ttl: Duration,
    max_entries: usize,
    evidence_threshold: u8,
    state: Mutex<LatchState>,
}

impl SessionLatch {
    pub(crate) fn from_config(config: &RoutingConfig) -> Option<Self> {
        config.session_latch().map(|latch| {
            Self::new(
                Duration::from_millis(latch.ttl_ms()),
                latch.max_entries(),
                latch.evidence_threshold(),
            )
        })
    }

    pub(crate) fn new(ttl: Duration, max_entries: usize, evidence_threshold: u8) -> Self {
        Self {
            ttl,
            max_entries,
            evidence_threshold,
            state: Mutex::new(LatchState::default()),
        }
    }

    pub(crate) fn is_latched(&self, key: SessionKey) -> bool {
        self.is_latched_at(key, Instant::now())
    }

    pub(crate) fn record_hard_evidence(&self, key: SessionKey) {
        self.record_hard_evidence_at(key, Instant::now());
    }

    pub(crate) fn clear_pending(&self, key: SessionKey) {
        self.clear_pending_at(key, Instant::now());
    }

    pub(crate) fn is_latched_at(&self, key: SessionKey, now: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        remove_if_expired(&mut state, key.0, now);
        state
            .entries
            .get(&key.0)
            .is_some_and(|entry| entry.hard_evidence >= self.evidence_threshold)
    }

    pub(crate) fn record_hard_evidence_at(&self, key: SessionKey, now: Instant) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        remove_if_expired(&mut state, key.0, now);
        if !state.entries.contains_key(&key.0) && state.entries.len() == self.max_entries {
            purge_expired(&mut state, now);
            if state.entries.len() == self.max_entries {
                evict_oldest(&mut state);
            }
        }
        state.sequence = state.sequence.wrapping_add(1);
        let sequence = state.sequence;
        let previous_evidence =
            remove_entry(&mut state, key.0).map_or(0, |entry| entry.hard_evidence);
        let entry = LatchEntry {
            hard_evidence: previous_evidence
                .saturating_add(1)
                .min(self.evidence_threshold),
            expires_at: now + self.ttl,
            sequence,
        };
        state.expirations.insert((entry.expires_at, key.0));
        state.order.insert((entry.sequence, key.0));
        state.entries.insert(key.0, entry);
    }

    pub(crate) fn clear_pending_at(&self, key: SessionKey, now: Instant) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        remove_if_expired(&mut state, key.0, now);
        if state
            .entries
            .get(&key.0)
            .is_some_and(|entry| entry.hard_evidence < self.evidence_threshold)
        {
            remove_entry(&mut state, key.0);
        }
    }
}

fn remove_if_expired(state: &mut LatchState, key: SessionHash, now: Instant) {
    if state
        .entries
        .get(&key)
        .is_some_and(|entry| entry.expires_at <= now)
    {
        remove_entry(state, key);
    }
}

fn purge_expired(state: &mut LatchState, now: Instant) {
    while let Some((expires_at, key)) = state.expirations.first().copied() {
        if expires_at > now {
            break;
        }
        remove_entry(state, key);
    }
}

fn evict_oldest(state: &mut LatchState) {
    if let Some((_, key)) = state.order.first().copied() {
        remove_entry(state, key);
    }
}

fn remove_entry(state: &mut LatchState, key: SessionHash) -> Option<LatchEntry> {
    let entry = state.entries.remove(&key);
    if let Some(entry) = entry.as_ref() {
        state.expirations.remove(&(entry.expires_at, key));
        state.order.remove(&(entry.sequence, key));
    }
    entry
}
