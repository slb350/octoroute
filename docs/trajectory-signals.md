# Deterministic trajectory signals

Octoroute can derive bounded execution evidence from explicitly typed tool
results. This is a shadow-only evaluation feature: the evidence is added to
the local semantic forecaster only in `semantic_mode = "shadow"`. It never
changes a route directly, and enforced forecasts remain byte-for-byte free of
trajectory context.

## Typed tool-result contract

A signal-bearing result must be paired with a preceding valid assistant
`tool_calls` entry through the same non-empty `tool_call_id`. Its tool-message
`content` is a JSON string with this strict envelope:

```json
{
  "type": "octoroute.trajectory/v1",
  "trajectory": {
    "outcome": "failure",
    "error_severity": "hard",
    "environment": "production",
    "test_status": "failed",
    "context_compacted": true
  },
  "result": {
    "error": "application-defined bounded result"
  }
}
```

The closed values are:

- `outcome`: `success` or `failure`;
- `error_severity`: `none`, `retryable`, or `hard`;
- `environment`: `unknown`, `development`, or `production`;
- `test_status`: `not_run`, `passed`, or `failed`; and
- `context_compacted`: a Boolean.

Success requires `error_severity = "none"`; failure requires `retryable` or
`hard`. Unknown envelope fields, inconsistent values, an ordinary untyped tool
result, an unmatched or repeated result, more than 64 visible tool calls,
malformed request history, or no completed tool pair makes Octoroute abstain
from trajectory extraction for the complete request. The application-owned
`result` value is preserved in the original request but is never copied into a
metric or safe trajectory log.

## Aggregation

Across the verified visible history, Octoroute produces only:

- the maximum observed error severity;
- the trailing success streak, capped at 32;
- the strongest environment evidence (`production` outranks `development`);
- the most recent test status; and
- whether any result reports context compaction.

The serialized evidence contains only those closed values. A shadow debug
event exposes them with the request ID so the external benchmark harness can
join signal-only, forecast-only, and combined evaluations. Tool-call IDs,
application results, prompts, and generated text are not logged.

Requests with tool history are eligible for this path only when the validated
local capability configuration enables `tools`. Otherwise the existing
capability gate routes or rejects them before semantic forecasting.
