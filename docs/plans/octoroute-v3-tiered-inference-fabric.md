# Octoroute v3: Tiered Inference Fabric

Status: implementation branch

Branch: `feat/v3-tiered-inference-fabric`

## Goal

Turn Octoroute from a single-local-endpoint/OpenRouter gateway into a reusable
inference data plane for coding agents and other OpenAI-compatible clients:

```text
OpenCode / OpenAI-compatible clients
                |
                v
           Octoroute v3
                |
       +--------+---------+
       |        |         |
       v        v         v
 worker pool  optional   cloud/subscription
 replicas     local      providers
              supervisor
```

The intended steady-state policy is local-first, with cloud available. Bounded
work should normally remain on a configurable pool of equivalent local
endpoints.
Complex planning and review can use a higher-capability local supervisor when
one is available. A deliberately small fraction of work may escalate to a
configured cloud or subscription model when the quality difference matters.

When no local supervisor is enabled, the same `supervisor` virtual model uses
its configured provider chain. Adding a local supervisor later is therefore a
configuration change; clients and OpenCode stay as they are.

## Responsibility boundary

### OpenCode remains the agent control plane

OpenCode owns:

- task decomposition;
- choosing worker versus supervisor semantics;
- choosing the reasoning effort for each task;
- subagent sessions and context compaction;
- git worktrees, branches, tests, and merges;
- reviewing and integrating worker results;
- deciding when a failed worker should be retried or escalated.

Octoroute must not infer git or agent semantics from prompt text.

### Octoroute becomes the inference data plane

Octoroute owns:

- virtual model names and ordered route chains;
- local-pool health, capacity, and exact context admission;
- least-loaded worker selection with fair tie rotation;
- provider concurrency and health;
- OpenAI- and Anthropic-compatible HTTP adapters;
- subscription-backed command adapters such as Codex CLI;
- strict local-only privacy;
- pre-commit fallback only;
- bounded route reasons, headers, metrics, and auditability.

This keeps endpoint identity and provider credentials out of OpenCode prompts.
A subagent asks for `model: worker`; it does not need to know which physical
endpoint is idle.

## V3 virtual models

The example configuration defines reusable routing roles, so a client names a
role rather than a machine or a model release:

| Client model | Intended behavior |
| --- | --- |
| `auto` | Alias for the configured local-first default route |
| `worker` | Equivalent local worker endpoints, never cloud |
| `supervisor` | Optional local supervisor, then configured provider chain |
| `local` | Local worker and supervisor pools only, never cloud |
| `cloud-sota` | Deliberate non-local escalation |

OpenCode should normally choose `worker` or `supervisor` explicitly because it
already has the task plan. `auto` remains useful for ordinary OpenAI-compatible
clients and general prompts.

## Local pools

A local pool describes equivalent model servers with a shared model identity,
context contract, capabilities, and reasoning default. Each member has its own
URL, concurrency limit, enabled state, and priority. Pool size and hardware are operator choices,
outside the client contract.

The repository example uses multiple interchangeable members:

```text
pool: workers
model: configured coding model
context: configured per deployment
reasoning default: Medium
members:
  worker-0 -> local model endpoint
  worker-1 -> local model endpoint
  worker-N -> local model endpoint
```

Each example member is single-slot. Parallelism comes from independent model
replicas, so concurrent requests never compete for one endpoint's context cache
and compute bandwidth.

The initial selector is deterministic:

1. reject a disabled pool;
2. reject unsupported capabilities;
3. reject an exact input + output + safety budget that exceeds the pool context;
4. ignore disabled or unreported members;
5. ignore unhealthy members;
6. ignore members at `max_in_flight`;
7. select the lowest live load;
8. use configured priority, then a rotating cursor, to break ties.

A future sticky-session policy may prefer an existing member while its cached
prefix is valuable, but stickiness must never bypass health, capacity, or
context gates.

## Reasoning policy

V3 supports Low, Medium, High, and XHigh so Octoroute can preserve the settings
accepted by different clients and providers. The repository example uses
Medium for bounded worker tasks and XHigh for complex supervisor work. Low is
available for operators who prefer it, even though it is not the default policy
for this deployment.

Octoroute preserves or supplies the selected effort but does not infer task
complexity from prompt text. Agent runtimes such as OpenCode remain responsible
for choosing the appropriate reasoning level and escalating failed work.

## Provider types

### HTTP providers

V3 models OpenAI-compatible and Anthropic-compatible APIs as one HTTP backend
with an explicit wire protocol. The initial presets are copied from contracts
already exercised by Drep:

- OpenRouter at `https://openrouter.ai/api/v1`;
- z.ai GLM Coding Plan at `https://api.z.ai/api/coding/paas/v4`;
- Moonshot Kimi for Coding at `https://api.kimi.com/coding/v1` using the
  Anthropic protocol;
- direct OpenAI API at `https://api.openai.com/v1`.

Kimi's preset carries the endpoint-specific requirements observed in Drep:
`max_tokens` is required, a 200,000-token fallback is accepted, and no default
temperature is injected. OpenRouter has a distinct request profile so Octoroute
continues to own Auto Router policy fields, which take precedence over
conflicting client values.

HTTP credentials are referenced by environment-variable name or by a safe argv
credential command. Exactly one source is allowed. Raw keys never belong in
TOML or Debug output.

### Codex subscription backend

Codex is a separate backend that invokes the installed official Codex CLI using
ChatGPT-managed credentials, so it has no HTTP endpoint and no API key.

The implementation reuses Drep's security posture:

- probe the CLI and ChatGPT login through a bounded cached readiness check;
- never read, persist, or log the account token;
- clear the child environment and pass only an allowlist;
- force the ChatGPT login method;
- use ephemeral non-interactive execution;
- ignore user rules/config that could alter the gateway contract;
- disable tools, apps, hooks, memories, web search, and subagents unless a future
  adapter explicitly requires and safely exposes them;
- enforce a bounded timeout and output contract;
- parse structured JSONL events, never terminal prose.

The gateway adapter serializes the OpenAI chat request as data under a stateless
execution contract and translates the validated final event back into an OpenAI
response. Tool calls are preserved. Streaming requests receive one complete SSE
chunk plus `[DONE]`; the adapter does not claim token-by-token streaming.
Unsupported media and provider-specific plugin features skip the Codex target
as incompatible, preserving request semantics.

## Route chains and fallback

A route is an ordered list such as:

```text
supervisor:
  pool:supervisor-local
  provider:kimi
  provider:zai
  provider:openrouter
  provider:codex
```

Each target may appear only once. Route order is the provider preference
contract; provider entries intentionally have no independent priority field.

Validation requires every local pool to precede every provider. Once a prompt
has been disclosed to cloud, the same request may not fall back to a local
machine. This preserves a simple disclosure boundary and prevents surprising
cloud-to-local retries.

Each route has an explicit fallback allowlist. Initial reasons are:

- `busy`;
- `unhealthy`;
- `context_overflow`;
- `incompatible`;
- `rate_limited`;
- `precommit_failure`.

Fallback is legal only before the first client-visible body byte. No provider
or pool may weaken the pre-commit invariant.

A provider's model refusal, malformed output, authentication failure, or other
non-retryable contract error should not be indiscriminately hidden by fallback.
The adapter must classify failures into a closed set before the route executor
can continue.

## Privacy

`X-Octoroute-Privacy: local-only` remains authoritative regardless of the
requested virtual model.

For a route that allows cloud, local-only handling narrows the already validated
route to local-pool steps before admission. If no local target remains, the
request fails. A cloud-only route combined with local-only privacy is a direct
error.

Explicit local routes and local-only requests must never invoke:

- cloud semantic classification;
- provider health checks that disclose request data;
- API-key commands for cloud providers;
- Codex CLI;
- cloud fallback after a local failure.

Health probes may remain credentialed, but they never include prompt content.

## Semantic classification

The v3 runtime has no semantic forecaster. OpenCode remains authoritative for
worker/supervisor choices because it understands the task plan, while other
clients use explicit virtual routes or the configured `auto` alias.

A future classifier may observe explicit decisions and be evaluated against
real outcomes, but it must earn a new labeled evidence gate before influencing
routing. It is not part of the current runtime or configuration.

## Runtime replacement

The feature branch is the migration unit and will not merge until the v3
runtime is complete. `config.toml`, the generated CLI template, the binary, and
all public integration tests now use only version 3. Shared request, auth,
environment, HTTP-limit, and streaming primitives live under neutral or fabric
ownership; the superseded runtime and version switch have been removed.

This keeps one executable contract during implementation. A document that is
not the exact v3 schema fails closed in `FabricConfig`.

## Runtime implementation plan

### Phase 1: schema and deterministic policy

Implemented in the first branch commit:

- validated v3 document;
- virtual model route chains;
- local pool/member configuration;
- least-loaded rotating member selection;
- exact context and capability gates;
- local-only route narrowing;
- provider backend/protocol metadata;
- Drep-derived provider presets;
- tested multi-member worker and disabled-supervisor example.

### Phase 2: local pool leases

Implemented by refactoring the existing single-upstream admission path into:

```text
LlamaCppMember
  health cache
  slot probe
  input-token probe
  semaphore

LlamaCppPool
  member selector
  rotating cursor
  PoolLease(member identity + request bytes + permit)
```

The fabric transport dispatches using the URL and credential carried by the
lease. The permit remains held until the streamed body is dropped.

### Phase 3: HTTP provider registry

Implemented as a registry keyed by provider name. Each provider owns:

- isolated credentials;
- protocol adapter;
- concurrency semaphore;
- health/readiness state;
- timeout policy;
- request profile;
- bounded identity for headers and metrics.

OpenRouter-specific Auto Router mutation remains an explicit profile rather
than contaminating generic providers.

OpenAI-compatible providers preserve the request schema. Anthropic-compatible
providers explicitly translate messages, tools, reasoning, responses, errors,
and fragmented SSE. Cached body-free readiness probes resolve credentials only
on refresh and publish bounded outcomes.

### Phase 4: subscription command providers

Implemented with a provider lease that prepares a response before commitment
and a Codex CLI adapter using the Drep pattern. Command providers are
single-purpose adapters, never arbitrary shell strings. Configuration may name
an argv credential command for HTTP keys, but execution backends themselves
must be compiled/known kinds.

### Phase 5: route executor

Implemented by executing validated route steps in order and mapping closed
failure classes to each route's `fallback_on` set. Response headers include:

- `X-Octoroute-Route`;
- `X-Octoroute-Target`;
- `X-Octoroute-Provider`;
- `X-Octoroute-Model-Revision` for local targets;
- existing destination, reason, upstream, and request IDs.

Metrics must keep bounded labels. Physical member names are bounded config
values; prompts, session IDs, raw model output, credentials, and arbitrary
provider error strings are never labels.

### Phase 6: OpenCode integration

Implemented as one OpenAI-compatible base URL and stable model IDs. OpenCode subagents use
`worker`; the primary agent uses `supervisor`. OpenCode remains responsible for
worktree isolation and review.

The automated suite validates:

- simultaneous independent worker requests occupy distinct local endpoints;
- an additional worker admission reports busy without taking cloud fallback on
  the `worker` local-only route;
- exact-context admission rejects oversized input before dispatch;
- Low, Medium, High, and XHigh fields survive schema-preserving proxying;
- provider-specific request quirks are applied only to their providers;
- OpenCode-style function tools and fragmented Anthropic SSE round-trip through
  the public OpenAI contract;
- Codex diagnostic, filtered environment, ephemeral invocation, lifecycle, and
  OpenAI response translation round-trip through the provider executor;
- local-only never launches a subscription command or contacts cloud;
- provider admission failures continue only under their matching closed
  trigger;
- streaming bodies retain provider permits through completion or drop.

## Non-goals

V3 does not:

- create or manage git worktrees;
- decompose tasks;
- merge agent commits;
- use a semantic classifier to override an explicit OpenCode tier choice;
- tensor-parallelize one model across an endpoint pool;
- hide provider contract failures behind unlimited retries;
- promise that every subscription CLI can be losslessly represented as OpenAI
  chat completions.

## Completion criteria

The v3 branch is ready to merge when:

- all v3 unit, integration, lint, format, docs, audit, and benchmark gates pass;
- the binary and generated template accept only the exact v3 schema;
- configured local members are independently admitted and selected under load;
- every provider has protocol, credential, concurrency, and compatibility tests;
- local-only invariants are proven across every route and failure class;
- response streaming holds leases and forbids post-commit switching;
- OpenCode can use `worker`, `supervisor`, `local`, and `cloud-sota` through one
  endpoint;
- operator documentation and production deployment profiles are complete;
- representative traffic confirms the intended predominantly-local routing mix.
