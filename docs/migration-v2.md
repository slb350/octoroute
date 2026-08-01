# Migrating from Octoroute v1 to v2

Octoroute 2.x replaces local size-tier routing with one explicit boundary.
`auto` uses compatible Strix capacity or OpenRouter Auto. Semantic judgment is
configurable and defaults to non-enforcing shadow observation.

## Breaking changes

- Add `config_version = 2`.
- Replace `[models]` and the v1 `[routing]` strategy fields.
- Remove `fast`, `balanced`, and `deep` client model names.
- Remove `POST /chat`; use `POST /v1/chat/completions`.
- Supply inbound and OpenRouter credentials through environment variables.
- Treat `model: auto` as local-versus-cloud routing.

Octoroute rejects a v1 configuration at startup with a migration error. It
does not guess which old endpoint should become Strix.

## Configuration mapping

Old:

```toml
[[models.fast]]
name = "small-model"
base_url = "http://host-a:8080/v1"

[[models.balanced]]
name = "medium-model"
base_url = "http://host-b:8080/v1"

[[models.deep]]
name = "large-model"
base_url = "http://host-c:8080/v1"

[routing]
strategy = "hybrid"
```

New:

```toml
config_version = 2

[server]
host = "0.0.0.0"
port = 8081
api_key_env = "OCTOROUTE_API_KEY"

[upstreams.local]
kind = "llama_cpp"
name = "strix"
base_url = "http://127.0.0.1:8080"
model = "strixtea"
context_window = 65536
context_safety_tokens = 1024
default_max_output_tokens = 4096
max_in_flight = 1
capabilities = ["chat", "stream"]
health_path = "/health"
slots_path = "/slots?fail_on_no_slot=1"
input_tokens_path = "/v1/chat/completions/input_tokens"

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
auto_model = "openrouter/auto"
cost_quality_tradeoff = 9

[routing]
default = "prefer_local"
fallback_before_commit = true
semantic_mode = "shadow"
decision_timeout_ms = 30000
```

Create an ignored `.env` beside the configuration:

```dotenv
OCTOROUTE_API_KEY=<long random client credential>
OPENROUTER_API_KEY=<OpenRouter credential>
```

## Client model mapping

| v1 intent | v2 model |
| --- | --- |
| Let Octoroute choose | `auto` |
| Force the local model | `local` or `strixtea` |
| Use cloud selection | `cloud` or `openrouter/auto` |
| Force a cloud model | exact `provider/model` slug |

There is no direct mapping for `fast`, `balanced`, or `deep`. Select
`local`, `cloud`, or an explicit provider model based on the real workload
requirement.

## Privacy

`model: local` and the configured local alias are no-cloud boundaries. For
automatic requests that must stay on the LAN, add:

```http
X-Octoroute-Privacy: local-only
```

If Strix is busy, unhealthy, incompatible, or over context, Octoroute returns
an error instead of falling back.

## Rollout

1. Keep the v1 binary, configuration, and service unit available.
2. Start v2 on a different port.
3. Verify liveness and readiness.
4. Test explicit `local`, `cloud`, and local-only requests.
5. Validate one non-streaming and one streaming OpenRouter response.
6. Occupy Strix and confirm `auto` reports `local_busy` and routes cloud.
7. Move a controlled client subset to v2.
8. Move llama.cpp to loopback-only ingress after all clients use Octoroute.

## Rollback

Stop v2, restore the v1 service and configuration, and return clients to the
old endpoint. OpenRouter credentials are independent of binary rollback;
rotate them if exposure is suspected.
