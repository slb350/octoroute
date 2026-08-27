# Deployment

## Build

```bash
cargo build --release --locked
```

The binary is `target/release/octoroute`. Octoroute requires Rust 1.90 to
build; the deployed binary has no Rust runtime dependency.

## Files

Install:

```text
/opt/octoroute/bin/octoroute
/opt/octoroute/config.toml
/opt/octoroute/.env
```

`.env` must be readable only by the service account:

```dotenv
OCTOROUTE_API_KEY=<long random client credential>
OPENROUTER_API_KEY=<OpenRouter API credential>
```

Never include secrets in `config.toml`, command arguments, unit files, logs,
or source control.

When Octoroute runs beside the local model endpoint, a loopback base URL such
as `http://127.0.0.1:8080` minimizes exposure. When the gateway and model run on
separate hosts, use the model endpoint's trusted LAN or VPN address instead.
Keep both services behind firewall rules appropriate for the deployment.

The repository profile binds Octoroute to port 8081 as an example. Operators
may choose another non-conflicting port in `config.toml`.

## systemd gateway service

```ini
[Unit]
Description=Octoroute local-first LLM gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=octoroute
Group=octoroute
WorkingDirectory=/opt/octoroute
ExecStart=/opt/octoroute/bin/octoroute --config /opt/octoroute/config.toml
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
LockPersonality=true
MemoryDenyWriteExecute=true
UMask=0077

[Install]
WantedBy=multi-user.target
```

The tracked `deploy/octoroute.service` contains this unit. Create a locked
service account and install the files:

```bash
sudo useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin octoroute
sudo install -d -o root -g octoroute -m 0750 /opt/octoroute/bin
sudo install -o root -g root -m 0755 target/release/octoroute /opt/octoroute/bin/octoroute
sudo install -o root -g octoroute -m 0640 config.toml /opt/octoroute/config.toml
sudo install -o root -g octoroute -m 0640 .env /opt/octoroute/.env
sudo install -o root -g root -m 0644 deploy/octoroute.service /etc/systemd/system/octoroute.service
```

A production `.env` should contain only credentials referenced by the active
configuration. Do not print secret values while creating the deployment file.

Then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now octoroute.service
sudo systemctl status octoroute.service
```

## Optional managed llama.cpp service

The tracked `deploy/local-llama-server.service` is a generic template for a
local llama.cpp endpoint. Its defaults are intentionally placeholders. Override
model path, alias, context size, compute offload, thread count, batch size, and
parallelism in `/etc/octoroute/local-llm.env` for the target machine.

Example overrides:

```dotenv
LOCAL_MODEL_PATH=/var/lib/octoroute/models/local-model.gguf
LOCAL_MODEL_ALIAS=local-model
LOCAL_CONTEXT_SIZE=65536
LOCAL_GPU_LAYERS=999
LOCAL_THREADS=8
LOCAL_BATCH_SIZE=2048
LOCAL_PARALLEL=1
```

Install the unit:

```bash
sudo install -o root -g root -m 0644 deploy/local-llama-server.service \
  /etc/systemd/system/local-llama-server.service
sudo systemctl daemon-reload
sudo systemctl enable --now local-llama-server.service
```

Do not run another process on the same address and port. Before starting
Octoroute, validate the configured llama.cpp contract from the gateway host:

```bash
curl --fail http://127.0.0.1:8080/health
curl --fail 'http://127.0.0.1:8080/slots?fail_on_no_slot=1'
curl --fail -H 'Content-Type: application/json' \
  -d '{"model":"local-model","messages":[{"role":"user","content":"probe"}]}' \
  http://127.0.0.1:8080/v1/chat/completions/input_tokens
```

Use the actual configured endpoint instead of loopback when the model runs on
a separate trusted host.

## Network

The gateway always requires bearer authentication, but it should still be
limited to trusted LAN or VPN clients with a firewall or reverse proxy. See
[Security](security.md) for browser, rotation, reverse-proxy, and
incident-response requirements.

Allow outbound access only to:

- configured local model endpoints;
- configured cloud providers.

For the v2 example, the cloud provider is `https://openrouter.ai`.

Do not expose llama.cpp directly to untrusted clients when Octoroute is meant
to enforce privacy, admission, and spend controls.

## Readiness

Use:

```bash
curl --fail http://127.0.0.1:8081/health/live
curl --fail http://127.0.0.1:8081/health/ready
```

The second endpoint is healthy when at least one upstream is available.

Protected smoke test:

```bash
curl --fail http://127.0.0.1:8081/v1/models \
  -H "Authorization: Bearer $OCTOROUTE_API_KEY"
```

## Rollout validation

1. Verify `/health/live`.
2. Verify `/health/ready` reports the expected upstream states.
3. Send `model: local` with `X-Octoroute-Privacy: local-only`.
4. Occupy all configured local capacity and verify `model: auto` reports
   `X-Octoroute-Reason: local_busy` and follows the configured fallback policy.
5. Verify the same capacity condition with local-only returns an error rather
   than contacting cloud.
6. Verify cloud non-streaming and SSE responses expose the actual model.
7. Scrape `/metrics` with authentication.

## Shutdown

SIGINT and SIGTERM trigger graceful Axum shutdown. In-flight body streams are
allowed to finish according to Axum/hyper shutdown behavior; dropping a
client stream releases its Octoroute and upstream permits.

## Release

The initial `2.0.0` release introduced the breaking v2 configuration/API
change. Minor release `2.2.0` adds calibrated local-success forecasting,
offline threshold evaluation, bounded shadow trajectory signals, and an
opt-in enforced-mode session latch while retaining shadow mode as the default.
Do not publish or tag while the tree is dirty or verification is incomplete.

See [Migrating from Octoroute v1 to v2](migration-v2.md) for staged rollout
and rollback instructions.
