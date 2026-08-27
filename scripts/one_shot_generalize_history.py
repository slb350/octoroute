from pathlib import Path

path = Path("docs/plans/local-cloud-routing-gateway.md")
text = path.read_text(encoding="utf-8")

text = text.replace(
    "Octoroute will become the single OpenAI-compatible gateway for the user's\n"
    "personal AI traffic:",
    "Octoroute will become a single OpenAI-compatible gateway for local-first\n"
    "AI traffic:",
)

start_heading = "### Current local model runtime"
end_heading = "### Relevant upstream behavior"
start = text.index(start_heading)
end = text.index(end_heading, start)
replacement = """### Reference local model runtime

The initial implementation was validated against a llama.cpp endpoint with the
following observable contract:

- a configured model alias and immutable model revision;
- a configured context window and output reserve;
- one or more parallel request slots;
- slot monitoring through `GET /slots`;
- health monitoring through `GET /health`;
- exact request token counting through
  `POST /v1/chat/completions/input_tokens`;
- optional Prometheus metrics when the server enables them;
- an explicitly configured listener and process manager.

These are deployment inputs rather than Octoroute product constants. The
routing and admission design must work across model releases, context sizes,
hardware classes, slot counts, network layouts, and service managers.

Admission control remains a first-class routing input. A request that is valid
for a local model may still spill to cloud immediately when all configured local
capacity is occupied, unless caller privacy forbids cloud disclosure.

Benchmark and calibration evidence is scoped to the configured
`model_revision`. Changing weights under the same model alias requires a new
revision identity and fresh evidence before previously measured capability
assumptions are enforced.

"""
text = text[:start] + replacement + text[end:]

phase_start = text.index("### Phase 10: Deployment and release")
phase_end = text.index("## Test layout", phase_start)
phase_replacement = """### Phase 10: Deployment and release

1. Build and install Octoroute as a managed service.
2. Run each local model endpoint under a durable process manager.
3. Restrict local model ingress to the gateway host or trusted network.
4. Enable and scrape upstream metrics where supported.
5. Choose non-conflicting listener addresses and ports through configuration.
6. Point clients at Octoroute rather than directly at model endpoints.
7. Start in observation mode with explicit local and cloud requests.
8. Enable `auto` for a controlled client subset.
9. Validate metrics, logs, costs, privacy, and fallback behavior.
10. Run fault drills for exhausted local capacity, stopped local endpoints,
    unavailable cloud providers, client disconnects, invalid credentials, and
    context overflow.
11. Complete the security hardening checklist.
12. Verify stable and the `1.90` toolchain channel, which CI pins as
    `1.90.0`.
13. Publish 2.0.0 only after the migration and rollback paths are tested.

"""
text = text[:phase_start] + phase_replacement + text[phase_end:]

replacements = {
    "Its live local model response schema must be pinned in a contract fixture before\n"
    "  it becomes an admission dependency.":
        "The configured endpoint's response schema must be pinned in a contract\n"
        "  fixture before it becomes an admission dependency.",
    "Spill from the local model endpoint to cloud before response commitment when local model is busy,\n"
    "   unhealthy, incompatible, or fails early.":
        "Spill from a local endpoint to cloud before response commitment when local\n"
        "   capacity is busy, unhealthy, incompatible, or fails early.",
    "Letting OpenRouter route directly to the private local model model.":
        "Letting OpenRouter route directly to a private local model endpoint.",
    "High availability across a complete local model host failure.":
        "High availability across a complete local endpoint failure.",
    "It must not silently guess which old tier represents local model.":
        "It must not silently guess which old tier represents the local model.",
    "The live local model": "The configured local model",
    "live local model": "configured local model",
    "local model host": "local endpoint host",
    "A personal deployment can restrict it": "An operator can restrict it",
    "representative personal requests": "representative workloads",
    "personal configuration": "operator-specific configuration",
}
for old, new in replacements.items():
    text = text.replace(old, new)

for forbidden in [
    "Gitea",
    "abandoned SSH",
    "abandoned-session",
    "personal AI traffic",
    "personal deployment",
    "personal requests",
    "personal configuration",
    "Port 3000",
    "Port 8081 is free",
    "inspected on 2026-07-22",
]:
    if forbidden in text:
        raise SystemExit(f"deployment-specific phrase remains: {forbidden}")

path.write_text(text, encoding="utf-8")
