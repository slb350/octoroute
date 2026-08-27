# Octoroute v3 runtime status

This document tracks the executable boundary of the v3 inference fabric. The
feature branch is the migration unit and will not merge until v3 is complete,
so preserving a parallel v2 runtime is not a release requirement.

## Executable now

A v3 configuration starts an authenticated OpenAI-compatible HTTP service with:

- `POST /v1/chat/completions`;
- `GET /v1/models`;
- liveness and readiness endpoints;
- bounded Prometheus exposition;
- request body, header, concurrency, and rate limits;
- virtual-model resolution and local-only privacy narrowing;
- ordered local-pool and provider admission with closed fallback triggers;
- member-specific health, slot, token-count, capability, and context checks;
- a lazy HTTP provider registry keyed only by validated configuration names;
- isolated provider credentials, concurrency permits, and timeouts;
- schema-preserving OpenAI-compatible request dispatch;
- explicit OpenRouter Auto request shaping;
- shared pre-commit streaming that retains the selected pool or provider permit
  until the response body is dropped;
- bounded route, target, pool, member, provider, upstream, model-revision, and
  request headers.

The OpenAI-compatible adapter supports z.ai, OpenRouter, direct OpenAI, and
other configured endpoints with the same wire contract. Environment credentials
and bounded argv credential commands are resolved lazily only after a provider
step is selected. Missing or invalid credentials fail closed before contacting
the provider and may continue only when the route explicitly allows the mapped
fallback trigger.

Anthropic-compatible providers and the Codex CLI are represented in the
registry but remain incompatible until their dedicated adapters are implemented.
They do not resolve credentials, execute commands, or receive prompts.

## Runtime invariants

- `X-Octoroute-Privacy: local-only` removes provider steps before dispatch.
- Local-only requests do not resolve cloud credentials or contact the registry.
- Pool and provider fallback is allowed only for each route's closed trigger set.
- Rate limits fall forward only when `rate_limited` is explicitly configured.
- Transport and server failures fall forward only before response commitment.
- Provider authentication failures and other committed non-retryable responses
  are returned instead of being hidden behind indiscriminate retries.
- No target switch is possible after the first client-visible body byte.
- Secrets are referenced by configured name and omitted from errors and logs.
- Physical endpoint identity is bounded configuration, not prompt-derived state.

## Next implementation boundary

1. add the Anthropic-compatible adapter with explicit message, tool, reasoning,
   and streaming translation;
2. add provider authentication/readiness probes with bounded cached state;
3. expand bounded provider metrics and fallback-class coverage;
4. implement the locked-down Codex CLI adapter and OpenAI response translation;
5. finish the v3-only startup/config migration and remove superseded v2 runtime
   code before the pull request is made ready for review.
