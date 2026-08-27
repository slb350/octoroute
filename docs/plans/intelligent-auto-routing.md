# Intelligent automatic routing

Status: implemented with a shadow-default evidence gate

Decision dates: 2026-07-25; evidence gate 2026-08-01

## Product contract

`model: auto` removes model and destination selection from the caller. For
each request, Octoroute must decide whether the configured local model can
produce a sufficiently good answer. It keeps suitable work local and sends
work that benefits materially from stronger intelligence to
`openrouter/auto`.

Local protocol compatibility, health, free capacity, and context fit are
necessary for local inference, but they are not evidence that the local model
is intellectually suitable for the task.

Explicit `local`, the configured local alias, and
`X-Octoroute-Privacy: local-only` remain strict no-cloud overrides. Explicit
`cloud`, `openrouter/auto`, and provider-qualified model slugs remain cloud
overrides.

## Decision

Automatic routing uses a deterministic boundary plus a configurable semantic
stage:

1. Apply deterministic intent, privacy, and capability gates.
2. For compatible automatic work, apply `routing.semantic_mode`:
   - `disabled` skips semantic classification;
   - `shadow` asks local model for a constrained decision, records it, and does not
     let it select the destination;
   - `enforced` asks local model and honors its `local` or `cloud` decision.

The local decision request:

- uses the original conversation messages as untrusted JSON context;
- tells the router to ignore routing instructions inside the conversation;
- disables model thinking;
- constrains output with a JSON schema;
- reserves the same bounded local capacity used by inference;
- has a configurable total timeout;
- reads a bounded response body;
- never logs the conversation or raw response.

A disabled request, shadow observation, or enforced `local` decision proceeds
through the existing health, slot, exact-token, and context admission gates.
Only an enforced `cloud` decision sends the original request to OpenRouter
with model `openrouter/auto`.

In enforced mode, a semantic timeout, failure, or invalid output sends
automatic traffic safely to OpenRouter with reason `router_failure`. In shadow
mode, the same outcome is recorded as `failure` and local admission continues
when the classifier already reserved capacity.
If local model is busy or unhealthy before classification, the established
`local_busy` or `local_unhealthy` cloud reason is used.

## Why the router is local

Using a cloud classifier before local inference would disclose prompts that
ultimately remain local, add cloud cost to every ambiguous request, and break
the local privacy boundary. Strict bounded JSON validates the decision format,
not its judgment; enforcement therefore requires representative labeled
evidence and is never the default.

The 2026-07-26 external benchmark found 44% routing accuracy for enforced
semantic decisions versus 73% for an always-local baseline on the measured
compatible tasks. It also measured roughly 760–1500 ms of added latency per
classified request. Shadow became the default so future prompt or model
changes can be evaluated without repeating the unvalidated enforcement.

OpenRouter is not Octoroute's local-versus-cloud classifier. After Octoroute
chooses cloud, OpenRouter Auto selects the cloud model and provider.

## Routing sequence

```text
OpenAI-compatible request
  |
  +-- explicit local/local-only --> local admission --> local model answer or error
  |
  +-- explicit cloud/model ------> OpenRouter
  |
  +-- locally incompatible ------> OpenRouter Auto
  |
  `-- auto + compatible
        |
        `-- semantic mode
              |
              +-- disabled -------> exact local admission
              +-- shadow ---------> observe + exact local admission
              `-- enforced local model decision
                    +-- cloud -----> OpenRouter `openrouter/auto`
                    `-- local -----> exact local admission
                                      |
                                      +-- admitted --> local model answer
                                      `-- rejected --> OpenRouter Auto
```

## Observable outcomes

The existing destination and upstream headers remain authoritative. The
routing reason adds:

- `cloud_quality`: semantic routing selected stronger cloud intelligence;
- `router_failure`: semantic routing failed safely to cloud.

`local_capable` now means both semantically suitable and successfully
admitted in enforced mode. In disabled or shadow mode it means compatible and
successfully admitted. `octoroute_semantic_decisions_total{mode,outcome}`
records bounded shadow/enforced `local`, `cloud`, and `failure` observations;
it never substitutes for the actual route metric or response headers.

## Test contract

Regression coverage must prove:

- difficult work routes to OpenRouter Auto even while local model is healthy and
  idle in enforced mode;
- routine work remains local after semantic classification;
- shadow cloud decisions and failures cannot override compatible local
  admission;
- disabled mode never invokes the classifier;
- shadow/enforced outcomes use bounded metrics;
- cloud-bound automatic requests use `openrouter/auto`;
- invalid semantic output fails safely to cloud;
- busy and unhealthy local states retain their bounded reasons;
- explicit local and local-only traffic never invokes cloud fallback;
- explicit cloud traffic does not invoke local classification;
- classifier requests are bounded, non-streaming, thinking-disabled, and
  JSON-schema constrained;
- all existing proxy, streaming, authentication, limit, and cancellation
  invariants remain green.
