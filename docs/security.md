# Security

Octoroute is an authenticated proxy across local and external trust boundaries.
Treat its configuration, service account, `.env`, and listening interface as
security-sensitive.

## Inbound authentication

Every chat, model-list, and metrics request requires the bearer secret named by
`server.api_key_env`. Comparison is constant-time. Missing and invalid values
produce bounded errors without echoing credentials.

Use a long random value, restrict `.env` to the service account, and bind to
loopback unless remote clients require access. Put TLS and network access
control in front of non-loopback deployments.

## Credential isolation

Configuration contains credential names, never raw values. Process environment
values override the optional `.env` beside the selected config file.

The inbound secret and enabled local-member secrets resolve during service
construction. Provider credentials remain lazy: building the registry, listing
models, and executing a local-only request do not read them. An explicit
readiness request resolves a provider credential only when that provider's
bounded probe cache expires.

An HTTP provider accepts exactly one of:

- `api_key_env`; or
- `api_key_command`, a literal argv without shell parsing.

Credential commands run with cleared environment plus `PATH`, null stdin and
stderr, piped stdout, process-kill-on-drop, a five-second timeout, and a 4 KiB
output limit. Output must be non-empty visible ASCII without whitespace.

Do not point credential commands at general-purpose shells or scripts whose
output includes logs.

## Local-only disclosure boundary

Routes declared `local_only` cannot reference providers. The request header
`X-Octoroute-Privacy: local-only` narrows any other route to local pools before
admission.

Local-only requests do not:

- resolve provider environment variables;
- run provider credential commands;
- contact provider endpoints;
- invoke HTTP or Codex provider adapters;
- fall back to cloud after local failure.

A cloud-only route combined with the header is rejected directly.

## Provider transport

External HTTP providers require absolute HTTPS endpoints without URL
credentials, queries, or fragments. OpenAI-compatible providers use bearer
authentication; Anthropic-compatible providers use `x-api-key` plus a pinned
`anthropic-version`. Credentials are applied only to a selected request or a
body-free expired readiness probe.

Each provider has a separate concurrency semaphore, request timeout, probe
timeout, and cached readiness state. Unsupported request features fail as
`incompatible` before credential resolution or external prompt disclosure.

The OpenRouter profile removes client-controlled `allowed_models` from the
Octoroute-owned Auto Router plugin and writes the configured policy value.

## Request and response safety

Inbound headers, bodies, rate, and concurrency are bounded before routing.
Malformed content shapes are not treated as safe local capabilities.

Upstream responses are stopped before commitment until the first body chunk is
available. Fallback can occur only within the configured closed trigger set and
only before client-visible bytes. Safe response headers are allowlisted; hop-by-
hop, cookie, and arbitrary provider headers are not forwarded.

Errors and logs omit:

- request bodies and message content;
- bearer and provider credentials;
- credential-command output;
- arbitrary provider response bodies;
- unbounded client identifiers.

## Operational hardening

- Run as a dedicated unprivileged account.
- Restrict configuration and `.env` permissions.
- Allow outbound access only to configured endpoints.
- Keep local llama.cpp services on a private network and use member bearer keys
  when that boundary is shared.
- Rotate a compromised provider key at the provider, update the environment,
  and restart Octoroute.
- Monitor authentication, admission, and upstream failure logs without enabling
  body logging in surrounding proxies.

## Codex CLI isolation

The Codex adapter never reads or stores ChatGPT account tokens. It inherits only
an allowlist needed for CLI discovery, the account home, locale, certificates,
and proxies, then drops `OCTOROUTE_*` and names containing API-key, token,
secret, password, credential, or auth markers. `forced_login_method` is pinned to
`chatgpt`, and readiness validates that mode through `doctor --json`.

Each request uses a new temporary workspace and invokes the CLI non-interactively
with ephemeral state, user configuration and rules ignored, approval disabled,
and a read-only sandbox. Shell/exec tools, web search, apps, hooks, memories,
and subagents are disabled. Stdin, stdout, stderr, event-line size, lifecycle,
wall time, and the final response schema are bounded. Any violation fails before
client commitment and can fall forward only under `precommit_failure`.

`GET /health/ready` is intentionally unauthenticated for orchestrators but can
resolve provider credentials or run the Codex diagnostic after cache expiry.
Expose it only on trusted operator networks; it never sends chat content.
