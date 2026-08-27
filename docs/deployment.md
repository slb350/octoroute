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
```

The process environment overrides `.env`. Provider variables can be omitted
until their route steps are used, but the inbound key and credentials for
enabled authenticated local members must exist at startup.

Restrict both files to the service account.

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

## Startup sequence

Startup performs static config validation, loads the optional `.env`, resolves
the inbound secret and enabled local-member secrets, constructs local pools,
and constructs provider adapters without reading provider credentials.

The listener binds only after those steps succeed. Unsupported Anthropic and
Codex adapters do not block startup; they report incompatible when selected or
in readiness.

## Verification

After startup:

1. call `/health/live` and verify `config_version: 3`;
2. call `/health/ready` and inspect every configured pool/provider;
3. authenticate to `/v1/models` and confirm the expected virtual routes;
4. send a non-streaming `worker` request;
5. send a streaming `worker` request and verify opaque SSE completion;
6. send `auto` with `X-Octoroute-Privacy: local-only` while local admission is
   unavailable and verify that no provider credential or endpoint is touched;
7. canary each enabled OpenAI-compatible provider route separately;
8. scrape authenticated `/metrics` and retain request IDs in proxy logs.

Readiness currently does not authenticate/probe providers, so a successful
provider canary is required before directing production traffic.

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
