# Octoroute - Tiered Local/Cloud Inference Fabric

This AGENTS.md is the tracked, authoritative instruction file for agents working in this repository. A local `CLAUDE.md` stays gitignored; it imports this file with `@AGENTS.md` and may add private notes.

## Project overview

Octoroute v3 is a Rust/Axum OpenAI-compatible gateway that routes each request
along a configured chain of local llama.cpp pools and cloud or
subscription-backed providers.

```text
Client -> Octoroute -> route steps in order
                    -> local pool member (health, slot, token, context checks)
                    -> HTTP provider (OpenAI or Anthropic protocol)
                    -> Codex CLI provider (ChatGPT subscription)
```

Routing is configuration. Octoroute never classifies a prompt to choose a
destination; the client names a route and the route names its steps in
preference order.

## Runtime contract

- Rust edition 2024; MSRV 1.90.0. Development is pinned to 1.97.1 by
  `rust-toolchain.toml`; CI is authoritative for MSRV.
- `Cargo.lock` is tracked because Octoroute ships a deployable binary.
- `POST /v1/chat/completions` is the only inference endpoint.
- `GET /v1/models` advertises `auto` plus every configured route.
- `GET /health/live` is process liveness.
- `GET /health/ready` and `/health` return aggregate readiness. The per-pool and
  per-provider breakdown is returned only to an authenticated caller, and the
  snapshot is cached because a readiness pass spawns `codex doctor` and sends
  credentialed `/models` probes.
- `GET /metrics` exposes the v3 Prometheus registry and requires the bearer.
- Inference and metadata endpoints require the configured bearer credential.
- Health endpoints are intentionally unauthenticated.

## Model intent

A client `model` is a **route name**, never a provider slug. Identifiers are
1-128 characters of ASCII letters, digits, dots, underscores, and hyphens, so a
v2-style `provider/model` passthrough returns 400. The route resolves the
provider.

| Client model | Destination |
| --- | --- |
| `auto` | Alias for `routing.default_model` |
| `auto-route` | Local pools first, then providers |
| `worker` | Local worker pool only |
| `supervisor` | Optional local supervisor, then providers |
| `local` | Local pools only |
| `cloud-sota` | Provider-only escalation |

`X-Octoroute-Privacy: local-only` narrows a route to its local steps before
admission. A route declaring `privacy = "local_only"` is narrowed the same way
whether or not the header is present, so the guarantee does not depend on
configuration validation having refused provider steps. Combining the header
with a `cloud_only` route is an error.

## Routing policy

For each step in the plan, in order:

1. authenticate before reading the request body;
2. validate bounded JSON and explicit route/privacy intent;
3. for a local pool: reject a request whose output reservation plus safety
   tokens already exceed the context window before probing anything, then
   acquire a member permit, check cached health and a free `/slots` entry,
   obtain exact input tokens from `/v1/chat/completions/input_tokens`, and
   verify input + output + safety reserve fits the context window;
4. for a provider: acquire a permit, resolve the cached credential, and build
   the protocol-specific request;
5. on rejection, fall forward only when the route's `fallback_on` set contains
   the resulting trigger.

The output reservation reads `n_predict` first, then `max_completion_tokens`,
then `max_tokens`. llama.cpp's `oaicompat_chat_params_parse` overwrites the
`max_tokens`-derived value with an explicit `n_predict`, so any other precedence
budgets one number while the member attempts another.

The permit is acquired before the probes so two of Octoroute's own requests
cannot both claim the same free `/slots` entry. The cost is a false `busy` for a
second request arriving while the first holds the permit through its probes.

A 4xx from the token-count endpoint is a **request** rejection, not member
incapability: the endpoint applies the chat template, so the failure is
deterministic across members and must not be retried against them. Only 404 and
501, an unparseable body, and statuses describing the member rather than the
request report `TokenCountUnavailable`.

Fallback triggers are a closed set: `busy`, `unhealthy`, `context_overflow`,
`incompatible`, `rate_limited`, `precommit_failure`, `unauthenticated`.
`unauthenticated` is **outside** the default set: an expired or missing
credential must surface rather than silently reroute traffic and spend. It is
honoured symmetrically at credential resolution and at a dispatch-time 401,
403, or 407;
an upstream 401 is never returned in a form a client reads as its own
credential failing.

A terminal route error reports the rejection that governed the route, not the
state of the last step tried.

## Proxy invariants

- Preserve the complete bounded JSON object and unknown request fields.
- Mutate only the model and server-owned OpenRouter Auto policy. Inject
  `reasoning_effort` only into providers explicitly configured for it.
- Serialize an admitted local request once and reuse the same bytes for
  llama.cpp token counting and inference.
- Forward response status, body bytes, SSE comments/data, `[DONE]`, usage,
  errors, and the actual selected model without reconstruction.
- Buffer the first upstream body chunk as the response commit boundary.
- Never switch target after the first client-visible body byte.
- Hold local, provider, and inbound permits through the response body lifetime.
- Never forward inbound Authorization to an upstream.
- Never return upstream credentials or unsafe response headers to a client.
  Response headers are rebuilt from a fixed allowlist, so a new upstream header
  fails closed.
- Refuse upstream redirects. The Anthropic protocol sends the credential in a
  custom `x-api-key` header, which reqwest does not strip across origins; a 3xx
  is a pre-commit failure.
- One pooled rustls client is shared by local probes, local inference, and every
  provider. Credentials are applied per request.

## Anthropic adapter contract

- Thinking is opt-in: the caller's reasoning control or the provider's
  `reasoning_effort`. A route default must never enable it.
- `max_tokens` is the total for thinking plus the answer, so the budget claims
  at most half of it and is omitted below Anthropic's 1024-token minimum.
- `temperature`, `top_p`, and `top_k` are dropped when thinking is enabled; the
  API rejects that combination.
- A `system` or `developer` turn after conversation content fails closed. It
  cannot be represented in place, and hoisting it would silently promote a
  mid-conversation instruction to a global one.
- `tool_choice: "none"` emits `{"type": "none"}` and keeps the tools array, so
  prior `tool_use`/`tool_result` history stays valid.
- Unknown content blocks, SSE events, and deltas are skipped behind a counter.
  `redacted_thinking` is the concrete case. `error` events stay fatal.
- Unrecognized `stop_reason` values pass through; `refusal` and `pause_turn` are
  not stops.
- Absent upstream usage omits the key rather than reporting zeros.
- Upstream error bodies preserve `error.message` and `error.type`.
- OpenAI fields with no verified mapping fail closed as `incompatible`, at
  every nesting level. The allowlist applies to message objects, content
  blocks, tool and function objects, and the `reasoning` object, not only to
  the top level: a dropped nested key hands the caller a plausible answer to
  a request it did not make. `reasoning.enabled` and `reasoning.max_tokens`
  have verified mappings onto the thinking budget; the rest fail closed.

## Codex CLI contract

- `env_clear()` plus an allowlist, argv vector with no shell,
  `--sandbox read-only`, `--ephemeral`, `--ignore-user-config`, `-a never`, six
  `--disable` flags, `project_doc_max_bytes=0`, `kill_on_drop`, bounded
  file-backed capture, and a timeout that kills the child.
- Unknown event and item types are skipped behind a counter, including events
  trailing `turn.completed`, so a future CLI adding one does not fail the
  run; `error` and `turn.failed` stay fatal and `turn.completed` is still
  required, so a truncated run is rejected.
- A CLI authenticated with an API key instead of the ChatGPT subscription is
  `unauthenticated`, never `unhealthy`. It is an operator error, and
  `unhealthy` is in the default fallback set, so the wrong mapping spills
  every request and its spend to the next step in silence.
- `turn.completed` usage becomes an OpenAI `usage` object.
- Responses are complete, not incremental: the CLI returns its answer as one
  final JSON agent message, so a streaming request receives one SSE chunk plus
  `[DONE]`. Octoroute does not claim token-by-token streaming.
- Cleanup of a process group that is already gone is not a failure. `ESRCH`
  says so on its own; `EPERM` does so only once our own leader has exited,
  because a leader we still own would have accepted the signal. Darwin reports
  a recycled pgid as `EPERM`. A leader still running that we may no longer
  signal is a real failure and must surface: `api_key_command` runs an
  operator's executable, and one that changes credentials leaves a process this
  guard cannot reap. Only `ProcessGroup::terminate` can settle `EPERM`; `Drop`
  has no child to ask.
- A failure inside cleanup never replaces the error that caused cleanup to run.
  `OutputTooLarge` and `Timeout` are what the route's fallback policy reads;
  reporting `Process` in their place changes the routing decision.
- `deploy/octoroute.service` deliberately omits `MemoryDenyWriteExecute` and
  `RestrictNamespaces`: both propagate to the Codex child, faulting any JIT and
  potentially blocking Codex's own read-only sandbox.

## Configuration and secrets

Configuration requires `config_version = 3`. Secret-bearing TOML fields contain
environment variable names, never values:

```dotenv
OCTOROUTE_API_KEY=<long random client credential>
OPENROUTER_API_KEY=<OpenRouter credential>
```

An optional ignored `.env` beside `config.toml` is read without mutating the
process environment. Process variables take precedence, except that an
exported-but-empty variable does not shadow a `.env` value.

- Provider endpoints must use HTTPS. URLs cannot contain embedded credentials,
  queries, or fragments.
- Local pool members must be explicit loopback, private-range, or link-local
  IP literals, so a public member cannot satisfy `local-only`. Hostnames,
  `.local` and `localhost` included, are refused: DNS can resolve a
  private-looking name to a public address after validation.
- HTTP providers require exactly one of `api_key_env` or `api_key_command`.
  Resolved credentials are cached for five minutes and discarded on 401, 403,
  or 407.
  Commands run under an allowlisted environment including `HOME` and `TMPDIR`.
- Optional `first_byte_timeout_ms` (pools and providers) bounds how long a hung
  upstream holds permits before the route falls forward. Set it only from
  measured behavior; unset, Octoroute invents no deadline.
- `token_count_timeout_ms` (pools) is the token-count deadline, separate from
  the two-second health/slot probe timeout.
- Configuration errors never interpolate raw configuration values.

`ProviderConfig::runtime` is a sum type (`Http { endpoint, protocol,
credential }` or `CodexCli { executable }`). The validator has always
enforced that shape;
encoding it means no runtime code recovers a discriminant with `expect`.

## Source layout

```text
src/
  main.rs                     # startup, telemetry, bounded graceful shutdown
  cli.rs                      # CLI and v3 config template
  telemetry.rs
  gateway/
    auth.rs                   # bearer validation
    env.rs                    # process + optional dotenv, SecretString
    http_client.rs            # shared pooled rustls client, no redirects
    request.rs                # schema-preserving request facts
    fabric/
      anthropic/              # response, shared error, and:
        request.rs + request/ # fields, messages, tools, params
        response.rs + response/ # translation facade + usage accounting
        tests.rs + tests/     # one module per translation concern
      codex/                  # adapter, process.rs child execution, events, tests
      config/                 # public types + validation/{targets,fields}
      local_pool/             # pool admission + member probes
      provider/               # registry + credential + body + readiness
      service/                # service surface + routing/rejection policy + responses
      bounded_response.rs     # shared bounded upstream response reader
      http.rs                 # Axum routes
      http_support.rs         # limits, rate limiting, response guards
      metrics.rs              # bounded Prometheus registry
      policy.rs               # privacy directive and route planning
      test_support.rs         # Unix executable fixtures shared by tests
      transport.rs            # credential-isolated HTTP/SSE proxy
tests/
  gateway_v3.rs
  main_process.rs             # executable exit, startup, and SIGTERM contracts
```

Keep every Rust source and test file below 600 lines and never above 800. Split
by responsibility before adding a second concern.

## Development workflow

1. write a failing test;
2. run it and verify RED;
3. implement the complete typed behavior;
4. rerun focused tests;
5. format and refactor while green;
6. run the full repository quality gate;
7. run `/simplify` before any code commit.

Required branch gate:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --doc --all-features
cargo doc --all-features --no-deps
cargo audit
just mutants
```

### Cleanup decisions (2026-09-04)

- Consolidate overlapping tests by retaining their unique assertions in the
  strongest contract test. Keep process cleanup, credential isolation, privacy,
  and response-commit boundaries explicit.
  Retain direct-service assertions when HTTP middleware can mask a service
  fault, such as supplying a missing request ID on its behalf.
- Use isolated instances of the production unknown-type counter implementation
  in tests. Production remains process-global; no retry loops or global resets.
- Preserve the body reader's zero-copy single-chunk path and bounded coalescing.
  Benchmarks confirmed its allocation benefit; consolidate boundary tests instead
  of replacing it with an always-copying or per-frame-retaining collector.
- Member ordering snapshots load once, then sorts borrowed members by load,
  priority, and unique rotation. Filter saturated members eagerly after sorting;
  changing that timing can change a mixed pool's governing rejection.
- Provider runtimes have one registry owner; use `Box`, retaining `Arc` only for
  resources actually shared across requests.
- The crate defines no feature variants. Run all targets and explicit doctests
  once per compiler, and avoid duplicate branch-push and pull-request CI runs.
- Mutation validation requires the working tree to match the index. Hooks refuse
  to rewrite or stage files. Remote upload, sweep, and artifact copying are
  serialized per checkout. Transfers retain a lock until they stop, including
  after loss of the controlling SSH session. Cleanup watchers retain the active
  lock through process-group cleanup; their timer children close both lease
  descriptors so an interrupted watcher cannot leave a timer holding the lock.

### Mutation testing

`cargo-mutants` is wired into `.githooks/pre-commit` (diff-scoped), the justfile,
and CI. Enable the hook with `git config core.hooksPath .githooks`.

Test-only helper functions in test code belong inside a module gated with the
exact attribute `#[cfg(test)]`. cargo-mutants recognizes only that literal gate
as scaffolding: `#[cfg(all(test, unix))]` is not recognized, so helpers under
it are mutated and reported as surviving production mutants. Put `#[cfg(unix)]`
on the individual items instead. Never use a mutants-skip attribute on
production code to hide a survivor.

Platform-specific production code has the mirror-image problem: a
`#[cfg(not(unix))]` module is never compiled on the Unix hosts that run the
sweep, so every mutant inside it reports missed without any test being at
fault. Express platform differences as cfg'd blocks inside shared,
always-compiled functions, as `process_group.rs` does.

Ordinary CI inspects the complete pushed or pull-request diff before allocating
the mutation runner. Added, changed, deleted, or renamed tests qualify: inline test
changes select their owning production source files; shared test modules,
fixtures, integration tests, and doctests require the full tree. Production-only
changes skip mutation. One Python classifier owns this policy and publishes the
runner arguments once; Git fixture checks run with workflow tests rather than in
every Cargo mutation test invocation. Manual dispatch and the monthly schedule
on the fifth at 09:17 UTC always run the full sweep. Scheduled and
manual runs omit ordinary CI jobs. Failures retain only the three bounded repair
reports for Jobsy's `octoroute-mutation-repair` job, which repairs the survivors through a draft pull request on the sixth.

- The verdict is `missed.txt`, not the exit code: cargo-mutants returns exit 3
  (Timeout) in preference to exit 2 (FoundProblems). `scripts/mutants-run.sh`
  owns that decision.
- Scratch copies go beside the checkout via `TMPDIR`, never in the system temp
  dir, because a killed run strands multi-GB tree copies in a tmpfs.
- Directories are not the whole of cleanup. `mutants-run.sh` also reaps
  processes matching the scratch root, excluding its own process tree: the
  mutants that disable the process-group kill path are exactly the ones that
  time out, so the fixture they spawned outlives the run and spins on a core.
- Fix a survivor by making a test discriminate, never by excluding the mutant.
  A mutation that genuinely cannot be observed marks dead code to delete.
- Every mutation workload runs on homelab-ai-1 as the `octoroute-mutants` role. CI sweeps run on that role's runner (`runs-on: [self-hosted, linux, x64, homelab-ai-1, octoroute-mutants]`) and never for pull requests from forks. `scripts/mutants-remote.sh` offloads hook and `just mutants` runs there through `scripts/mutants-ai1-transport.sh`, which sends every command through ai-1's sandboxed `offload.py`, and falls back to a local run with a warning when ai-1 is unreachable. Hosted and laptop runs serialize on `/srv/ci/fleet/octoroute-mutants/home/host.lock`, and ai-1's CI slice bounds their CPU and memory. The five scripts are drep's (`~/dev/drep/scripts/`): keep them in step with it, changing only the role, lock and workspace defaults plus Octoroute's three additions, which are orphan reaping, closing the lock descriptor for cargo, and the index gate.
- Entrypoint mutation qualification, 2026-08-28: unit tests in `src/main.rs`
  pin exact artifact writes, the `AlreadyExists` remap and non-remapped I/O
  errors, and parseable config generation without overwrite. Process tests in
  `tests/main_process.rs` pin non-zero prefixed startup errors and prove the
  server remains live until SIGTERM, then exits cleanly. The process fixture
  polls separate startup, liveness, and shutdown deadlines, uses an
  OS-assigned listen port, makes every network upstream unreachable on
  loopback port zero, and points Codex at a nonexistent executable.
  Hand-applying each of the six `src/main.rs` mutations makes its named
  discriminator fail; the two process tests also pass 20 consecutive runs
  without synthetic load.
- Final full-tree survivor qualification, 2026-08-28: the local-pool mutation
  test drives four admissions through three members, pins the `0, 1, 2, 0`
  rotation, and asserts that the stored cursor wraps instead of growing
  unbounded. The Anthropic UTF-8 truncation test covers both an exact emoji
  boundary and a limit inside that emoji; the redundant `end > 0` guard was
  removed because byte index zero is always a character boundary and its
  `>=` mutant was equivalent. Member and provider concurrency defaults are
  both one, so their literal-one return mutants were equivalent too; raw
  values now remain optional until validation applies the documented
  constants, and one omitted-field table pins both concurrency defaults plus
  the 30-second provider readiness timeout. The OpenAI provider-body test pins
  both sides of reasoning-default injection: it applies only when neither
  caller reasoning control is present. These restructures preserve runtime
  behavior and remove equivalent mutation sites rather than excluding them.

Wiremock tests bind loopback listeners. Config fixtures go through
`config_with`/`config_with_first`, which assert their anchor text is present: a
plain `str::replace` whose anchor has moved silently becomes a no-op and the
test then passes against an unmodified config.

Mutation-workflow test fixtures that capture a subprocess pipe close it after
stopping and reaping the child. Run the Python workflow suite with
`-W error::ResourceWarning` to catch leaked pipes.
The custom mutation-runner labels are listed in `.github/actionlint.yaml` so
local workflow lint checks the same runner names that GitHub uses.

## Observability and safety

Use only bounded enum-derived metric labels. Never use prompts, arbitrary client
models, credentials, or raw errors as labels.

Gateway response headers: `X-Octoroute-Destination`, `X-Octoroute-Reason`,
`X-Octoroute-Upstream`, `X-Octoroute-Request-Id`, `X-Request-Id`.

Metric families:

- `octoroute_fabric_pool_admissions_total{pool,state}`
- `octoroute_fabric_pool_fallbacks_total{pool,trigger}` - the signal that local
  capacity is spilling to cloud
- `octoroute_fabric_provider_{admissions,responses,fallbacks,probes}_total`
- `octoroute_fabric_routing_duration_seconds` - admission and probes for every
  step that attempts admission, rejected steps included, excluding the upstream
  call. Observing only admitted steps silences the histogram during an outage
- `octoroute_fabric_unknown_upstream_types_total{adapter}`

Safe logs may contain request IDs, bounded route reason/destination, status
class, and timing. They must never contain request bodies, Authorization
headers, keys, or raw invalid configuration lines. `GatewayRequest` and
`PoolLease` carry redacting `Debug` impls so a stray `?request` cannot leak a
prompt or a member credential.

## V3 architecture decision

- **Choice:** replace the v2 single-local/OpenRouter gateway with an ordered
  fabric of local pools and providers.
- **Routing:** configuration only. The v2 semantic classifier, calibration
  command, capability card, trajectory evidence, and session latch are removed.
- **Transport:** direct bounded JSON and opaque streaming HTTP proxying, with
  explicit translation for the Anthropic and Codex protocols.
- **Privacy:** local-only intent never reaches a provider, enforced by filtering
  the plan rather than by a runtime check the executor could forget.
- **Fallback:** work spills to the next step only before response commitment.
- **Release:** breaking `3.0.0` with `config_version = 3`; no v2 compatibility
  layer.
- **Decision date:** 2026-08-27.

The detailed design is `docs/plans/octoroute-v3-tiered-inference-fabric.md`.
The production hardening contract is `docs/security.md`.

## Laptop development profile

- `config.laptop.toml` binds Octoroute to `127.0.0.1:8081` and routes local work
  to `http://127.0.0.1:8080`, a llama.cpp server on the same workstation.
- The ignored `.env` beside the configuration holds `OCTOROUTE_API_KEY` and
  provider credentials; never print or commit them.
- Start with `cargo run --release -- --config config.laptop.toml`.

## Scheduled maintenance

Jobsy on homelab-ai-1 runs this repository's scheduled maintenance as a weekly dependency-security job and a weekly improvement job. Each works in a fresh checkout of main and opens a draft pull request from a `jobsy/` branch. Mutation testing runs in CI on the pull request, scoped by the mutation policy above.

A daily Jobsy review job reviews each open `jobsy/` pull request against this file, fixes it on its own branch when it is not solid, and merges it with a merge commit once every check on the reviewed head has passed; Jobsy refuses the merge otherwise. A weekly Jobsy release job decides under the release rules in this file whether the merged work warrants a release, and if so opens a `jobsy/release/` pull request that bumps the version and moves the changelog entries. When that pull request merges, Jobsy pushes the annotated tag `vX.Y.Z` on the merge commit, and `publish-crate.yml` publishes that commit to crates.io through trusted publishing; `release.yml` builds the GitHub release from the same tag. No Jobsy job pushes main directly or publishes from its own host.

Maintenance decisions that still hold:

- Dependabot checks Cargo and GitHub Actions for version updates weekly, Saturday at 9:00 PM America/Los_Angeles. Scheduled version updates exclude semver-major releases; this exclusion does not apply to Dependabot security updates.
- The 2026-09-19 maintenance release preserved PR #21's reviewed lockfile
  update and added a direct `rustls >=0.23.45` manifest floor. A lockfile-only
  security update is insufficient when published library consumers could
  otherwise resolve a vulnerable transitive version.
- Runtime security fixes warrant a patch release; docs/test/CI/dev-only changes
  do not.
- GitHub Actions use immutable commit pins with human-readable release comments,
  read-only default permissions, and pinned installer versions.

Use current primary documentation for Axum, reqwest, OpenRouter, Anthropic, and
llama.cpp contracts. Check an appropriate MCP first; if none is available, use
official upstream documentation.
