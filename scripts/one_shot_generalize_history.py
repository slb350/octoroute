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
}
for old, new in replacements.items():
    text = text.replace(old, new)

for forbidden in [
    "Gitea",
    "abandoned SSH",
    "personal AI traffic",
    "Port 3000",
    "Port 8081 is free",
    "inspected on 2026-07-22",
]:
    if forbidden in text:
        raise SystemExit(f"deployment-specific phrase remains: {forbidden}")

path.write_text(text, encoding="utf-8")
