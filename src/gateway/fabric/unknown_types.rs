//! One counter for upstream variants Octoroute does not recognize.
//!
//! Production adapters share `GLOBAL`, so multiple gateway configurations in
//! one process aggregate their forward-compatibility signal. Scoped instances
//! let translation and exposition be checked without unrelated request traffic.

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
}

#[derive(Default)]
pub(super) struct Counters {
    anthropic: AtomicU64,
    codex: AtomicU64,
}

pub(super) static GLOBAL: Counters = Counters {
    anthropic: AtomicU64::new(0),
    codex: AtomicU64::new(0),
};

impl Counters {
    fn counter(&self, adapter: Adapter) -> &AtomicU64 {
        match adapter {
            Adapter::Anthropic => &self.anthropic,
            Adapter::Codex => &self.codex,
        }
    }

    pub(super) fn record(&self, adapter: Adapter) {
        self.counter(adapter).fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count(&self, adapter: Adapter) -> u64 {
        self.counter(adapter).load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::{Adapter, Counters};

    #[test]
    fn counter_instances_keep_adapter_counts_independent() {
        let first = Counters::default();
        let second = Counters::default();
        first.record(Adapter::Anthropic);
        first.record(Adapter::Codex);
        second.record(Adapter::Codex);
        assert_eq!(first.count(Adapter::Anthropic), 1);
        assert_eq!(first.count(Adapter::Codex), 1);
        assert_eq!(second.count(Adapter::Anthropic), 0);
        assert_eq!(second.count(Adapter::Codex), 1);
    }

    #[test]
    fn adapter_labels_are_the_bounded_prometheus_values() {
        for (adapter, expected) in [(Adapter::Anthropic, "anthropic"), (Adapter::Codex, "codex")] {
            assert_eq!(adapter.as_str(), expected, "{adapter:?}");
        }
    }
}
