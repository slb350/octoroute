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
- reserved `auto` alias and non-repeating route targets;
- route privacy versus target kinds;
- URL, model, environment-name, credential-argv, context, concurrency, and
  timeout bounds;
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

- a pool-scoped HTTP client handle;
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

An HTTP provider owns:

- normalized protocol-specific request and models URLs;
- isolated lazy credential source;
- an OpenAI-preserving or Anthropic-translating adapter;
- timeout;
- bounded cached readiness state;
- concurrency semaphore.

Provider preference is represented only by route order. There is no separate
provider priority that could disagree with the executable chain.

Credential resolution occurs after the executor selects the provider and
acquires its permit. The only other resolution path is an explicit readiness
request after that provider's probe cache expires. Readiness is body-free,
bounded, and coalesced per provider.

The OpenAI adapter clones the schema-preserving body, patches the model, and
applies configured defaults only when the caller omitted them. OpenRouter Auto is an explicit profile,
so the behavior stays out of every other provider.

The Anthropic adapter explicitly maps system/developer messages, alternating
user/assistant text, tool definitions and history, sampling controls, output
budget, and reasoning effort into Messages. It translates bounded non-streaming
responses and incremental SSE back into OpenAI response, tool-call, reasoning,
usage, finish-reason, and error shapes. Features without a verified mapping
fail as `incompatible` before credential resolution or prompt disclosure.

The Codex provider is a separate command runtime. Admission serializes the
OpenAI request as data for a stateless backend contract. Execution uses the
official CLI with ChatGPT-managed login, a filtered environment, an empty
temporary working directory, read-only sandboxing, ephemeral state, ignored
user config/rules, and tools, apps, web, hooks, memories, and subagents
disabled. A strict bounded JSONL lifecycle and output schema must validate
before the response can commit.

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

The production transport obtains response headers and buffers the first usable
body chunk before returning a prepared response. Anthropic streaming buffers
the first translated event; Anthropic non-streaming and Codex CLI execution
validate their complete bounded result before commitment. Until then, the
executor may drop the response and select another target when policy allows.
After decoration and return to Axum, the target is committed.

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

## Adapter isolation

OpenAI-compatible HTTP, Anthropic-compatible HTTP, and Codex CLI are explicit
runtime variants, declared in configuration and never inferred from an endpoint
or a prompt. Each adapter advertises only verified request features, uses its own
authentication mechanism, and fails closed before disclosure when a request
cannot be translated safely.
