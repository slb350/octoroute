# Octoroute v3 runtime status

This document tracks the executable boundary of the v3 inference fabric. The
feature branch is the migration unit and will not merge until v3 is complete.
The repository now has one runtime, parser, generated template, and public HTTP
contract: version 3.

## Executable now

A v3 configuration starts an authenticated OpenAI-compatible service with:

- chat completions, model listing, liveness, readiness, and metrics;
- bounded request body, headers, concurrency, and rate;
- bounded virtual/physical model identifiers, credential argv, upstream
  deadlines, and semaphore sizes;
- deterministic non-repeating virtual routes, a reserved `auto` alias, and
  request-level local-only narrowing;
- ordered local-pool and provider admission with closed fallback triggers;
- member-specific health, slot, token-count, capability, and context checks;
- effective local-pool reasoning defaults when callers omit reasoning controls;
- least-loaded selection with rotating ties;
- a lazy HTTP provider registry keyed by validated names;
- isolated provider credentials, permits, and deadlines;
- schema-preserving OpenAI-compatible provider dispatch;
- explicit Anthropic Messages request, response, tool, reasoning, error, and
  incremental SSE translation;
- locked-down, subscription-backed Codex CLI dispatch with ChatGPT-managed
  authentication and structured output validation;
- cached bounded provider authentication/reachability probes;
- fixed-label provider admission, response, fallback, and probe counters;
- explicit OpenRouter Auto request shaping;
- held-first-byte streaming that retains the selected member/provider and
  inbound permits until the response body is dropped;
- bounded route, target, pool, member, provider, upstream, model-revision, and
  request headers.

OpenAI-compatible dispatch supports z.ai, OpenRouter, direct OpenAI, and other
configured endpoints with the same wire contract. Environment credentials and
bounded argv credential commands resolve lazily after a provider step is
selected or an explicit readiness request expires that provider's probe cache.
Missing or invalid credentials fail closed before chat-provider contact and
continue only when route policy explicitly allows the mapped trigger.

The default `config.toml`, laptop profile, CLI generator, executable startup,
crate version, documentation, and integration tests are v3-only. Superseded
single-local/OpenRouter routing, semantic forecasting, calibration, runtime
version dispatch, and their tests have been removed. Shared security and HTTP
primitives were retained under neutral or fabric ownership.

## Runtime invariants

- `X-Octoroute-Privacy: local-only` removes provider steps before dispatch.
- Local-only requests do not resolve cloud credentials or contact providers.
- Readiness sends no prompt data; its bounded provider probes are the only
  credential-resolution path outside selected provider admission.
- Local targets always precede providers in a route.
- Fallback is allowed only for the route's closed trigger set.
- Rate limits fall forward only when `rate_limited` is configured.
- Transport and server failures fall forward only before response commitment.
- Provider authentication failures and other committed non-retryable responses
  are returned instead of hidden behind indiscriminate retries.
- No target switch is possible after the first client-visible body byte.
- Secrets are referenced by configured name and omitted from errors and logs.
- Physical endpoint identity is bounded configuration, not prompt-derived state.
- Provider preference is exactly the configured route order; no inert priority
  field can conflict with it.

## Provider runtime boundary

The registry now has three executable variants: OpenAI-compatible HTTP,
Anthropic-compatible HTTP, and Codex CLI. Compatibility remains request-scoped:
an adapter rejects features without a verified mapping before external prompt
disclosure and can continue only through the route's explicit `incompatible`
fallback trigger.

Representative OpenCode-style tool/SSE translation, fake-Codex lifecycle and
environment isolation, adapter-incompatible multi-choice rejection, readiness
caching, provider fallback metrics, and
local-only zero-contact behavior are covered by the test suite. Operators can
run `scripts/v3-canary.sh` for liveness, readiness, model discovery, local-only
non-streaming/streaming completions, and an optional explicit provider route.
