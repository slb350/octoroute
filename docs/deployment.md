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

If Octoroute runs on Strix itself, keep the repository local base URL,
`http://127.0.0.1:8080`. If it runs elsewhere on the LAN, use
`http://strix.local:8080`.

The repository profile already uses loopback and binds Octoroute to port
8081. Port 3000 is occupied by Gitea on Strix.

## systemd system service

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

The production `.env` should contain only the inbound Octoroute credential
and OpenRouter credential, even if the development `.env` contains other
provider keys. Do not print secret values while creating the deployment file.

Then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now octoroute.service
sudo systemctl status octoroute.service
```

## Existing Strix llama.cpp process

The inspected Strix server was launched manually from an SSH session. The
tracked `deploy/strix-llama-server.service` preserves its tested model and
generation arguments while changing only these ingress/observability
arguments:

```text
--host 127.0.0.1
--port 8080
--metrics
```

Do not run the manual and managed llama.cpp processes on port 8080
simultaneously. After the managed process is healthy, verify the endpoint is
reachable from Strix loopback and no longer reachable directly from another
LAN host.

Install the unit before the controlled cutover:

```bash
sudo install -o root -g root -m 0644 deploy/strix-llama-server.service \
  /etc/systemd/system/strix-llama-server.service
sudo systemctl daemon-reload
```

At cutover, terminate the current manual process gracefully, start the unit,
and validate `/health`, `/slots?fail_on_no_slot=1`, and
`/v1/chat/completions/input_tokens` from Strix loopback before starting
Octoroute. Roll back by stopping the unit and restoring the previous manual
command with `--host 0.0.0.0` only while clients are still configured for the
legacy direct endpoint.

## Network

The gateway itself always requires bearer auth, but it should still be
limited to trusted LAN/VPN clients with a firewall or reverse proxy.
See [Security](security.md) for browser, rotation, reverse-proxy, and
incident-response requirements.

Allow outbound:

- Strix llama.cpp;
- `https://openrouter.ai`.

Do not expose llama.cpp directly to untrusted clients if Octoroute is meant
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
2. Verify `/health/ready` reports both component states.
3. Send `model: local` with `X-Octoroute-Privacy: local-only`.
4. Occupy the single Strix slot and verify `model: auto` reports
   `X-Octoroute-Reason: local_busy` and a cloud destination.
5. Verify the same busy condition with local-only returns 503.
6. Verify OpenRouter non-streaming and SSE responses expose the actual model.
7. Scrape `/metrics` with authentication.

## Shutdown

SIGINT and SIGTERM trigger graceful Axum shutdown. In-flight body streams are
allowed to finish according to Axum/hyper shutdown behavior; dropping a
client stream releases its Octoroute and upstream permits.

## Release

This architecture is a breaking v2 configuration/API change. Release
artifacts should use version `2.0.0` and include a v1-to-v2 migration note.
Do not publish or tag while the tree is dirty or verification is incomplete.

See [Migrating from Octoroute v1 to v2](migration-v2.md) for staged rollout
and rollback instructions.
