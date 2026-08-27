# Architecture

Octoroute v3 separates a stable client contract from physical inference
targets.

```text
OpenAI-compatible request
          |
   authenticate + bound
          |
    resolve virtual route
          |
  apply local-only privacy
          |
   ordered target executor
      /             \
local pool       provider registry
```

## Static configuration boundary

`FabricConfig` validates the complete document before startup:

- stable names and cross-references;
- local-before-provider ordering;
- route privacy versus target kinds;
- URL, environment-name, context, concurrency, and timeout bounds;
- exactly one credential source for each HTTP provider;
- provider-kind-specific fields.

The parser denies unknown fields and omits source values from errors.

## Request boundary

`GatewayRequest` retains the parsed OpenAI object and rewrites only fields owned
by the selected destination. It validates the minimum envelope and lazily
infers features needed for local admission. Unknown or malformed local feature
shapes become `incompatible`; they are never guessed safe.

Virtual routing is deterministic. Octoroute does not infer agent roles, git
semantics, or task difficulty from prompt text. Clients such as OpenCode choose
`worker` or `supervisor`; ordinary clients can use `auto`.

## Local pool runtime

Each `LlamaCppPool` owns equivalent members. A member owns:

- an isolated HTTP client;
- resolved optional local credential;
- health, slot, input-token, and chat URLs;
- a concurrency semaphore;
- a short bounded health cache.

Admission filters capabilities and context, then tries members by live load,
configured priority, and rotating tie order. Health, slot availability, and
exact input tokens are checked before a `PoolLease` is issued.

The lease carries physical identity, destination request bytes, and its permit.
The transport moves that permit into the response body stream, so capacity is
released only when the body completes or is dropped.

## Provider registry

`ProviderRegistry` is keyed only by validated provider names. Construction does
not read provider credentials or run commands.

An OpenAI-compatible provider owns:

- normalized chat-completions URL;
- isolated lazy credential source;
- request-shaping configuration;
- timeout;
- concurrency semaphore.

Credential resolution occurs after the executor selects the provider and
acquires its permit. Unsupported protocols and provider kinds return
`incompatible` without resolving credentials.

The OpenAI adapter clones the schema-preserving body, patches the model, and
applies configured defaults only when the caller omitted them. OpenRouter Auto
is an explicit profile rather than behavior embedded in every provider.

## Disclosure and fallback

The executor first resolves the virtual route, then applies request-level
privacy. `local-only` removes every provider step before any admission or
credential operation.

Configuration requires local steps before providers. Once a request is sent to
a provider, no local target can appear later in the same chain.

Every continuation is authorized by the route's closed `fallback_on` set.
Target admission states map to `busy`, `unhealthy`, `context_overflow`, or
`incompatible`; HTTP 429 maps only to `rate_limited`; transport and upstream
server failures map only to `precommit_failure`.

## Commitment point

The production transport obtains response headers and buffers the first body
chunk before returning a prepared response. Until then, the executor may drop
the response and select another target when policy allows. After decoration and
return to Axum, the target is committed and the remainder streams opaquely.

Safe upstream response headers are allowlisted. Request IDs, route identity,
pool/member/provider names, and model revisions come from generated or validated
bounded values.

## Runtime HTTP controls

Inbound processing applies these controls before route execution:

1. aggregate header bound;
2. constant-time bearer authentication;
3. fixed-window authenticated request rate limit;
4. inbound concurrency permit;
5. bounded body read;
6. minimum request validation.

The inbound permit is also held by the response body stream.

## Remaining adapters

Anthropic-compatible HTTP and Codex CLI are explicit schema/runtime variants,
not special cases hidden inside the OpenAI adapter. Until their translation and
security contracts are implemented, registry entries remain incompatible and
receive no prompt data.
