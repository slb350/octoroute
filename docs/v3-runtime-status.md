# Octoroute v3 runtime status

This document tracks the executable boundary of the v3 inference fabric. The
feature branch is the migration unit and will not merge until v3 is complete.
The repository now has one runtime, parser, generated template, and public HTTP
contract: version 3.

## Executable now

A v3 configuration starts an authenticated OpenAI-compatible service with:

- chat completions, model listing, liveness, readiness, and metrics;
- bounded request body, headers, concurrency, and rate;
- deterministic virtual routes and request-level local-only narrowing;
- ordered local-pool and provider admission with closed fallback triggers;
- member-specific health, slot, token-count, capability, and context checks;
- least-loaded selection with rotating ties;
- a lazy HTTP provider registry keyed by validated names;
- isolated provider credentials, permits, and deadlines;
- schema-preserving OpenAI-compatible provider dispatch;
- explicit OpenRouter Auto request shaping;
- held-first-byte streaming that retains the selected member/provider and
  inbound permits until the response body is dropped;
- bounded route, target, pool, member, provider, upstream, model-revision, and
  request headers.

OpenAI-compatible dispatch supports z.ai, OpenRouter, direct OpenAI, and other
configured endpoints with the same wire contract. Environment credentials and
bounded argv credential commands resolve lazily only after a provider step is
selected. Missing or invalid credentials fail closed before provider contact
and continue only when route policy explicitly allows the mapped trigger.

The default `config.toml`, laptop profile, CLI generator, executable startup,
crate version, documentation, and integration tests are v3-only. Superseded
single-local/OpenRouter routing, semantic forecasting, calibration, runtime
version dispatch, and their tests have been removed. Shared security and HTTP
primitives were retained under neutral or fabric ownership.

## Runtime invariants

- `X-Octoroute-Privacy: local-only` removes provider steps before dispatch.
- Local-only requests do not resolve cloud credentials or contact providers.
- Local targets always precede providers in a route.
- Fallback is allowed only for the route's closed trigger set.
- Rate limits fall forward only when `rate_limited` is configured.
- Transport and server failures fall forward only before response commitment.
- Provider authentication failures and other committed non-retryable responses
  are returned instead of hidden behind indiscriminate retries.
- No target switch is possible after the first client-visible body byte.
- Secrets are referenced by configured name and omitted from errors and logs.
- Physical endpoint identity is bounded configuration, not prompt-derived state.

## Represented but incompatible

Anthropic-compatible HTTP and Codex CLI providers remain explicit schema and
registry variants. They do not resolve credentials, execute commands, or
receive prompts until dedicated adapters advertise compatibility.

## Next implementation boundary

1. add the Anthropic-compatible adapter with explicit message, tool, reasoning,
   error, and streaming translation;
2. add provider authentication/readiness probes with bounded cached state;
3. expand bounded provider metrics and fallback-class coverage;
4. implement the locked-down Codex CLI adapter and OpenAI response translation;
5. run representative OpenCode integration and deployment canaries.
