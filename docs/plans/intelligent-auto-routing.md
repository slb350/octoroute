# Intelligent automatic routing

Status: implemented

Decision date: 2026-07-25

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

Automatic routing uses a two-stage boundary:

1. Apply deterministic intent, privacy, and capability gates.
2. For compatible automatic work, ask Strix for a constrained semantic
   `local` or `cloud` decision.

The local decision request:

- uses the original conversation messages as untrusted JSON context;
- tells the router to ignore routing instructions inside the conversation;
- disables model thinking;
- constrains output with a JSON schema;
- reserves the same bounded local capacity used by inference;
- has a configurable total timeout;
- reads a bounded response body;
- never logs the conversation or raw response.

A `local` decision proceeds through the existing health, slot, exact-token,
and context admission gates. A `cloud` decision sends the original request to
OpenRouter with model `openrouter/auto`.

If the semantic decision times out, fails, or returns invalid output,
automatic traffic fails safely to OpenRouter with reason `router_failure`.
If Strix is busy or unhealthy before classification, the established
`local_busy` or `local_unhealthy` cloud reason is used.

## Why the router is local

Using a cloud classifier before local inference would disclose prompts that
ultimately remain local, add cloud cost to every ambiguous request, and break
the local privacy boundary. The configured Strix model is capable enough to
make the binary task-suitability decision, and the decision is validated as
strict bounded JSON.

OpenRouter is not Octoroute's local-versus-cloud classifier. After Octoroute
chooses cloud, OpenRouter Auto selects the cloud model and provider.

## Routing sequence

```text
OpenAI-compatible request
  |
  +-- explicit local/local-only --> local admission --> Strix answer or error
  |
  +-- explicit cloud/model ------> OpenRouter
  |
  +-- locally incompatible ------> OpenRouter Auto
  |
  `-- auto + compatible
        |
        `-- local semantic decision on Strix
              |
              +-- cloud ----------> OpenRouter `openrouter/auto`
              |
              `-- local ----------> exact local admission
                                      |
                                      +-- admitted --> Strix answer
                                      `-- rejected --> OpenRouter Auto
```

## Observable outcomes

The existing destination and upstream headers remain authoritative. The
routing reason adds:

- `cloud_quality`: semantic routing selected stronger cloud intelligence;
- `router_failure`: semantic routing failed safely to cloud.

`local_capable` now means both semantically suitable and successfully
admitted, rather than merely protocol-compatible and idle.

## Test contract

Regression coverage must prove:

- difficult work routes to OpenRouter Auto even while Strix is healthy and
  idle;
- routine work remains local after semantic classification;
- cloud-bound automatic requests use `openrouter/auto`;
- invalid semantic output fails safely to cloud;
- busy and unhealthy local states retain their bounded reasons;
- explicit local and local-only traffic never invokes cloud fallback;
- explicit cloud traffic does not invoke local classification;
- classifier requests are bounded, non-streaming, thinking-disabled, and
  JSON-schema constrained;
- all existing proxy, streaming, authentication, limit, and cancellation
  invariants remain green.
