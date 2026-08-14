//! Deterministic signals from explicitly typed, paired tool-result history.

use super::request::{GatewayRequest, RequestFeature, valid_tool_call_id};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

const MAX_CLEAN_STREAK: u8 = 32;
const MAX_TRAJECTORY_TOOL_CALLS: usize = 64;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ToolOutcome {
    Success,
    Failure,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorSeverity {
    None,
    Retryable,
    Hard,
}

impl ErrorSeverity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Retryable => "retryable",
            Self::Hard => "hard",
        }
    }
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum EnvironmentEvidence {
    Unknown,
    Development,
    Production,
}

impl EnvironmentEvidence {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Development => "development",
            Self::Production => "production",
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TestStatus {
    NotRun,
    Passed,
    Failed,
}

impl TestStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TypedToolResult {
    #[serde(rename = "type")]
    _kind: ToolResultKind,
    trajectory: ToolTrajectory,
    #[serde(rename = "result")]
    _result: IgnoredAny,
}

#[derive(Deserialize)]
enum ToolResultKind {
    #[serde(rename = "octoroute.trajectory/v1")]
    V1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolTrajectory {
    outcome: ToolOutcome,
    error_severity: ErrorSeverity,
    environment: EnvironmentEvidence,
    test_status: TestStatus,
    context_compacted: bool,
}

impl ToolTrajectory {
    fn is_consistent(&self) -> bool {
        matches!(
            (self.outcome, self.error_severity),
            (ToolOutcome::Success, ErrorSeverity::None)
                | (
                    ToolOutcome::Failure,
                    ErrorSeverity::Retryable | ErrorSeverity::Hard
                )
        )
    }
}

/// Closed trajectory evidence suitable for local shadow forecasting and logs.
#[derive(Serialize)]
pub(crate) struct TrajectorySignals {
    error_severity: ErrorSeverity,
    clean_streak: u8,
    environment: EnvironmentEvidence,
    test_status: TestStatus,
    context_compacted: bool,
}

impl TrajectorySignals {
    /// Extract signals only when every tool result is typed and paired to a prior call.
    pub(crate) fn extract(request: &GatewayRequest) -> Option<Self> {
        if request
            .features()
            .contains(&RequestFeature::UnsupportedContent)
        {
            return None;
        }
        let mut call_states = HashMap::new();
        let mut signals = None;
        for message in request.messages() {
            let message = message.as_object()?;
            let role = message.get("role")?.as_str()?;
            if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                if role != "assistant" {
                    return None;
                }
                for call in tool_calls {
                    if call_states.len() == MAX_TRAJECTORY_TOOL_CALLS {
                        return None;
                    }
                    let id = valid_tool_call_id(call)?;
                    if call_states.insert(id, false).is_some() {
                        return None;
                    }
                }
            }
            if role != "tool" {
                continue;
            }
            let id = message
                .get("tool_call_id")?
                .as_str()
                .filter(|id| !id.is_empty())?;
            let resolved = call_states.get_mut(id)?;
            if *resolved {
                return None;
            }
            *resolved = true;
            let content = message.get("content")?.as_str()?;
            let result: TypedToolResult = serde_json::from_str(content).ok()?;
            if !result.trajectory.is_consistent() {
                return None;
            }
            let current = signals.get_or_insert(Self {
                error_severity: ErrorSeverity::None,
                clean_streak: 0,
                environment: EnvironmentEvidence::Unknown,
                test_status: TestStatus::NotRun,
                context_compacted: false,
            });
            current.error_severity = current.error_severity.max(result.trajectory.error_severity);
            current.clean_streak = match result.trajectory.outcome {
                ToolOutcome::Success => {
                    current.clean_streak.saturating_add(1).min(MAX_CLEAN_STREAK)
                }
                ToolOutcome::Failure => 0,
            };
            current.environment = current.environment.max(result.trajectory.environment);
            current.test_status = result.trajectory.test_status;
            current.context_compacted |= result.trajectory.context_compacted;
        }
        if call_states.values().all(|resolved| *resolved) {
            signals
        } else {
            None
        }
    }

    pub(crate) fn to_prompt_json(&self) -> String {
        serde_json::to_string(self).expect("closed trajectory signals always serialize")
    }

    pub(crate) const fn error_severity(&self) -> &'static str {
        self.error_severity.as_str()
    }

    pub(crate) const fn clean_streak(&self) -> u8 {
        self.clean_streak
    }

    pub(crate) const fn environment(&self) -> &'static str {
        self.environment.as_str()
    }

    pub(crate) const fn test_status(&self) -> &'static str {
        self.test_status.as_str()
    }

    pub(crate) const fn context_compacted(&self) -> bool {
        self.context_compacted
    }
}
