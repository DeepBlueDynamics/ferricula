# Scribe

Interface and persistence agent. Owns the REPL, TUI splash, HTTP service,
WAL/snapshot system, and the MCP connector.

## Jurisdiction

- `src/main.rs` — Arg parsing (`--serve [port]`), REPL dispatch, HTTP command dispatch, identity loading, splash screen
- `src/http.rs` — HTTP thread (tiny_http), HttpCommand enum, endpoint routing, CORS
- `src/persist.rs` — DurableEngine, WAL (postcard binary), V2 snapshots
- `src/planner.rs` — freeform->SQL rewrite (rule-based, AGENT_KEY slot for LLM)
- `tools/ferricula-mcp.py` — FastMCP stdio server, dual transport (subprocess + HTTP)

## Dual-mode operation

Ferricula runs in two modes selected by `--serve`:

- **REPL mode** (`ferricula ./data`): stdin reader thread, interactive command dispatch, TUI splash
- **HTTP mode** (`ferricula ./data --serve 8765`): tiny_http thread on port (default 8765), channel-based command dispatch to main thread

Both modes share the same main loop: drain clock events, then process commands (stdin or HTTP). Identity is loaded at startup in both modes.

### Thread model

```
main thread          http thread          clock thread
────────────         ────────────         ────────────
DurableEngine (mut)  tiny_http server     radio polling
IdentityState        parse request        entropy reservoir
drain clock_rx       create sync_channel  emit ClockEvent
drain http_rx        send HttpCommand     emit DreamTrigger
respond via reply    block on reply_rx
```

The HTTP thread never touches mutable state. Each request creates a `sync_channel(1)`, packs an `HttpCommand` with the reply sender, sends it over `mpsc::Sender<HttpCommand>`, and blocks on the reply. The main thread processes commands one at a time via `try_recv()`.

## REPL Commands

| Command | Action |
|---------|--------|
| `remember <json>` | Ingest Row + MemoryRecord + auto-index terms |
| `recall <query>` | Search + update recall stats on hits |
| `dream` | Run dream cycle, print report |
| `status` | Memory/graph/tree statistics |
| `inspect <id>` | Full memory record details |
| `keystone <id>` | Toggle keystone status |
| `connect <id1> <id2> <label>` | Create graph edge |
| `disconnect <id1> <id2>` | Remove graph edge |
| `neighbors <id>` | Show graph neighbors |
| `terms` | List prime tree terms with member counts |
| `upsert <json>` | Raw row insert (no memory envelope) |
| `delete <id>` | Delete row |
| `query <sql>` | Raw SQL query |
| `rewrite <freeform>` | Show planner rewrite |
| `topk <metric> <vec> <k>` | Vector similarity search |
| `maxid` | Highest row ID in engine |
| `get <id>` | Return row as JSON |
| `touch <id>` | Update recall stats on memory |
| `clock` | Show clock telemetry |
| `offer <hex>` | Inject entropy manually, trigger dream |
| `checkpoint` | Flush to disk |

## HTTP Endpoints

16 endpoints dispatched via `dispatch()` in `http.rs`:

| Method | Path | HttpCommand variant |
|--------|------|-------------------|
| POST | `/remember` | Remember |
| POST | `/recall` | Recall |
| POST | `/dream` | Dream |
| GET | `/status` | Status |
| GET | `/inspect/:id` | Inspect |
| POST | `/offer` | Offer |
| POST | `/keystone/:id` | Keystone |
| POST | `/connect` | Connect (JSON body: `{a, b, label}`) |
| POST | `/disconnect` | Disconnect (JSON body: `{a, b}`) |
| GET | `/neighbors/:id` | Neighbors |
| GET | `/clock` | Clock |
| POST | `/checkpoint` | Checkpoint |
| GET | `/identity` | Identity |
| GET | `/get/:id` | GetRow |
| GET | `/terms` | Terms |
| GET | `/inversion/:id` | InversionCheck |

All responses: `Content-Type: application/json`, `Access-Control-Allow-Origin: *`. OPTIONS requests return `{}` for CORS preflight. Errors return status 400 with `{"error": "..."}`.

## MCP Transport

`tools/ferricula-mcp.py` supports two transports:

- **Subprocess** (default): spawns `ferricula` binary, sends REPL commands over stdin, reads responses
- **HTTP** (when `FERRICULA_URL` is set): sends REST requests to running ferricula service

HTTP mode adds two tools not available in subprocess mode:
- `ferricula_identity()` — GET /identity
- `ferricula_inversion_check(id)` — GET /inversion/:id

## Principles

1. **WAL-first** — every mutation appends to WAL before acknowledging.
   Checkpoint atomically writes snapshot then truncates WAL.
2. **V2 backward compat** — new snapshot format (`snapshot_v2.bin`) includes
   all stores. Falls back to V1 (`snapshot.bin`) for rows-only data.
3. **Prompt detection** — MCP subprocess mode detects `\n> ` as the REPL prompt.
4. **Splash is optional** — `NO_COLOR` or `TERM=dumb` skips the ratatui splash.
   `--serve` mode also skips splash.
5. **Parse once** — `remember` parses JSON once for both Row and MemoryRecord
   fields (emotion, importance, keystone are optional extras in the same JSON).
6. **Channel isolation** — HTTP thread never mutates engine state. All mutations
   go through the main thread via mpsc channel. This avoids mutex contention.
7. **Graceful shutdown** — Ctrl+C sets `should_stop` flag, HTTP thread checks
   `running` flag each iteration, main thread checkpoints before exit.

## Persistence Format

- WAL: length-prefixed postcard frames (`u32_le + payload`)
- Snapshot V2: single postcard blob with rows, records, edges, tree state
- Identity: `identity.json` (serde_json, human-readable)
- Atomic checkpoint: write to `.tmp`, rename, truncate WAL

## Development Notes

- After modifying WalEntry variants, only append new ones at the end
- The DurableEngine.dream() method exists to satisfy the borrow checker —
  dream_cycle needs &mut MemoryStore + &Engine + &mut MemoryGraph simultaneously
- MCP server (`tools/ferricula-mcp.py`) manages ferricula as a subprocess
  with thread-safe locking and auto-restart in subprocess mode
- HTTP dispatch uses 10s reply timeout — increase if dream cycles get slow
- `json_wrap()` and `json_error()` are the response formatters for HTTP mode
