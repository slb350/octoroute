# PR #11 Fix Verification: 174e4e5

Checked: fix commit `174e4e5` ("Resolve the PR #11 review and close its
mutation-gate gaps") against the findings in pr-11-review-2026-08-27.md.
Read-only verification; nothing modified.

## Gate status

- `cargo test --all-targets --all-features`: 270 passed, 0 failed (was 126).
- `cargo test --no-default-features`: all suites pass.
- `cargo clippy --all-targets --all-features -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.
- Largest file 564 LOC; all under the 600 soft limit.
- CHANGELOG now has `## [3.0.0] - 2026-08-27`; `config.laptop.toml` is pinned
  by a parsing test.

## Important findings (20): all fixed

All nine functional/docs findings verified in code: `n_predict` budgets with
llama.cpp's own precedence including the -1/0 semantics; token-count 4xx maps
to `RequestRejected` -> `Incompatible` with 401/403/429/408/404 classified
separately; local fall-forward arms record `pool_fallbacks` (429 included);
dispatch-time 401/403 falls forward on an opted-in `unauthenticated` trigger
and never surfaces as the client's own 401; Codex `NotChatGpt`/`Diagnostic`
map to `Unauthenticated`; duplicate-target validation parses before it
reports; the Anthropic adapter fail-closes at message, content-block, tool,
tool_choice, tool-call-function, and `reasoning` levels with
`enabled`/`max_tokens`/`exclude` mapped; both docs/configuration.md errors
corrected (seven-trigger set with the deliberate default exclusion, and the
actual permit-then-probe admission order).

All eleven test-gap findings are covered by new, discriminating tests:
redirect.rs (never-followed proven via empty received_requests),
commit_boundary.rs, credential_isolation.rs (exact credential-set equality),
preflight.rs (body-polled flag proves auth-before-body), readiness.rs
(snapshot coalescing, 401/403 -> Unauthenticated), credential.rs
(re-resolution after rejection), metadata authz integration tests, the full
structural-validator suite, and codex/process_tests.rs (deadline kill/reap,
capture bounds, exit kinds, doctor failures). The flaky metric test now
asserts the race-free invariant.

## Minor findings: 50 fixed, 3 partial, 1 not fixed, 1 open by design

Partial:

- No test pins the local pool *member* permit living through the response
  body (inbound and provider permits are pinned).
- The exact-fit `used_context == context_window` boundary and degraded-state
  precedence across members remain unpinned.
- Priority-as-tiebreaker is documented in a code comment but not in
  docs/configuration.md.
- Anthropic adapter still lacks acceptance tests for `n: 1` and
  `response_format: {"type":"text"}`, a `MAX_SSE_EVENT_BYTES` bound test, a
  `truncate_on_char_boundary` test, and a multi-`data:`-line join test.

Not fixed:

- `unhealthy_member_is_skipped_before_disclosing_to_next_local_member`
  (local_pool_selection_tests.rs:62-85) still mounts no `expect(0)` on the
  unhealthy member's `input_tokens`; a mutation probing token-count before
  health still passes.

Open by design: no clock injection for the 1s health / 5min credential TTLs.

## One Important finding remains, and it was my omission

The Anthropic review agent reported `input_json_delta` on an untracked block
index as fatal post-commit (an unknown `content_block_start` is skipped, but
its follow-on deltas then kill the stream with a truncated client response).
I failed to carry that finding into the synthesized review report, so the fix
commit never saw it. Verified still present at
src/gateway/fabric/anthropic/response.rs:373-378: `tool_indices.get(&index)`
returns `AnthropicAdapterError::Response` for any index not tracked, and the
skip path records nothing. Fix when convenient: record skipped block indices
and skip-and-count their deltas.

## CI mutation gate

The workflow is restructured as recommended: every job has
`timeout-minutes`; PRs run `--in-diff` against the merge base (45 min); the
full sweep runs only on push to main (330 min). For this PR the restructure
cannot produce a verdict: the PR diff is effectively the whole tree, so the
diff-scoped job cancelled at its 45-minute timeout (job 98786741068) with no
missed.txt. The mutation evidence for this branch has to come from a local
`just mutants` run (remote offload), which I did not run.
