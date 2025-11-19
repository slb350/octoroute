//! Priority-based selection tests
//!
//! Tests priority filtering logic: highest priority tier selection,
//! weighted distribution within priority tier, and fallback behavior.

use super::*;
use crate::models::endpoint_name::ExclusionSet;
use crate::models::selector::ModelSelector;
use crate::router::TargetModel;
use std::sync::Arc;

// TODO: Extract priority tests from original selector.rs
// Tests needed:
// - test_priority_selection_highest_chosen
// - test_priority_with_weighted_distribution
// - test_priority_all_same_uses_weighted
