# Octoroute v3 runtime status

This document tracks the executable boundary of the v3 inference fabric while
`config_version = 2` remains fully supported.

## Executable now

The binary selects the strict v2 or v3 parser from `config_version`. A v3
configuration starts an authenticated OpenAI-compatible HTTP service with:

- `POST /v1/chat/completions`;
- `GET /v1/models`;
- liveness and readiness endpoints;
- an initial bounded Prometheus exposition;
- request body, header, concurrency, and rate limits;
- virtual-model resolution and local-only privacy narrowing;
- ordered local-pool admission and fallback;
- member-specific health, slot, token-count, capability, and context checks;
- shared pre-commit streaming that retains the selected member lease until the
  response body is dropped;
- bounded route, pool, member, upstream, model-revision, and request headers.

The `worker` and `local` routes can therefore execute against configured local
pools. Routes that reach a provider step fail closed with
`provider_runtime_unavailable`. They do not resolve provider credentials,
contact a provider, or pretend that provider compatibility exists.

## Preserved invariants

- V2 parsing, routing, transport, and tests remain independent and unchanged.
- A v3 document is never partially interpreted as v2.
- `X-Octoroute-Privacy: local-only` removes provider steps before dispatch.
- Local pool fallback is allowed only for the route's closed trigger set.
- No target switch is possible after the first client-visible body byte.
- Secrets are resolved by configured name and are not echoed by parse or startup
  errors.
- Physical endpoint identity is bounded configuration, not prompt-derived state.

## Next implementation boundary

The next slice is the HTTP provider registry and route-executor integration:

1. build isolated provider instances with credentials, permits, timeout policy,
   health state, protocol, model, and request profile;
2. add a schema-preserving OpenAI-compatible adapter;
3. add an Anthropic-compatible adapter with explicit translation and verified
   compatibility limits;
4. preserve OpenRouter Auto mutation as an explicit provider profile;
5. classify pre-commit provider failures into the closed fallback triggers;
6. add provider readiness, response headers, bounded metrics, and integration
   tests;
7. prove that local-only requests never resolve credentials or contact the
   registry.

Codex CLI remains a separate later adapter because its authentication,
execution, and response contract differ from HTTP providers.
