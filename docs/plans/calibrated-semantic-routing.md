# Calibrated semantic routing

Status: implementation in progress; forecast-policy foundation complete

Decision date: 2026-08-14

Inspiration: NVIDIA NeMo Switchyard capability and stage routing. Octoroute
borrows the design ideas, not Switchyard source code. Octoroute remains an MIT
licensed, local-first Strix/OpenRouter gateway rather than becoming a generic
multi-provider router.

Upstream evidence reviewed on 2026-08-14:

- [Switchyard repository](https://github.com/NVIDIA-NeMo/Switchyard), commit
  `a17efa945811302f3c33a68c9a28eab55ad4d08c`;
- [LLM classifier routing](https://nvidia-nemo.github.io/Switchyard/routing_algorithms/llm_classifier_routing/);
- [stage routing](https://nvidia-nemo.github.io/Switchyard/routing_algorithms/stage_routing/);
- [known issues](https://nvidia-nemo.github.io/Switchyard/known_issues/).

## Problem

Octoroute's semantic router currently asks Strix to emit a binary `local` or
`cloud` destination. The 2026-07-26 labeled replay found 44% routing accuracy,
compared with 73% for always-local on the same compatible tasks. It produced
31 unnecessary escalations and missed 8 tasks that the local model did not
solve. Classification also adds roughly 760-1500 ms to every classified
request.

The binary verdict combines two distinct responsibilities:

1. forecast whether the configured local model can complete the task; and
2. decide how much forecasted risk justifies cloud cost and disclosure.

The prompt also describes only "the configured private local model". It does
not name Strix, state its enabled capabilities, or give the judge a
benchmark-backed capability boundary. Strict JSON proves only that a verdict
has the expected shape; it does not calibrate the judgment.

## Goals

- Make the local judge forecast whole-task local success instead of selecting
  a destination directly.
- Apply routing thresholds in deterministic Rust policy that can be tuned and
  replayed without another model call.
- Give the judge a versioned, Strix-specific qualitative capability card based
  on measured evidence rather than prompt length or technical vocabulary.
- Preserve the existing disabled/shadow/enforced evidence gate. Shadow remains
  the default.
- Preserve explicit local, explicit cloud, and `local-only` behavior exactly.
- Preserve local health, slot, exact-token, context, concurrency, cancellation,
  first-byte commitment, credential, and opaque proxy invariants.
- Record only bounded forecast dimensions in metrics and safe logs.
- Make the next labeled evaluation capable of measuring probability
  calibration as well as binary route quality.

## Non-goals

- General multi-provider or arbitrary model-graph routing.
- OpenAI/Anthropic protocol translation.
- A cloud classifier or a configurable cloud judge.
- Buffering a completed local answer so another model can judge it.
- Switching upstream after the first response byte.
- Session affinity or trajectory-based enforcement in the first milestone.
- Copying Switchyard's source, prompts, error-pattern tables, or default
  thresholds.
- Enabling semantic enforcement before the evidence gate passes.

## Decision

Automatic compatible traffic keeps the existing routing sequence, but the
semantic stage produces a forecast:

```text
deterministic intent/privacy/capability gates
  -> optional local semantic forecast
       -> deterministic threshold policy
            -> local admission
            -> or OpenRouter Auto
```

The local judge returns a strict object equivalent to:

```json
{
  "p_local_success": 0.72,
  "capability_boundary": "uncertain",
  "primary_rule": "bounded_verification",
  "crux": "Requires a correct recursive query across an underspecified schema"
}
```

`p_local_success` forecasts the probability that the configured Strix model
will complete the whole task successfully in one run under the visible
request, enabled local capabilities, and normal execution budget. It is not
confidence in the forecast, a route recommendation, or a cost judgment.

`capability_boundary` is a closed enum:

- `supported`: the capability card directly supports the task's crux;
- `uncertain`: material ambiguity remains;
- `unsupported`: the card identifies a material local limitation;
- `unmatched`: no card rule adequately covers the crux.

`primary_rule` is a closed, versioned enum from the checked-in Strix capability
card. `crux` is required, bounded, and never used as a metric label or routine
log field.

The deterministic policy computes:

```text
required_probability =
  base_threshold + capability_boundary.steps * threshold_step

supported.steps = 0
uncertain.steps = 1
unmatched.steps = 1
unsupported.steps = 2
```

The forecast selects local when `p_local_success >= required_probability` and
cloud otherwise. Invalid, inconsistent, truncated, or out-of-range forecasts
retain the existing mode-dependent failure behavior: shadow continues local
admission; enforced fails safely to cloud.

Threshold values must be finite, within `[0, 1]`, and satisfy
`base_threshold + 2 * threshold_step <= 1`. Defaults are provisional until the
labeled replay selects them. Changing them does not require another judge run
when saved forecast artifacts are available.

## Capability card

The first capability-card revision will:

- identify the configured local model alias and the Strix deployment role;
- describe only capabilities enabled by validated Octoroute configuration;
- distinguish deterministic verification from merely plausible output;
- state measured limitations from the existing benchmark, including database
  tasks where local outcomes were poor;
- state that short wording, declarative framing, or an unfamiliar domain alone
  is not evidence of task ease or difficulty;
- prohibit invented tools, repository state, validators, success rates, and
  hidden context; and
- be versioned so a model or capability change requires explicit review and a
  new evaluation.

The card remains local configuration/code, is sent only to Strix, and contains
no prompt or credential data.

## Request context

Milestone one keeps the complete validated message array as untrusted,
delimited JSON so forecast behavior can be compared directly with the existing
router. A later measured optimization may retain system/developer framing, the
opening user task, and a bounded recent window while keeping tool-call/result
pairs intact. Octoroute will not adopt Switchyard's omission of client system
and developer context without evidence that doing so is safe and accurate.

## Configuration

Add validated routing fields:

```toml
[routing]
semantic_mode = "shadow"
decision_timeout_ms = 30000
local_success_threshold = 0.50
boundary_threshold_step = 0.10
```

These defaults are starting values for shadow evaluation, not a claim that the
forecast is calibrated. Existing configuration without the new fields remains
valid through defaults. Configuration validation rejects non-finite,
out-of-range, or internally inconsistent thresholds.

The classifier output-token limit remains an internal bounded constant during
the first milestone. It will increase only enough to carry the strict forecast
object and will be checked against real Strix output before becoming a public
configuration surface.

## Observability and evaluation artifacts

Keep `octoroute_semantic_decisions_total{mode,outcome}` for compatibility,
where `outcome` remains `local`, `cloud`, or `failure` after deterministic
policy.

Add a bounded forecast histogram keyed only by semantic mode and capability
boundary. Do not put model output, `crux`, prompts, arbitrary model names,
request IDs, or raw errors in metric labels.

The external benchmark harness should save, outside production metrics:

- challenge identifier;
- `p_local_success`;
- boundary and rule enum;
- derived threshold and decision;
- actual local outcome; and
- actual cloud outcome when available.

The evidence report must include calibration bins, Brier score, precision,
recall, accuracy, false escalation count, missed-rescue count, route split,
latency, and estimated cloud spend. Threshold sweeps operate on the saved
forecasts.

## Milestones

### 1. Forecast and policy foundation

- [x] Add typed forecast, boundary, rule, and deterministic policy types.
- [x] Add RED tests for valid threshold decisions and malformed forecasts.
- [x] Change the Strix response schema and parser from binary destination to the
  forecast object.
- [x] Preserve mode-specific routing and failure behavior.
- [x] Add threshold configuration and validation tests.
- [x] Update request-shape and semantic-mode integration tests.

### 2. Capability card and safe observability

- [x] Add the versioned Strix capability card and forecast instructions.
- [x] Include configured local identity and enabled capabilities.
- [x] Add bounded probability/boundary metrics without logging generated text.
- [x] Document the new fields, metrics, and prompt contract.

### 3. Labeled replay and calibration gate

- Extend the existing Strix benchmark harness to retain raw forecast fields.
- Replay the existing labeled challenge set with the same model and
  temperature.
- Sweep deterministic thresholds offline.
- Compare against always-local and the previous binary classifier.
- Keep enforcement blocked unless the selected policy beats the compatible
  baseline on declared quality and cost criteria.

### 4. Deterministic trajectory signals

- Extract signals only from verified typed tool-call/result history.
- Begin with shadow-only error severity, clean streak, production, clean test,
  and context-compaction evidence.
- Abstain when history is absent, malformed, or unsupported locally.
- Evaluate signal-only, forecast-only, and combined policies separately.

### 5. Optional operational refinements

- Consider a bounded shadow sample rate after enough representative evidence
  exists, while benchmark runs remain at full sampling.
- Consider a bounded, hashed, TTL-based cloud-only session latch after repeated
  hard evidence. Explicit local and local-only intent always override it, and
  local decisions are never latched.

## Test contract

Regression coverage must prove:

- probability equal to the applicable threshold selects local;
- probability below the applicable threshold selects cloud;
- boundary steps monotonically raise the required local probability;
- unknown rules, empty or oversized cruxes, inconsistent rule/boundary pairs,
  and out-of-range probabilities are rejected;
- the request uses strict structured output, thinking disabled, bounded output,
  the configured local model, and untrusted delimited conversation data;
- shadow forecasts never select the actual destination;
- enforced forecasts select cloud only through deterministic policy;
- invalid forecasts preserve shadow/enforced failure behavior;
- disabled, explicit local, explicit cloud, and local-only traffic do not invoke
  the semantic judge unexpectedly;
- existing local admission and pre-commit fallback behavior remains unchanged;
- only bounded enum-derived metric labels are introduced; and
- all existing authentication, request limit, opaque proxy, SSE, cancellation,
  and credential-isolation tests remain green.

## Completion gate

Implementation is complete only after focused RED/GREEN tests, formatting,
repository-wide clippy and tests, no-default-features tests, documentation,
Rustdoc, audit, benchmark compilation, `/simplify`, the repository LOC audit,
and an updated `AGENTS.md`. Passing implementation tests does not authorize
semantic enforcement, publication, deployment, commit, or push.
