# Ferricula Tool Inventory

## External Surface (cognitive)

Tools available to the LLM singleton via `FERRICULA_SURFACE=cognitive`.
The agent thinks, remembers, recalls, observes, and introspects.

```
ferricula_remember(text, channel, emotion, importance, keystone) — Store memory with auto-embedding
ferricula_recall(query) — Search memories by text, returns enriched results
ferricula_reflect(thought, importance) — Working memory with faster decay
ferricula_observe(path, summary) — File observation, keystoned reference
ferricula_inspect(id) — View memory details: text, fidelity, emotion, graph
ferricula_connect(a, b, label) — Create intentional graph relation
ferricula_neighbors(id) — Browse graph connections
ferricula_status() — Aggregate memory statistics
ferricula_health() — Component diagnostics (ferricula + chonk)
ferricula_identity() — Agent identity (hexagram, zodiac, archetypes)
```

## Internal Surface (system)

Tools available to archetypes/steward/clock via `FERRICULA_SURFACE=system`.
System maintenance, quality auditing, raw access.

```
ferricula_dream() — Run consolidation cycle (normally clock-driven)
ferricula_keystone(id) — Toggle decay immunity
ferricula_checkpoint() — Flush WAL to snapshot
ferricula_offer_entropy(source) — Inject entropy, trigger dream
ferricula_inversion_check(id) — Semantic fidelity via vec2text
ferricula_terms() — Prime tree term inspection
ferricula_query(sql) — Raw SQL query
ferricula_disconnect(a, b) — Remove graph edges
ferricula_clock() — Clock telemetry (ticks, dreams, entropy, radio)
```

## Surface Selection

Set via `--surface` CLI arg or `FERRICULA_SURFACE` env var:
- `cognitive` — 10 tools (external LLM agents)
- `system` — 9 tools (internal archetypes)
- `all` — 19 tools (operator/debug, default)
