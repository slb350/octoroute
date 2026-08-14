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
  "capability_card_version": "octoroute-strix-capability-card/v1",
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

Required fields are `challenge_id`, `model_alias`, `capability_card_version`,
the lowercase SHA-256 `capability_card_fingerprint`, `p_local_success`,
`capability_boundary`, `primary_rule`, and `local_success`. The remaining
fields are optional. Unknown fields, duplicate identifiers, non-finite or
out-of-range numbers, oversized identifiers, malformed fingerprints,
inconsistent rule/boundary pairs, and mixed model/card/fingerprint populations
fail the complete run.

The benchmark harness obtains the forecast fields from the bounded shadow log
event keyed by the response `X-Request-Id`, then joins the standalone local and
cloud outcomes. The event contains probability, boundary, primary rule,
derived threshold, policy destination, card version, and the SHA-256
fingerprint of the exact rendered capability card. It never contains the
prompt, crux, response text, credentials, or provider errors.

## Report

The report includes:

- ten fixed calibration bins and Brier score; bins are lower-inclusive and
  upper-exclusive except for the final `[0.9, 1.0]` bin;
- an always-local baseline and optional previous-binary-policy comparison;
- every valid base-threshold/boundary-step pair on the requested grid;
- the highest-accuracy candidate, with deterministic lower-spill tie-breaking;
- accuracy, cloud precision and recall, false escalations, missed rescues, and
  local/cloud route counts;
- covered successful rescues and failed cloud routes;
- covered estimated cloud cost; and
- observed average semantic-routing latency.

`beats_always_local` is evidence about the supplied labeled artifact only. It
does not enable enforced mode or change configuration. Report schema version 2
carries the single validated model, card revision, and rendered-card
fingerprint for the complete artifact. The benchmark dataset and collection
procedure still require human review before adopting a threshold.
