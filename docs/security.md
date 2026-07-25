# Security

Octoroute is a private API gateway that can spend cloud credits and transmit
prompt data. Authentication and network boundaries are required even for a
personal deployment.

## Authentication

All inference, model metadata, and metrics requests require exactly one:

```http
Authorization: Bearer <OCTOROUTE_API_KEY>
```

The configured credential is hashed to a fixed length before constant-time
comparison. Missing, repeated, malformed, or incorrect headers return 401.
Credentials that cannot be represented safely as HTTP bearer values fail
startup.

Octoroute has one configured inbound credential in v2. Rotate it by replacing
`OCTOROUTE_API_KEY` and restarting the service. Existing requests finish; new
requests must use the new value.

## Secret handling

- Secrets live in process environment variables or the ignored `.env`.
- TOML contains environment variable names, never secret values.
- Process variables override `.env`.
- Configuration and dotenv errors omit input values.
- Secret-bearing types redact `Debug` output.
- Inbound Authorization is never forwarded to either upstream.
- Local and OpenRouter clients use separate credentials.
- Other provider keys in `.env` are ignored unless configuration names them.

Restrict `.env` to the service account. Rotate the OpenRouter key through the
OpenRouter account and restart Octoroute. Revoke it immediately after any
suspected exposure.

## Request controls

- Header bytes are bounded before authentication.
- Authentication happens before request-body consumption.
- Request bodies are read with a hard byte limit.
- JSON is minimally validated before routing.
- Authenticated inference requests have a fixed-window rate limit.
- Inbound, local, and cloud concurrency have independent ceilings.
- Permits remain held through response completion or cancellation.
- Prometheus labels are enum-derived and bounded.

For Internet-facing deployments, add a reverse proxy or firewall with a
coarse per-IP rate limit. The application limit protects the configured
credential; it does not replace network-level denial-of-service controls.

## Privacy boundary

`model: local`, the configured local alias, and
`X-Octoroute-Privacy: local-only` prohibit cloud fallback. Unsupported,
unhealthy, busy, or over-context local-only requests fail instead of reaching
OpenRouter.

`model: auto` may use OpenRouter. Treat automatic prompts as cloud-eligible
unless the local-only header is present.

## Browser posture

Octoroute does not use cookies, so CSRF protections do not apply. It emits no
CORS allow-origin header; cross-origin browser JavaScript is denied by
default. Browser applications should use a same-origin backend or an
explicitly configured trusted reverse proxy rather than exposing the bearer
credential to arbitrary page JavaScript.

Every response includes:

```text
X-Content-Type-Options: nosniff
X-Frame-Options: DENY
Referrer-Policy: no-referrer
Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=()
Content-Security-Policy: default-src 'none'; frame-ancestors 'none'
```

Octoroute does not emit HSTS because the default deployment is plain HTTP on
a trusted LAN. Terminate TLS at a reverse proxy and configure HSTS there only
after HTTPS is verified.

## Upstream transport

- OpenRouter configuration requires HTTPS.
- Local HTTP is permitted for loopback or a trusted LAN.
- Base URLs reject embedded credentials, queries, and fragments.
- Response headers use a strict allowlist; cookies and hop-by-hop headers are
  removed.
- Streaming bodies are forwarded with backpressure and bounded request
  memory.
- Upstream switching is forbidden after the first response body byte.

Place llama.cpp on loopback after all clients move to Octoroute. Otherwise a
direct caller can bypass Octoroute admission and race its single-slot check.

## Supply chain

- `Cargo.lock` is tracked for reproducible application builds.
- CI tests stable and Rust 1.90.0 with `--locked`.
- CI runs Clippy, rustfmt, rustdoc warnings, and RustSec.
- Release builds use the lockfile, a pinned `cross`, supported GitHub runner
  labels, and a supported release action runtime.
- Weekly security maintenance refreshes RustSec and compatible dependencies.

Run before deployment:

```bash
cargo audit
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

## Incident response

If a secret may have leaked:

1. revoke the OpenRouter key;
2. replace the Octoroute inbound key;
3. rotate an optional llama.cpp key;
4. restart the service;
5. inspect bounded route/failure metrics and safe logs;
6. verify no unknown client still reaches llama.cpp directly.

Never paste credentials or personal prompt bodies into an issue, test
fixture, benchmark, or support log.
