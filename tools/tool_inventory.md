# Ferricula Tool Inventory

## External Surface (cognitive)

Tools available to the LLM singleton via `FERRICULA_SURFACE=cognitive`.
The agent thinks, remembers, recalls, observes, and introspects.

```
ferricula_remember(text, channel, emotion, importance, keystone, target) — Store memory with auto-embedding
ferricula_recall(query, target) — Search memories by text, returns enriched results
ferricula_reflect(thought, importance, target) — Working memory with faster decay
ferricula_observe(path, summary, target) — File observation, keystoned reference
ferricula_inspect(id, target) — View memory details: text, fidelity, emotion, graph
ferricula_connect(a, b, label, target) — Create intentional graph relation
ferricula_neighbors(id, target) — Browse graph connections
ferricula_status(target) — Aggregate memory statistics
ferricula_health(target) — Component diagnostics (ferricula + shivvr)
ferricula_identity(target) — Agent identity (hexagram, zodiac, archetypes)
```

## Internal Surface (system)

Tools available to archetypes/steward/clock via `FERRICULA_SURFACE=system`.
System maintenance, quality auditing, raw access.

```
ferricula_dream(target) — Run consolidation cycle (normally clock-driven)
ferricula_keystone(id, target) — Toggle decay immunity
ferricula_checkpoint(target) — Flush WAL to snapshot
ferricula_offer_entropy(source, target) — Inject entropy, trigger dream
ferricula_inversion_check(id, target) — Semantic fidelity via vec2text
ferricula_terms(target) — Prime tree term inspection
ferricula_query(sql, target) — Raw SQL query
ferricula_disconnect(a, b, target) — Remove graph edges
ferricula_clock(target) — Clock telemetry (ticks, dreams, entropy, radio)
```

## Multi-Instance Tools (always registered)

```
ferricula_discover(ports) — Scan ports, call /identity, register characters
ferricula_list_characters() — Show all registered character instances
```

## Targeting

All tools accept an optional `target` parameter (character name or port number).
Use `ferricula_discover()` to scan and register, then target by name:

```
ferricula_status(target="assis")
ferricula_recall("physics", target="8774")
```

## Surface Selection

Set via `--surface` CLI arg or `FERRICULA_SURFACE` env var:
- `cognitive` — 10 tools (external LLM agents)
- `system` — 9 tools (internal archetypes)
- `all` — 21 tools (operator/debug + multi-instance, default)
