# Semantic forecast calibration

Octoroute calibrates semantic routing offline. The command does not start the
gateway, load `config.toml` or `.env`, resolve credentials, or call Strix or
OpenRouter.

```bash
octoroute calibrate \
  --input forecasts.jsonl \
  --output calibration-report.json \
  --grid-step 0.05
```

Omit `--output` to write JSON to stdout. Existing output files are never
overwritten. Input is limited to 64 MiB and 100,000 nonblank records.

## Artifact contract

Each JSONL line is one labeled challenge:

```json
{
  "challenge_id": "cql-tier-1-004",
  "model_alias": "strixtea",
  "model_revision": "agents-a1-q8_0",
  "capability_card_version": "octoroute-strix-capability-card/v2",
  "capability_card_fingerprint": "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881",
  "p_local_success": 0.23,
  "capability_boundary": "unsupported",
  "primary_rule": "known_local_limit",
  "local_success": false,
  "previous_cloud_decision": false,
  "cloud_success": true,
  "routing_latency_ms": 912,
  "cloud_cost_usd": 0.0184
}
```

Required fields are `challenge_id`, `model_alias`, the immutable
`model_revision`, `capability_card_version`, the lowercase SHA-256
`capability_card_fingerprint`, `p_local_success`, `capability_boundary`,
`primary_rule`, and `local_success`. The remaining fields are optional. Unknown
fields, duplicate identifiers, non-finite or out-of-range numbers, identifiers
over 128 bytes, non-visible or whitespace-bearing model revisions, malformed
fingerprints, inconsistent rule/boundary pairs, and mixed
alias/revision/card/fingerprint populations fail the complete run.
Covered `cloud_cost_usd` values must be from zero through $1,000,000 per
challenge, which guarantees every bounded aggregate remains finite.

The benchmark harness obtains the forecast fields from the bounded shadow log
event keyed by the response `X-Octoroute-Request-Id`, then joins the standalone
local and cloud outcomes. The event contains the immutable model revision,
probability, boundary, primary rule, derived threshold, policy destination,
card version, and the SHA-256 fingerprint of the exact rendered capability
card. The harness takes the model alias from its validated run configuration.
The event never contains the prompt, crux, response text, credentials, or
provider errors.

## Report

The report includes:

- ten fixed calibration bins and Brier score; bins match Prometheus's
  upper-inclusive deciles, starting with `[0.0, 0.1]` and continuing with
  `(0.1, 0.2]` through `(0.9, 1.0]`;
- an always-local baseline and optional previous-binary-policy comparison;
- every valid base-threshold/boundary-step pair on the requested grid;
- the highest-accuracy candidate, with deterministic lower-spill tie-breaking;
- accuracy, cloud precision and recall, false escalations, missed rescues, and
  local/cloud route counts;
- covered successful rescues and failed cloud routes;
- covered estimated cloud cost; and
- observed average semantic-routing latency.

`beats_always_local` is evidence about the supplied labeled artifact only. It
does not enable enforced mode or change configuration. Report schema version 3
carries the single validated model alias, immutable model revision, card
revision, and rendered-card fingerprint for the complete artifact. The
benchmark dataset and collection procedure still require human review before
adopting a threshold.
