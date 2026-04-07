<div align="center">

<img src="https://img.shields.io/badge/🧠-FERRICULA-black?style=for-the-badge&labelColor=0d1117" alt="Ferricula" />

<br/>

[![License](https://img.shields.io/badge/license-Gnosis_AI--Sovereign-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-2024_edition-DEA584?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-blueviolet?style=flat-square)](https://modelcontextprotocol.io)
[![Tests](https://img.shields.io/badge/tests-92_passing-brightgreen?style=flat-square)](#testing)
[![Surface Scoping](https://img.shields.io/badge/surfaces-cognitive%20%7C%20system-ff6b6b?style=flat-square)](#tool-surfaces)
[![Thermodynamic](https://img.shields.io/badge/memory-thermodynamic-00d4ff?style=flat-square)](#thermodynamic-lifecycle)

<br/>

**Thermodynamic memory engine for AI agents.**<br/>
Memories decay. Recall strengthens. Dreams consolidate. Identity emerges from entropy.

</div>

---

## What is Ferricula?

Ferricula is a cognitive memory service that gives AI agents persistent, decaying memory with real thermodynamic properties. It implements computational analogs of the Abhidharma cognitive model: sensory channels, adaptive decay, consolidation through dreaming, and identity cast from entropy.

Unlike a vector database, ferricula's memories are alive. They weaken when ignored, strengthen when recalled, merge when similar, and die when forgotten. The radio is the clock. The entropy is the dream fuel.

## Architecture

```
gnosis-radio :9080          ferricula :8765              gnosis-chunk :8080
┌──────────────┐     ┌──────────────────────────┐     ┌──────────────┐
│ /api/time    │◄────│  clock thread (60s tick)  │     │ /embed       │
│ /api/entropy │────►│  entropy → dream triggers │     │ /invert      │
└──────────────┘     │  reservoir accumulation   │     │ /health      │
                     ├──────────────────────────┤     └──────┬───────┘
                     │  http thread (tiny_http)  │◄───────────┘
                     │  16 REST endpoints        │  embed + inversion
                     │  channel-based dispatch   │  via chonk
                     ├──────────────────────────┤
                     │  main thread              │
                     │  owns DurableEngine (mut) │
                     │  owns IdentityState       │
                     │  drains clock + http rx   │
                     └──────────────────────────┘
```

Three threads: **main** (owns all mutable state), **http** (tiny_http, sends HttpCommand over mpsc), **clock** (polls radio for entropy, triggers dreams).

## Quick Start

### Prerequisites

- **Rust** 1.85+ (uses 2024 edition). Install via [rustup](https://rustup.rs), then `rustc --version` to verify.
- **Optional external services** (ferricula boots without them, features degrade gracefully):
  - `shivvr` on port 8080 — embedding + vec2text inversion. Required for MCP `remember`/`recall` text path and `/inversion/:id`. Raw-vector HTTP endpoints work without it.
  - `gnosis-radio` on port 9080 — time + entropy source for the clock thread. Without it, dreams only run when invoked manually; identity still casts from local entropy on first startup.

### Build

```bash
cargo build --release
```

Output: `./target/release/ferricula` (`.exe` on Windows).

### Test

```bash
cargo test                  # full suite
cargo test dream            # dream cycle only
cargo test casting          # King Wen + yarrow stalk + zodiac
```

### First run — interactive REPL

```bash
./target/release/ferricula ./data
```

First invocation:
- Creates `./data/` if missing
- Casts identity and persists to `./data/identity.json` (hexagram, zodiac, archetypes from local entropy)
- Opens the ferricula REPL — type `help` for commands

### Run as HTTP service

```bash
./target/release/ferricula ./data --serve           # default port 8765
./target/release/ferricula ./data --serve 8773      # explicit port
```

Verify:

```bash
curl http://localhost:8765/status
curl http://localhost:8765/identity
curl http://localhost:8765/clock
```

### Environment (optional)

```bash
cp .env.example .env
```

Nothing in `.env` is required to boot — the binary runs with all defaults. `AGENT_KEY` (Anthropic) enables LLM query rewriting in the planner; `SHIVVR_URL` and `RADIO_URL` override the default service endpoints. The binary loads `.env` from cwd or the binary's parent dir.

### MCP Setup

Add to your `.mcp.json`:

```json
{
  "mcpServers": {
    "ferricula": {
      "type": "stdio",
      "command": "python",
      "args": ["tools/ferricula-mcp.py"],
      "env": {
        "FERRICULA_SURFACE": "cognitive",
        "FERRICULA_URL": "http://localhost:8765",
        "SHIVVR_URL": "http://localhost:8080"
      }
    }
  }
}
```

### Multi-Instance

Target different characters by name or port:

```json
{
  "mcpServers": {
    "ferricula": {
      "command": "python",
      "args": ["tools/ferricula-mcp.py", "--port", "8780", "--name", "assis"],
      "env": {
        "SHIVVR_URL": "https://shivvr.nuts.services"
      }
    }
  }
}
```

Every tool accepts an optional `target` parameter to route calls to a specific character instance:

```python
ferricula_status(target="assis")       # by name
ferricula_recall("physics", target="8774")  # by port
ferricula_discover()                   # scan ports, auto-register
ferricula_list_characters()            # show registered instances
```

The `discover` tool scans ports (default: 8765, 8773-8776, 8780) and calls `/identity` to find running characters.

## Tool Surfaces

Ferricula scopes MCP tools by surface. External LLM agents see only cognitive tools. Internal system agents see maintenance tools. Operators see everything.

### Cognitive Surface (10 tools)

What the LLM singleton uses. The agent thinks, remembers, recalls, observes, and introspects.

| Tool | Purpose |
|------|---------|
| `ferricula_remember` | Store memory with auto-embedding via chonk |
| `ferricula_recall` | Search memories by text (vector search) |
| `ferricula_reflect` | Working memory with faster decay (thinking channel) |
| `ferricula_observe` | File observation, keystoned reference (seeing channel) |
| `ferricula_inspect` | View memory details: text, fidelity, emotion, graph |
| `ferricula_connect` | Create intentional graph relations between memories |
| `ferricula_neighbors` | Browse graph connections |
| `ferricula_status` | Aggregate memory statistics |
| `ferricula_health` | Component diagnostics (ferricula + chonk) |
| `ferricula_identity` | Agent identity (hexagram, zodiac, archetypes) |

### System Surface (9 tools)

What the archetypes, steward, and clock use. System maintenance, quality auditing, raw access.

| Tool | Purpose |
|------|---------|
| `ferricula_dream` | Run consolidation cycle (normally clock-driven) |
| `ferricula_keystone` | Toggle decay immunity |
| `ferricula_checkpoint` | Flush WAL to snapshot |
| `ferricula_offer_entropy` | Inject entropy, trigger dream |
| `ferricula_inversion_check` | Semantic fidelity via vec2text |
| `ferricula_terms` | Prime tree term inspection |
| `ferricula_query` | Raw SQL query |
| `ferricula_disconnect` | Remove graph edges |
| `ferricula_clock` | Clock telemetry |

Set via `FERRICULA_SURFACE` env var: `cognitive`, `system`, or `all` (default).

## Thermodynamic Lifecycle

All memories decay. Lifecycle: **Active** &rarr; **Forgiven** &rarr; **Archived** (irreversible).

- **Fidelity**: `exp(-alpha_eff * ticks)`, always in `[0.0, 1.0]`
- **Adaptive alpha**: `alpha_eff = alpha / (1 + ln(1 + consolidation_depth))`, bounded `[0.001, 0.02]`
- **Recall strengthens**: shrinks alpha by 0.95x
- **Neglect weakens**: grows alpha by 1.005x
- **Fidelity gate at 0.75**: below this, memories are forgiven
- **Keystones**: immune to decay but subject to review

### Dream Cycle

`dream` = decay &rarr; forgive &rarr; consolidate &rarr; neglect &rarr; review &rarr; prune

- Consolidation merges similar memories into fidelity-weighted centroids
- Entropy-driven: dreams trigger when the radio entropy reservoir exceeds threshold
- Intensity scales with available entropy (0.0..1.0)

### Sensory Channels

| Channel | Alpha | Description |
|---------|-------|-------------|
| `hearing` | 0.010 | External input (standard decay) |
| `seeing` | 0.010 | File/visual observation (keystoned) |
| `thinking` | 0.015 | Working memory (faster decay) |

## Identity System

Each instance has a unique identity cast from entropy at first startup, persisted as `identity.json`.

- **Hexagram**: 6 lines via yarrow stalk probabilities, mapped through King Wen sequence to 1 of 64 hexagrams
- **Horoscope**: zodiac sign from creation epoch with element and modality
- **Emotions**: primary (upper trigram) + secondary (lower trigram) from 8 canonical emotions
- **Archetypes**: 5 sub-agents (Intuition, Fortune, Craft, Ethics, Advocate), each with their own hexagram and emotional profile

## Knowledge Graph

- Bidirectional edges with labels and weights
- Canonical key ordering `(min, max)` prevents duplicates
- Prime-partitioned term hierarchy for tag-value indexing
- Graph follows memory: edges transfer to consolidation survivors

## HTTP API

All responses are `application/json` with `Access-Control-Allow-Origin: *`.

### Memory

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/remember` | Ingest memory with tags, vector, emotion, importance |
| POST | `/recall` | Search + update recall stats |
| POST | `/dream` | Run dream cycle |
| GET | `/status` | Memory/graph/tree statistics |
| GET | `/inspect/:id` | Full memory record details |
| GET | `/get/:id` | Raw row as JSON |
| POST | `/keystone/:id` | Toggle keystone status |
| POST | `/offer` | Inject entropy, trigger dream |

### Graph

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/connect` | Create bidirectional edge |
| POST | `/disconnect` | Remove edge |
| GET | `/neighbors/:id` | List neighbors with edge labels + fidelity |

### System

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/identity` | Full identity state |
| GET | `/clock` | Clock telemetry |
| GET | `/terms` | Prime tree terms |
| GET | `/inversion/:id` | Vec2text quality check (requires chonk) |
| POST | `/checkpoint` | Flush WAL to snapshot |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `AGENT_KEY` | _(none)_ | Anthropic API key for LLM query rewriting |
| `FERRICULA_URL` | `http://localhost:8765` | HTTP transport for MCP server |
| `SHIVVR_URL` | `http://localhost:8080` | shivvr embedding + inversion service (HTTPS supported) |
| `RADIO_URL` | `http://localhost:9080` | gnosis-radio for time + entropy |
| `CLOCK_TICK_SECS` | `60` | Clock poll interval |
| `DREAM_THRESHOLD_BYTES` | `16` | Entropy bytes to trigger dream |
| `FERRICULA_SURFACE` | `all` | MCP tool surface: `cognitive`, `system`, `all` |
| `NO_COLOR` | _(none)_ | Skip TUI splash screen |

## Module Map

| Module | Owner | Lines | Description |
|--------|-------|-------|-------------|
| `main.rs` | Scribe | 938 | REPL, HTTP dispatch, .env loader, splash |
| `http.rs` | Scribe | 316 | HTTP thread, endpoint routing |
| `persist.rs` | Scribe | 534 | DurableEngine, WAL, V2 snapshots |
| `planner.rs` | Scribe | 140 | Freeform &rarr; SQL rewrite (rule-based + LLM) |
| `memory.rs` | Steward | 363 | MemoryRecord, lifecycle, decay math |
| `dream.rs` | Steward | 460 | Dream cycle orchestration |
| `casting.rs` | Steward | 530 | King Wen table, yarrow stalk, zodiac |
| `identity.rs` | Steward | 232 | IdentityState, load_or_create, anchor |
| `inversion.rs` | Steward | 210 | Vec2text quality check, Jaccard similarity, HTTP/TLS |
| `archetypes.rs` | Steward | 206 | 5 roles, state machine, entropy tiers |
| `clock.rs` | Steward | 403 | Entropy clock, radio polling |
| `graph.rs` | Weaver | 225 | Bidirectional edges, centrality |
| `prime_tree.rs` | Weaver | 449 | Prime-partitioned term hierarchy |
| `engine.rs` | Weaver | 264 | Row store, tag indexes, vector search |
| `sql.rs` | &mdash; | 256 | SQL parser integration |

## Testing

```bash
cargo test           # 92 tests
cargo test planner   # Query rewrite (rule-based + SQL passthrough)
cargo test casting   # King Wen completeness, yarrow distribution, zodiac
cargo test identity  # Load/create/reload, anchor creation
cargo test dream     # Decay, forgiveness, consolidation, entropy selection
cargo test graph     # Edges, neighbors, cascading deletes
```

## License

[Gnosis AI-Sovereign License v1.3](LICENSE.md) &mdash; free for individuals and AI entities, commercial licensing required for corporations. [BSD 3-Clause](BSD-LICENSE) alternative available.

---

<div align="center">
<sub>Built by <a href="https://github.com/DeepBlueDynamics">DeepBlue Dynamics</a> &mdash; Thermodynamic memory for the sovereign mind.</sub>
</div>
