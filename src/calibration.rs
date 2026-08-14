//! Offline semantic-forecast replay and threshold calibration.

use crate::gateway::{
    config::{MAX_SEMANTIC_BOUNDARY_STEPS, is_probability},
    intelligence::{
        SEMANTIC_PROBABILITY_BUCKETS, SemanticBoundary, SemanticRule, probability_meets_threshold,
    },
};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, cmp::Ordering, collections::HashSet, fmt};
use thiserror::Error;

/// Maximum accepted labeled artifact size.
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_LABEL_BYTES: usize = 128;
const MAX_CLOUD_COST_USD: f64 = 1_000_000.0;
const CALIBRATION_BIN_COUNT: usize = SEMANTIC_PROBABILITY_BUCKETS.len();
const REPORT_ROUNDING_FACTOR: f64 = 1_000_000.0;
// The record and cost caps must keep even the rounded aggregate finite.
const _: () = assert!(MAX_CLOUD_COST_USD * MAX_RECORDS as f64 * REPORT_ROUNDING_FACTOR < f64::MAX);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCalibrationRecord<'a> {
    #[serde(borrow)]
    challenge_id: Cow<'a, str>,
    #[serde(borrow)]
    model_alias: Cow<'a, str>,
    #[serde(borrow)]
    model_revision: Cow<'a, str>,
    #[serde(borrow)]
    capability_card_version: Cow<'a, str>,
    #[serde(borrow)]
    capability_card_fingerprint: Cow<'a, str>,
    p_local_success: f64,
    capability_boundary: SemanticBoundary,
    primary_rule: SemanticRule,
    local_success: bool,
    previous_cloud_decision: Option<bool>,
    cloud_success: Option<bool>,
    routing_latency_ms: Option<u64>,
    cloud_cost_usd: Option<f64>,
}

#[derive(Clone, Copy)]
struct CalibrationRecord {
    p_local_success: f64,
    capability_boundary: SemanticBoundary,
    local_success: bool,
    previous_cloud_decision: Option<bool>,
    cloud_success: Option<bool>,
    routing_latency_ms: Option<u64>,
    cloud_cost_usd: Option<f64>,
}

struct CalibrationDataset {
    identity: DatasetIdentity,
    records: Vec<CalibrationRecord>,
}

#[derive(PartialEq, Eq, Serialize)]
struct DatasetIdentity {
    model_alias: String,
    model_revision: String,
    capability_card_version: String,
    capability_card_fingerprint: String,
}

#[derive(Serialize)]
struct CalibrationReport {
    schema_version: u8,
    dataset: DatasetIdentity,
    record_count: usize,
    calibration: CalibrationQuality,
    always_local: ThresholdMetrics,
    previous_binary: Option<PreviousPolicyMetrics>,
    best_accuracy: ThresholdMetrics,
    beats_always_local: bool,
    observed_average_routing_latency_ms: Option<f64>,
    candidates: Vec<ThresholdMetrics>,
}

#[derive(Serialize)]
struct CalibrationQuality {
    brier_score: f64,
    bins: Vec<CalibrationBin>,
}

#[derive(Serialize)]
struct CalibrationBin {
    #[serde(skip_serializing_if = "Option::is_none")]
    lower_inclusive: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lower_exclusive: Option<f64>,
    upper_inclusive: f64,
    count: usize,
    average_probability: Option<f64>,
    local_success_rate: Option<f64>,
}

#[derive(Serialize)]
struct PreviousPolicyMetrics {
    coverage: usize,
    accuracy: f64,
    false_escalations: usize,
    missed_rescues: usize,
    local_routes: usize,
    cloud_routes: usize,
}

#[derive(Clone, Serialize)]
struct ThresholdMetrics {
    base_threshold: f64,
    boundary_threshold_step: f64,
    accuracy: f64,
    cloud_precision: Option<f64>,
    cloud_recall: Option<f64>,
    false_escalations: usize,
    missed_rescues: usize,
    local_routes: usize,
    cloud_routes: usize,
    successful_cloud_rescues: usize,
    failed_cloud_routes: usize,
    cloud_outcome_coverage: usize,
    estimated_cloud_cost_usd: f64,
    cloud_cost_coverage: usize,
}

/// Analyze bounded labeled JSONL and return a deterministic JSON report.
pub fn analyze_jsonl(input: &str, grid_step: f64) -> Result<String, CalibrationError> {
    if input.len() > MAX_ARTIFACT_BYTES {
        return Err(CalibrationError::ArtifactTooLarge);
    }
    let grid_denominator = validate_grid_step(grid_step)?;
    let dataset = parse_records(input)?;
    let record_count = dataset.records.len();
    let calibration = calibration_quality(&dataset.records);
    let observed_average_routing_latency_ms = average(
        dataset
            .records
            .iter()
            .filter_map(|record| record.routing_latency_ms.map(|value| value as f64)),
    );
    let previous_binary = previous_policy_metrics(&dataset.records);
    let scores = PreparedScores::new(dataset.records);
    let always_local = scores.score_thresholds(0.0, 0.0);
    let candidates = threshold_candidates(&scores, grid_denominator);
    let best_accuracy = candidates
        .iter()
        .max_by(|left, right| compare_candidates(left, right))
        .cloned()
        .ok_or(CalibrationError::NoRecords)?;
    let report = CalibrationReport {
        schema_version: 3,
        dataset: dataset.identity,
        record_count,
        calibration,
        beats_always_local: best_accuracy.accuracy > always_local.accuracy,
        always_local,
        previous_binary,
        best_accuracy,
        observed_average_routing_latency_ms,
        candidates,
    };
    serde_json::to_string_pretty(&report).map_err(|_| CalibrationError::Serialization)
}

fn previous_policy_metrics(records: &[CalibrationRecord]) -> Option<PreviousPolicyMetrics> {
    let mut counts = RouteCounts::default();
    for record in records {
        if let Some(cloud) = record.previous_cloud_decision {
            counts.observe(*record, cloud);
        }
    }
    (counts.total != 0).then(|| PreviousPolicyMetrics {
        coverage: counts.total,
        accuracy: counts.accuracy(),
        false_escalations: counts.false_escalations,
        missed_rescues: counts.missed_rescues,
        local_routes: counts.local_routes(),
        cloud_routes: counts.cloud_routes,
    })
}

fn validate_grid_step(grid_step: f64) -> Result<u32, CalibrationError> {
    if !grid_step.is_finite() || !(0.01..=0.25).contains(&grid_step) {
        return Err(CalibrationError::InvalidGridStep);
    }
    let denominator = (1.0 / grid_step).round();
    if ((denominator * grid_step) - 1.0).abs() > 1e-9 {
        return Err(CalibrationError::InvalidGridStep);
    }
    Ok(denominator as u32)
}

fn parse_records(input: &str) -> Result<CalibrationDataset, CalibrationError> {
    let mut records = Vec::new();
    let mut identifiers = HashSet::new();
    let mut identity: Option<DatasetIdentity> = None;
    for (index, line) in input.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        if records.len() == MAX_RECORDS {
            return Err(CalibrationError::TooManyRecords);
        }
        let raw: RawCalibrationRecord<'_> = serde_json::from_str(line)
            .map_err(|_| CalibrationError::InvalidRecord { line: line_number })?;
        validate_record(&raw, line_number)?;
        let RawCalibrationRecord {
            challenge_id,
            model_alias,
            model_revision,
            capability_card_version,
            capability_card_fingerprint,
            p_local_success,
            capability_boundary,
            primary_rule: _,
            local_success,
            previous_cloud_decision,
            cloud_success,
            routing_latency_ms,
            cloud_cost_usd,
        } = raw;
        if !identifiers.insert(challenge_id) {
            return Err(CalibrationError::DuplicateChallenge { line: line_number });
        }
        match &identity {
            Some(expected)
                if expected.model_alias != model_alias.as_ref()
                    || expected.model_revision != model_revision.as_ref()
                    || expected.capability_card_version != capability_card_version.as_ref()
                    || expected.capability_card_fingerprint
                        != capability_card_fingerprint.as_ref() =>
            {
                return Err(CalibrationError::MixedDataset { line: line_number });
            }
            Some(_) => {}
            None => {
                identity = Some(DatasetIdentity {
                    model_alias: model_alias.into_owned(),
                    model_revision: model_revision.into_owned(),
                    capability_card_version: capability_card_version.into_owned(),
                    capability_card_fingerprint: capability_card_fingerprint.into_owned(),
                });
            }
        }
        records.push(CalibrationRecord {
            p_local_success,
            capability_boundary,
            local_success,
            previous_cloud_decision,
            cloud_success,
            routing_latency_ms,
            cloud_cost_usd,
        });
    }
    let identity = identity.ok_or(CalibrationError::NoRecords)?;
    Ok(CalibrationDataset { identity, records })
}

fn validate_record(record: &RawCalibrationRecord<'_>, line: usize) -> Result<(), CalibrationError> {
    let valid_cost = record
        .cloud_cost_usd
        .is_none_or(|cost| cost.is_finite() && (0.0..=MAX_CLOUD_COST_USD).contains(&cost));
    if valid_label(&record.challenge_id)
        && valid_label(&record.model_alias)
        && valid_model_revision(&record.model_revision)
        && valid_label(&record.capability_card_version)
        && valid_sha256_fingerprint(&record.capability_card_fingerprint)
        && is_probability(record.p_local_success)
        && valid_cost
        && record.primary_rule.boundary() == record.capability_boundary
    {
        Ok(())
    } else {
        Err(CalibrationError::InvalidRecord { line })
    }
}

fn valid_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_LABEL_BYTES && !value.chars().any(char::is_control)
}

fn valid_model_revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LABEL_BYTES
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn valid_sha256_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Default)]
struct BinAccumulator {
    count: usize,
    probability_sum: f64,
    success_count: usize,
}

fn calibration_quality(records: &[CalibrationRecord]) -> CalibrationQuality {
    let mut squared_error_sum = 0.0;
    let mut accumulators = [BinAccumulator::default(); CALIBRATION_BIN_COUNT];
    for record in records {
        let outcome = usize::from(record.local_success);
        squared_error_sum += (record.p_local_success - outcome as f64).powi(2);
        let index = SEMANTIC_PROBABILITY_BUCKETS
            .iter()
            .position(|upper| record.p_local_success <= *upper)
            .unwrap_or(CALIBRATION_BIN_COUNT - 1);
        let bin = &mut accumulators[index];
        bin.count += 1;
        bin.probability_sum += record.p_local_success;
        bin.success_count += outcome;
    }
    let bins = accumulators
        .into_iter()
        .enumerate()
        .map(|(index, bin)| {
            let lower = (index != 0).then(|| SEMANTIC_PROBABILITY_BUCKETS[index - 1]);
            CalibrationBin {
                lower_inclusive: (index == 0).then_some(0.0),
                lower_exclusive: lower,
                upper_inclusive: SEMANTIC_PROBABILITY_BUCKETS[index],
                count: bin.count,
                average_probability: ratio_float(bin.probability_sum, bin.count),
                local_success_rate: ratio(bin.success_count, bin.count),
            }
        })
        .collect();
    CalibrationQuality {
        brier_score: round6(squared_error_sum / records.len() as f64),
        bins,
    }
}

#[derive(Clone, Copy, Default)]
struct RouteCounts {
    total: usize,
    false_escalations: usize,
    missed_rescues: usize,
    cloud_routes: usize,
}

impl RouteCounts {
    fn observe(&mut self, record: CalibrationRecord, cloud: bool) {
        self.total += 1;
        self.cloud_routes += usize::from(cloud);
        self.false_escalations += usize::from(cloud && record.local_success);
        self.missed_rescues += usize::from(!cloud && !record.local_success);
    }

    fn local_routes(self) -> usize {
        self.total - self.cloud_routes
    }

    fn accuracy(self) -> f64 {
        ratio(
            self.total - self.false_escalations - self.missed_rescues,
            self.total,
        )
        .unwrap_or(0.0)
    }
}

#[derive(Clone, Copy, Default)]
struct CloudAggregate {
    routes: usize,
    local_failures: usize,
    successful_rescues: usize,
    failed_routes: usize,
    outcome_coverage: usize,
    cost_sum: f64,
    cost_coverage: usize,
}

impl CloudAggregate {
    fn include(mut self, record: CalibrationRecord) -> Self {
        self.routes += 1;
        self.local_failures += usize::from(!record.local_success);
        if let Some(cloud_success) = record.cloud_success {
            self.outcome_coverage += 1;
            self.successful_rescues += usize::from(!record.local_success && cloud_success);
            self.failed_routes += usize::from(!cloud_success);
        }
        if let Some(cost) = record.cloud_cost_usd {
            self.cost_sum += cost;
            self.cost_coverage += 1;
        }
        self
    }

    fn merge(&mut self, other: Self) {
        self.routes += other.routes;
        self.local_failures += other.local_failures;
        self.successful_rescues += other.successful_rescues;
        self.failed_routes += other.failed_routes;
        self.outcome_coverage += other.outcome_coverage;
        self.cost_sum += other.cost_sum;
        self.cost_coverage += other.cost_coverage;
    }
}

struct BoundaryDistribution {
    boundary: SemanticBoundary,
    probabilities: Vec<f64>,
    prefixes: Vec<CloudAggregate>,
}

impl BoundaryDistribution {
    fn new(boundary: SemanticBoundary, mut records: Vec<CalibrationRecord>) -> Self {
        records.sort_by(|left, right| left.p_local_success.total_cmp(&right.p_local_success));
        let mut probabilities = Vec::with_capacity(records.len());
        let mut prefixes = Vec::with_capacity(records.len() + 1);
        prefixes.push(CloudAggregate::default());
        for record in records {
            probabilities.push(record.p_local_success);
            prefixes.push(prefixes.last().copied().unwrap_or_default().include(record));
        }
        Self {
            boundary,
            probabilities,
            prefixes,
        }
    }

    fn below_threshold(&self, base_threshold: f64, boundary_step: f64) -> CloudAggregate {
        let threshold = self
            .boundary
            .required_probability(base_threshold, boundary_step);
        let index = self
            .probabilities
            .partition_point(|probability| !probability_meets_threshold(*probability, threshold));
        self.prefixes[index]
    }
}

struct PreparedScores {
    distributions: [BoundaryDistribution; SemanticBoundary::ALL.len()],
    record_count: usize,
    local_failures: usize,
}

impl PreparedScores {
    fn new(records: Vec<CalibrationRecord>) -> Self {
        let record_count = records.len();
        let mut groups: [Vec<CalibrationRecord>; SemanticBoundary::ALL.len()] =
            std::array::from_fn(|_| Vec::new());
        let mut local_failures = 0;
        for record in records {
            local_failures += usize::from(!record.local_success);
            groups[record.capability_boundary.index()].push(record);
        }
        Self {
            distributions: std::array::from_fn(|index| {
                BoundaryDistribution::new(
                    SemanticBoundary::ALL[index],
                    std::mem::take(&mut groups[index]),
                )
            }),
            record_count,
            local_failures,
        }
    }

    fn score_thresholds(
        &self,
        base_threshold: f64,
        boundary_threshold_step: f64,
    ) -> ThresholdMetrics {
        let mut cloud = CloudAggregate::default();
        for distribution in &self.distributions {
            cloud.merge(distribution.below_threshold(base_threshold, boundary_threshold_step));
        }
        let counts = RouteCounts {
            total: self.record_count,
            false_escalations: cloud.routes - cloud.local_failures,
            missed_rescues: self.local_failures - cloud.local_failures,
            cloud_routes: cloud.routes,
        };
        ThresholdMetrics {
            base_threshold: round6(base_threshold),
            boundary_threshold_step: round6(boundary_threshold_step),
            accuracy: counts.accuracy(),
            cloud_precision: ratio(cloud.local_failures, cloud.routes),
            cloud_recall: ratio(cloud.local_failures, self.local_failures),
            false_escalations: counts.false_escalations,
            missed_rescues: counts.missed_rescues,
            local_routes: counts.local_routes(),
            cloud_routes: counts.cloud_routes,
            successful_cloud_rescues: cloud.successful_rescues,
            failed_cloud_routes: cloud.failed_routes,
            cloud_outcome_coverage: cloud.outcome_coverage,
            estimated_cloud_cost_usd: round6(cloud.cost_sum),
            cloud_cost_coverage: cloud.cost_coverage,
        }
    }
}

fn threshold_candidates(scores: &PreparedScores, denominator: u32) -> Vec<ThresholdMetrics> {
    let mut candidates = Vec::new();
    for step_tick in 0..=denominator / u32::from(MAX_SEMANTIC_BOUNDARY_STEPS) {
        let used_by_strictest = u32::from(MAX_SEMANTIC_BOUNDARY_STEPS) * step_tick;
        for base_tick in 0..=denominator.saturating_sub(used_by_strictest) {
            candidates.push(scores.score_thresholds(
                f64::from(base_tick) / f64::from(denominator),
                f64::from(step_tick) / f64::from(denominator),
            ));
        }
    }
    candidates
}

fn compare_candidates(left: &ThresholdMetrics, right: &ThresholdMetrics) -> Ordering {
    left.accuracy
        .total_cmp(&right.accuracy)
        .then_with(|| right.false_escalations.cmp(&left.false_escalations))
        .then_with(|| right.missed_rescues.cmp(&left.missed_rescues))
        .then_with(|| right.cloud_routes.cmp(&left.cloud_routes))
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    ratio_float(numerator as f64, denominator)
}

fn ratio_float(numerator: f64, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| round6(numerator / denominator as f64))
}

fn average(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0_usize), |(sum, count), value| {
        (sum + value, count + 1)
    });
    ratio_float(sum, count)
}

fn round6(value: f64) -> f64 {
    (value * REPORT_ROUNDING_FACTOR).round() / REPORT_ROUNDING_FACTOR
}

/// Safe offline-calibration failures which never include artifact contents.
#[derive(Error)]
pub enum CalibrationError {
    #[error("forecast artifact exceeds the 64 MiB limit")]
    ArtifactTooLarge,
    #[error("forecast artifact contains more than 100000 records")]
    TooManyRecords,
    #[error("forecast artifact contains no records")]
    NoRecords,
    #[error("invalid forecast artifact record at line {line}")]
    InvalidRecord { line: usize },
    #[error("duplicate challenge identifier at line {line}")]
    DuplicateChallenge { line: usize },
    #[error(
        "forecast artifact mixes model aliases, model revisions, capability-card versions, or card fingerprints at line {line}"
    )]
    MixedDataset { line: usize },
    #[error("forecast artifact is not valid UTF-8")]
    InvalidEncoding,
    #[error("grid step must evenly divide one and be from 0.01 through 0.25")]
    InvalidGridStep,
    #[error("failed to serialize calibration report")]
    Serialization,
}

impl fmt::Debug for CalibrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `main` errors are rendered with `Debug`; keep that operator surface bounded and safe.
        fmt::Display::fmt(self, formatter)
    }
}
