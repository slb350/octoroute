//! One counter for upstream variants Octoroute does not recognize.
//!
//! Every translating adapter skips content blocks, events, and deltas it has no
//! mapping for rather than failing a committed response, and each skip is
//! recorded here. One counter with a closed `adapter` label rather than a static
//! per adapter: a new adapter opts in by adding a variant, and the exposition
//! shape stays in one place.
//!
//! Process-global by necessity - the translation functions are pure and have no
//! handle on a service instance - so a process running two gateway
//! configurations reports their skips together. That is acceptable for a
//! forward-compatibility signal, whose only use is "did an upstream start
//! sending something new".

use std::sync::atomic::{AtomicU64, Ordering};

/// A translating adapter that can encounter an unrecognized upstream variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Adapter {
    Anthropic,
    Codex,
}

impl Adapter {
    /// Every variant, so the exposition emits a zero series for each.
    pub(super) const ALL: [Self; 2] = [Self::Anthropic, Self::Codex];

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }

    const fn counter(self) -> &'static AtomicU64 {
        match self {
            Self::Anthropic => &ANTHROPIC,
            Self::Codex => &CODEX,
        }
    }
}

static ANTHROPIC: AtomicU64 = AtomicU64::new(0);
static CODEX: AtomicU64 = AtomicU64::new(0);

/// Record one skipped unrecognized variant.
pub(super) fn record(adapter: Adapter) {
    adapter.counter().fetch_add(1, Ordering::Relaxed);
}

/// Read one adapter's count for the Prometheus registry.
pub(super) fn count(adapter: Adapter) -> u64 {
    adapter.counter().load(Ordering::Relaxed)
}
