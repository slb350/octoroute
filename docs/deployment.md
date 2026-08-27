# Deployment

Octoroute v3 is a single Rust service in front of local llama.cpp pools and
configured provider endpoints.

## Prepare configuration

Start from `config.toml` or `config.laptop.toml`. Set stable pool/member names,
immutable model revisions, context limits, provider chains, and route privacy.

Create a `.env` beside the selected config file:

```dotenv
OCTOROUTE_API_KEY=<long random inbound secret>
OPENROUTER_API_KEY=<provider credential when used>
ZAI_API_KEY=<provider credential when used>
KIMI_API_KEY=<Anthropic-compatible provider credential when used>
```

The process environment overrides `.env`. Provider variables can be omitted at
startup, but the provider will remain unavailable when selected or probed.
The inbound key and credentials for enabled authenticated local members must
exist at startup.

Restrict both files to the service account.

For an enabled `codex_cli` provider, install the official `codex` executable
for the service account and complete ChatGPT login under its `HOME` or
`CODEX_HOME`. Verify `codex doctor --json` as that account before startup.

## Build and run

```bash
cargo build --locked --release
target/release/octoroute --config /etc/octoroute/config.toml
```

The repository includes example systemd units in `deploy/`. Adjust user/group,
paths, network dependencies, and local-model service relationships for the
deployment.

## Network placement

- Bind `127.0.0.1` for a same-host client.
- For remote clients, bind a private address and place authenticated TLS in
  front of Octoroute.
- Keep llama.cpp members on trusted private networks.
- Restrict outbound traffic to configured HTTPS provider endpoints.
- Do not expose local health, slot, or token-count endpoints publicly.
- Restrict Octoroute's unauthenticated readiness endpoint to operator networks:
  a cache refresh resolves provider credentials and can execute the bounded
  Codex diagnostic, though it sends no prompt body.

## Startup sequence

Startup performs static config validation, loads the optional `.env`, resolves
the inbound secret and enabled local-member secrets, constructs local pools,
and constructs OpenAI, Anthropic, and Codex provider adapters without reading
provider credentials or launching the CLI.

The listener binds only after those steps succeed. Provider credentials and
Codex availability remain lazy until selection or an expired readiness probe.

## Verification

After startup:

1. call `/health/live` and verify `config_version: 3`;
2. call `/health/ready` and inspect every configured pool/provider;
3. authenticate to `/v1/models` and confirm the expected virtual routes;
4. send a non-streaming `worker` request;
5. send a streaming `worker` request and verify opaque SSE completion;
6. send `auto` with `X-Octoroute-Privacy: local-only` while local admission is
   unavailable and verify that no provider credential or endpoint is touched;
7. canary each enabled HTTP or Codex provider route separately;
8. scrape authenticated `/metrics` and retain request IDs in proxy logs.

Run the repository canary against the deployed listener:

```bash
OCTOROUTE_URL=https://octoroute.internal \
OCTOROUTE_API_KEY="$OCTOROUTE_API_KEY" \
OCTOROUTE_LOCAL_MODEL=worker \
OCTOROUTE_PROVIDER_MODEL=cloud-sota \
scripts/v3-canary.sh
```

Omit `OCTOROUTE_PROVIDER_MODEL` to test only the local-only boundary. The
script checks liveness, active readiness, model listing, local-only completion,
local-only SSE, and the optional explicit provider route without printing
response bodies or placing the bearer secret in curl's argument list.

Readiness verifies bounded authentication/reachability, but it is not a model
generation test. A successful explicit-route canary is still required before
directing production traffic.

## Rolling changes

Virtual model names are the client contract. Pool membership, model revisions,
provider priority, and route chains can change without reconfiguring clients.

For a model replacement:

1. deploy the new llama.cpp member under a new or updated immutable revision;
2. verify its health, slot, input-token, and chat endpoints;
3. add/enable it in a pool with bounded capacity;
4. canary through Octoroute;
5. remove the old member after in-flight streams finish.

For provider changes, add the credential out of band, canary an explicit route,
then adjust route order. Local targets must remain before providers.

## Rollback

Keep the previous v3 binary and configuration as a pair. To roll back, stop the
service, restore both artifacts, and restart. Avoid restoring a configuration
that names a model revision no longer deployed.

Credential revocation is independent of binary rollback. Revoke suspected
provider keys immediately, update the environment, and restart the service.
