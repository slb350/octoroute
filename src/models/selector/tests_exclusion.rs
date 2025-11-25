//! Exclusion set tests
//!
//! Tests exclusion set handling for retry logic: filtering out failed endpoints,
//! behavior when all endpoints excluded, and interaction with priority/weight.

// TODO: Extract exclusion tests from original selector.rs
// Tests needed:
// - test_exclusion_filters_endpoints
// - test_exclusion_all_endpoints_returns_none
// - test_exclusion_all_tiers_returns_none
// - test_exclusion_preserves_priority_and_weight
