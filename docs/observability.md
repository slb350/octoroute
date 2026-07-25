# Observability

## Response headers

Every response has `X-Request-Id`. Octoroute preserves an allowlisted upstream
request ID and generates one when the response does not already contain one.
Successful upstream responses also expose:

- `X-Octoroute-Destination`
- `X-Octoroute-Reason`
- `X-Octoroute-Upstream`

The response records the destination actually returned. A local attempt that
falls back is reported as cloud with `local_early_failure`.

## Prometheus

`GET /metrics` requires the configured bearer credential.

Current v2 metrics:

```text
octoroute_route_decisions_total{destination,reason}
octoroute_local_fallbacks_total
octoroute_local_busy_spillovers_total
octoroute_upstream_requests_total{upstream,outcome,status_class}
octoroute_upstream_failures_total{upstream,phase}
octoroute_request_duration_seconds{destination}
octoroute_time_to_first_byte_seconds{destination}
octoroute_routing_duration_seconds
octoroute_in_flight_requests{destination}
```

`request_duration` measures the lifetime of a committed response body through
completion or cancellation. `time_to_first_byte` measures dispatch through
the first upstream body chunk, which Octoroute buffers before commitment.
`upstream_requests` classifies HTTP responses by status class and transport
failures as `status_class="none"`.

All labels come from bounded enums or HTTP status classes. Prompt and model
text are never used as metric labels. Octoroute deliberately does not parse
opaque response bodies merely to calculate spend; use OpenRouter generation
accounting for authoritative cloud cost reporting.

Suggested alerts:

- increasing `octoroute_upstream_failures_total{upstream="openrouter"}`;
- sustained `octoroute_local_busy_spillovers_total`;
- sustained `local_early_failure` route decisions;
- high local or cloud first-byte latency;
- in-flight requests that remain near configured concurrency limits;
- unexpected disappearance of local route decisions;
- readiness returning 503.

## Health

`/health/live` proves the process can serve HTTP.

`/health/ready` and `/health` concurrently inspect:

- Octoroute local permit availability;
- cached llama.cpp health;
- llama.cpp free-slot state;
- cached authenticated OpenRouter key probe.

The gateway is ready when either local is ready or OpenRouter is reachable.
Busy local capacity is reported separately and does not make the gateway
unready when cloud is available.

## Logging

Set `RUST_LOG` for an explicit filter or use `[observability].log_level`.

Safe logs may include:

- request ID;
- bounded destination/reason/upstream;
- safe status classes and timing;
- configuration field names.

Never log:

- bearer/OpenRouter/local keys;
- Authorization headers;
- prompt or message bodies;
- arbitrary model text as a metrics label;
- raw invalid TOML/dotenv lines.

## Upstream response headers

Octoroute filters upstream headers. It preserves content type, cache control,
retry-after, request IDs, `X-Generation-Id`, and common rate-limit
diagnostics. It strips connection-specific headers, cookies, and other
non-allowlisted data.
