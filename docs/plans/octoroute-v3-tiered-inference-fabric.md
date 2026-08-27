# Octoroute v3: Tiered Inference Fabric

Status: implementation branch

Branch: `feat/v3-tiered-inference-fabric`

## Goal

Turn Octoroute from a one-local-model/OpenRouter gateway into the inference data
plane for a personal coding cluster:

```text
OpenCode / OpenAI-compatible clients
                |
                v
           Octoroute v3
                |
       +--------+---------+
       |        |         |
       v        v         v
 worker pool  local     cloud/subscription
 3x RTX 3080  supervisor  providers
 Qwen3.8-27B  M5 Ultra   Kimi / z.ai /
                              OpenRouter /
                              OpenAI / Codex
```

The intended steady-state policy is local-first, not local-only. Most bounded
work should remain on the three Qwen workers. Complex planning and review should
use the local M5 Ultra supervisor when available. A deliberately small fraction
of tasks may escalate to a cloud SOTA model when the quality difference matters.

Before the M5 Ultra arrives, the same `supervisor` virtual model should use a
configured subscription/API chain. Enabling the Ultra later must be a config
change, not a client or OpenCode rewrite.

## Responsibility boundary

### OpenCode remains the agent control plane

OpenCode owns:

- task decomposition;
- choosing worker versus supervisor semantics;
- Medium versus XHigh reasoning effort;
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

This keeps GPU identity and provider credentials out of OpenCode prompts. A
subagent asks for `model: worker`; it does not need to know which physical GPU
is idle.

## V3 virtual models

The example configuration defines these initial routes:

| Client model | Intended behavior |
| --- | --- |
| `auto` | Alias for the configured local-first default route |
| `worker` | Three-card Qwen worker pool, never cloud |
| `supervisor` | Local Ultra when enabled, then configured cloud/subscription chain |
| `local` | Worker pool and local supervisor only, never cloud |
| `cloud-sota` | Deliberate non-local escalation |

OpenCode should normally choose `worker` or `supervisor` explicitly because it
already has the task plan. `auto` remains useful for ordinary OpenAI-compatible
clients and general prompts.

## Local pools

A local pool describes equivalent model servers with a shared model identity,
context contract, capabilities, and reasoning default. Each physical member has
its own URL, concurrency limit, enabled state, and priority.

The initial worker pool is:

```text
pool: workers
model: Qwen3.8-27B Unsloth UD-Q4_K_M
context: 131,072
reasoning default: Medium
members:
  worker-0 -> RTX 3080 20 GB
  worker-1 -> RTX 3080 20 GB
  worker-2 -> RTX 3080 20 GB
```

Each member remains single-slot. Parallelism comes from three independent model
replicas rather than multiple concurrent slots competing for one card's KV
cache and bandwidth.

The initial selector is deterministic:

1. reject a disabled pool;
2. reject unsupported capabilities;
3. reject an exact input + output + safety budget that exceeds the pool context;
4. ignore disabled or unreported members;
5. ignore unhealthy members;
6. ignore members at `max_in_flight`;
7. select the lowest live load;
8. use configured priority, then a rotating cursor, to break ties.

A future sticky-session policy may prefer an existing member while its KV prefix
is valuable, but stickiness must never bypass health, capacity, or context gates.

## Reasoning policy

V3 deliberately does not expose Low reasoning as a policy value. Real-world
Qwen3.8 coding tests showed that Low can consume more total tokens by repeatedly
iterating toward a complete answer.

The intended policy is:

- Medium for easy and bounded implementation work;
- XHigh for complex debugging, architecture, unfamiliar code, or a retry after a
  failed Medium attempt;
- High remains available for providers whose native contract uses it;
- Octoroute preserves or supplies the chosen setting but does not classify task
  complexity itself.

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
`max_tokens` is required, a 200,000-token fallback is accepted, and no
temperature is sent. OpenRouter has a distinct request profile so Octoroute can
continue owning Auto Router policy fields instead of trusting conflicting
client values.

HTTP credentials are referenced by environment-variable name or by a safe argv
credential command. Exactly one source is allowed. Raw keys never belong in
TOML or Debug output.

### Codex subscription backend

Codex is not an HTTP API-key provider in this design. It is a separate backend
that invokes the installed official Codex CLI using ChatGPT-managed
credentials.

The implementation should reuse Drep's security posture:

- probe the CLI and ChatGPT login once per process;
- never read, persist, or log the account token;
- clear the child environment and pass only an allowlist;
- force the ChatGPT login method;
- use ephemeral non-interactive execution;
- ignore user rules/config that could alter the gateway contract;
- disable tools, apps, hooks, memories, web search, and subagents unless a future
  adapter explicitly requires and safely exposes them;
- enforce a bounded timeout and output contract;
- parse structured JSONL events rather than scraping terminal prose.

The gateway adapter still has to translate an OpenAI chat request into the Codex
execution contract and translate the final event back into an OpenAI response.
Streaming and tool-call compatibility must be advertised only after they are
verified. Until then, incompatible requests skip the Codex target instead of
silently losing semantics.

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

Fallback is still legal only before the first client-visible body byte. No v3
provider or pool may weaken the v2 pre-commit invariant.

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

## Semantic routing

V2's semantic forecaster remains shadow-only during the v3 migration. The
existing measured result, 44% routing accuracy versus a 73% always-local
baseline with roughly 760-1500 ms of added latency, is not strong enough to make
worker/supervisor/cloud decisions authoritative.

OpenCode is initially authoritative because it understands the task plan. V3
may later record a shadow prediction for each explicit decision and calibrate a
new classifier against real outcomes. Enforcement requires a new labeled gate;
it is not inherited from v2 merely because the configuration version changes.

## Configuration migration

`config.v3.toml` is a tested example, not yet the production default. The
migration sequence is intentionally additive:

1. land and test the v3 static schema and policy module;
2. introduce pool leases without changing v2 single-local behavior;
3. add generic HTTP provider transports;
4. add the Codex subscription adapter;
5. add the v3 route executor and response headers;
6. teach the binary to load config version 2 or 3;
7. run v3 in shadow/explicit-model mode against representative OpenCode work;
8. make v3 the generated template only after parity tests are green.

V2 must continue compiling and passing its existing tests throughout the
branch. A version-3 document must never be partially interpreted as v2.

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
- tested three-worker and disabled-Ultra example.

### Phase 2: local pool leases

Refactor the existing single-upstream admission path into:

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

`GatewayTransport::local` should dispatch using the URL and credential carried
by the lease rather than a single URL captured at construction. The permit must
remain held until the streamed body is dropped, matching v2.

### Phase 3: HTTP provider registry

Replace the OpenRouter-only transport fields with a registry keyed by provider
name. Each provider owns:

- isolated credentials;
- protocol adapter;
- concurrency semaphore;
- health/readiness state;
- timeout policy;
- request profile;
- bounded identity for headers and metrics.

OpenRouter-specific Auto Router mutation remains an explicit profile rather
than contaminating generic providers.

### Phase 4: subscription command providers

Build a provider trait that can prepare a response before commitment. Add a
Codex CLI implementation using the Drep pattern. Command providers must be
single-purpose adapters, never arbitrary shell strings. Configuration may name
an argv credential command for HTTP keys, but execution backends themselves
must be compiled/known kinds.

### Phase 5: route executor

Execute validated route steps in order, mapping closed failure classes to each
route's `fallback_on` set. Add headers such as:

- `X-Octoroute-Route`;
- `X-Octoroute-Tier: worker|supervisor|cloud`;
- `X-Octoroute-Target`;
- `X-Octoroute-Provider`;
- `X-Octoroute-Model-Revision` for local targets;
- existing destination, reason, upstream, and request IDs.

Metrics must keep bounded labels. Physical member names are bounded config
values; prompts, session IDs, raw model output, credentials, and arbitrary
provider error strings are never labels.

### Phase 6: OpenCode integration

Expose one OpenAI-compatible base URL and these model IDs. OpenCode subagents use
`worker`; the primary agent uses `supervisor`. OpenCode remains responsible for
worktree isolation and review.

Validate with a representative suite:

- three simultaneous independent worker requests occupy three GPUs;
- a fourth worker request returns/queues according to configured policy without
  taking cloud fallback on the `worker` local-only route;
- 128K exact-context admission rejects oversized input before dispatch;
- Medium and XHigh fields survive schema-preserving proxying;
- disabled Ultra is skipped without marking the whole route unhealthy;
- enabling Ultra makes it the first supervisor target;
- Kimi and z.ai provider quirks are applied only to their requests;
- local-only never launches Codex or contacts cloud;
- pre-commit failures may continue; post-commit failures may not switch target;
- provider rate limits can continue only when the route explicitly allows it;
- OpenCode can run three worktree-isolated subagents and integrate their results.

## Non-goals

V3 does not:

- create or manage git worktrees;
- decompose tasks;
- merge agent commits;
- use a semantic classifier to override an explicit OpenCode tier choice;
- tensor-parallelize one model across the three 3080s;
- hide provider contract failures behind unlimited retries;
- promise that every subscription CLI can be losslessly represented as OpenAI
  chat completions.

## Completion criteria

The v3 branch is ready to merge when:

- all v2 tests remain green;
- config versions 2 and 3 fail closed into the correct parser;
- three local members are independently admitted and selected under load;
- every provider has protocol, credential, concurrency, and compatibility tests;
- local-only invariants are proven across every route and failure class;
- response streaming holds leases and forbids post-commit switching;
- OpenCode can use `worker`, `supervisor`, `local`, and `cloud-sota` through one
  endpoint;
- a migration document and production deployment profile are complete;
- shadow traffic confirms the intended predominantly-local routing mix before
  any automatic quality classifier is enabled.
