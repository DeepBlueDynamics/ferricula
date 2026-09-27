# ferricula_v2 — Fusion Plan

**Fusing agent memories (ferricula) + document search (lume) + chunking/embeddings (shivvr) into one Rust workspace.**

_July 2026. Inputs: `scan/lume.md`, `scan/ferricula.md`, `scan/shivvr.md` (surveyed read-only by the Varying Viper pane), `ferricula/INTEGRATION_PLAN.md`, `ferricula/plan/specs/ferricula_v1_roadmap.md`, direct reads of the three codebases. No source project was modified._

---

## 1. Thesis

The three projects have already grown into each other. The fusion isn't a new idea — it's acknowledging what the code is already doing and removing the HTTP tape holding it together:

- **shivvr vendors lume** — `lume-hybrid = { path = "vendor/lume-hybrid" }` in shivvr's Cargo.toml. Every shivvr session builds a lume `Bm25Index` on ingest and fuses dense + lexical scores with RRF.
- **lume calls shivvr** — `hybrid.rs` proxies embeddings over ureq. Because shivvr has no `/embed` endpoint, `embed_text()` ingests into a throwaway scratch store (`/temp/lume-embed-scratch/ingest`) and reads the vector off the response. That is a hack standing in for a function call.
- **ferricula calls shivvr** — `embed.rs` fetches vectors the same way (`/temp/ferricula/ingest`); MCP `remember`/`recall` text paths hard-depend on the sidecar. Meanwhile ferricula has quietly absorbed its own `tokenizer.rs`, `bb25.rs`, `sparse.rs`, and `inversion.rs` — a partial reimplementation of the other two projects.
- **shivvr is ephemeral** — the sled persistence in its spec was never built; sessions are in-memory `HashMap`s. Restart shivvr and every embedding lume and ferricula ever pushed is gone until re-ingest. Meanwhile ferricula already persists vectors durably (`Row.vector`, postcard + WAL). Two vector stores, one real.

**ferricula_v2 = one workspace, one binary, trait seams instead of HTTP seams.** The `INTEGRATION_PLAN.md` sidecar federation becomes in-process calls. The v1 roadmap's substrate work (typed nodes, DenseAM dreams, fidelity probes, evals) is **not** re-planned here — it runs on top of this substrate. V2 keeps one sovereign internal agent and preserves the five Wisdom Kings as bounded, traceable perspectives whispering into that agent's deliberation (§5a). They are not five identities, schedulers, memory owners, or tool users.

What dies: three deploy targets, three auth layers, hand-rolled HTTP clients, the scratch-store embed hack, the vendored lume copy drifting from upstream, and the 60-second ureq timeouts pretending to be error handling.

What survives, on purpose: shivvr as an *optional* remote embedder (GPU box / nuts.services deploy), and lume as a standalone CLI. Both become thin binaries over the shared crates instead of forks.

---

## 2. Target architecture

```
                          ferricula_v2 (one process)
┌────────────────────────────────────────────────────────────────────┐
│  ferricula-server  (axum: HTTP + MCP on one port; CLI/REPL)        │
│      │                                                             │
│      ▼ mpsc command channel (main thread owns all mutable state,   │
│        exactly like ferricula's current 3-thread design)           │
│  ┌──────────────────────────┐   ┌───────────────────────────────┐  │
│  │ ferricula-core           │   │ ferricula-search (ex-lume)    │  │
│  │ MemoryRecord + Row       │◄──┤ FST tagger · field BM25       │  │
│  │ roaring tag index        │   │ spelling · entity graph/SKG   │  │
│  │ postcard snapshot + WAL  │   │ graph_search · sections       │  │
│  │ decay · gates · graph    │   └───────────────┬───────────────┘  │
│  │ prime_tree · skg         │                   │                  │
│  └────────────┬─────────────┘                   │                  │
│               │        Embedder / Chunker / Inverter traits        │
│               ▼                                 ▼                  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ ferricula-semantic (ex-shivvr)   [feature = "ml"]            │  │
│  │ GTR-T5 ONNX embedder · Monte Carlo chunker · vec2text        │  │
│  │ inverter · simsimd cosine · per-agent orthogonal crypto      │  │
│  │ impls: LocalOnnx | RemoteShivvr | OpenAi                     │  │
│  └──────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ ferricula-cognition: dream · abhidharma (internal agent) ·   │  │
│  │ casting · pali · identity · clock (entropy) · planner        │  │
│  └──────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────┘
   external, optional: gnosis-radio :9080 (entropy) · remote shivvr
   (only when running with a remote/GPU embedder) · Anthropic API
   (planner query rewrite)
```

### Workspace layout

```
ferricula_v2/
├── Cargo.toml                # [workspace]
├── PLAN.md                   # this file
├── scan/                     # read-only surveys of the three sources
├── crates/
│   ├── ferricula-core/       # memory, engine, persist, graph, skg, prime_tree, model
│   ├── ferricula-search/     # fst tagger, bm25, spelling, semantic_mesh, graph_search, regex, crawl
│   ├── ferricula-semantic/   # embedder, chunker, inverter, similarity, crypto, openai  [feature "ml"]
│   ├── ferricula-cognition/  # dream, archetypes, casting, pali, identity, clock, planner
│   └── ferricula-server/     # axum HTTP+MCP, CLI/REPL, auth — the `ferricula` binary
└── bins (thin, over shared crates):
    ├── lume                  # standalone CLI, preserved surface   [crate ferricula-search]
    └── shivvr                # standalone sidecar, preserved API   [crate ferricula-semantic]
```

Five crates, not ten. The oracle already warned about this: 賁 → 白賁 — elegant structure, no over-engineering.

### Source → destination map

| From | Modules | To | Notes |
|---|---|---|---|
| ferricula | engine, memory, persist, model, sparse, graph, skg, prime_tree | `ferricula-core` | unchanged semantics; snapshot v5 envelope + WAL kept, migration test required |
| ferricula | dream, casting, pali, identity, clock, planner, archetype roles/tiers | `ferricula-cognition` | `abhidharma.rs` is the internal-agent surface; `wisdom.rs` preserves the five roles as bounded perspectives rather than autonomous sub-agents (§5a) |
| ferricula | http, mcp, auth, server_config, main (REPL) | `ferricula-server` | unified with lume's MCP tools |
| ferricula | tokenizer, bb25, inversion, embed, ec_key, transform | **deleted** | superseded by search/semantic crates (transform's sanitizers fold into core) |
| lume | lib (tagger), bm25, spelling, semantic_mesh, graph_search, regex, stream, answer, eval, crawl | `ferricula-search` | keep JSON index format v2.0-compatible |
| lume | fast_retrieval (MiniRoaring, PrimeFilter) | **replaced** | standardize on the `roaring` crate; PrimeFilter merges with ferricula's prime_tree if benchmarks justify it, else dropped |
| lume | hybrid (shivvr HTTP client), inversion | **replaced** | becomes trait calls into `ferricula-semantic` |
| lume | agent, main (CLI) | `ferricula-search` + thin `lume` bin | MCP tools re-registered on the unified server |
| shivvr | embedder, chunker, inverter, similarity, crypto, openai | `ferricula-semantic` | `ml` feature gates ort/tokenizers/simsimd, exactly as today |
| shivvr | store, temp_store (RRF + FST boost) | RRF/boost logic → `ferricula-search`; session stores **deleted** | chunks land in core's durable store instead of ephemeral maps |
| shivvr | api, auth (JWT/JWKS), agent, main | thin `shivvr` bin + `ferricula-server` | one auth story (see §5) |
| shivvr | vendor/lume-hybrid | **deleted** | the whole point |

---

## 3. Unified data model

**One memory record, two retrieval planes, one linkage.**

- **Experiential plane** (`ferricula-core`): `Row { id, tags, vector, refs }` + `MemoryRecord` thermodynamic envelope — unchanged. Chunk provenance from ingestion is added to `refs`/tags: `source`, `start_byte..end_byte`, `chunk_quality` (inversion round-trip score, when computed).
- **Document plane** (`ferricula-search`): lume's `Section`s and JSON indexes (`state.json`, `bm25.json`, `spelling.json`, `entity_graph.json`) — unchanged format in v2.0 so existing `.lume-index` dirs keep working.
- **Linkage**: `source_url → (db_dir, section_index, section_hash)` exactly as the v1 roadmap Phase 3 specifies; verbatim text recovered via `Bm25Index.sections`, FNV-1a section hashes detect staleness (already implemented in lume's hybrid.rs — the mechanism survives, only the transport dies).

**Embeddings**: dimension becomes config, not constant. Default stays GTR-T5 768-d so existing ferricula snapshots load unchanged. The `Embedder` trait carries `fn dim(&self)`; the roadmap's Phase 4 modernization (Qwen3-Embedding / BGE-M3) becomes swapping the impl + a re-embed migration, not surgery. The 384-d bge-small in shivvr's old specs is dead — the deployed code is GTR-T5 and lume asserts 768-d; v2 codifies that.

**Persistence rule**: anything worth embedding is worth persisting. Chunk + embed happens in-process at ingest; vectors go straight into core's postcard/WAL store. The only ephemeral store left is a scratch namespace for transient searches (temp queries, planner experiments), explicitly documented as such.

---

## 4. Unified recall — the payoff

One `recall(query)` runs the whole stack in-process, embedding the query **once**:

```
query
  ├─ FST tag pass (tagger) ──────────────► tag/intent hits
  ├─ roaring bitmap tag filter ──────────► candidate set        (core)
  ├─ BM25 field-aware lexical ───────────► lexical scores       (search)
  ├─ SKG / entity-graph walk ────────────► graph boost          (search)
  ├─ dense cosine over candidates ───────► semantic scores      (semantic)
  ├─ blend: RRF (shivvr) or multiplicative/normalized (lume) — config
  ├─ resonance gates: fidelity · lifecycle · temporal · heat · load
  ├─ thermodynamic side-effects: recall_shrink α, heat, last_recalled
  └─ hydrate: verbatim sections via source_url linkage + ghost echoes
```

Today that path costs three processes and 2–4 HTTP round-trips with a 60 s timeout each. In v2 it's function calls; "exact cosine over roaring-bitmap-filtered candidates" stays the storage strategy per roadmap decision #3 — brute force until it hurts.

The blend-mode question (RRF vs lume's multiplicative/normalized) is settled empirically in Phase D on the Monte Cristo corpus with lume's existing `eval` harness — not by taste.

---

## 5. Server & API surface

One port, axum, async at the edge only. The core engine stays synchronous with a single owning thread draining an mpsc channel — ferricula's current design, kept deliberately: determinism is a feature, and nothing in the hot path awaits.

- **HTTP**: ferricula's endpoints (`/remember`, `/recall`, `/dream`, `/status`, `/identity`, ...) + document endpoints (`/docs/search`, `/docs/index`) + utility endpoints shivvr never had cleanly: **`POST /embed`** (killing the scratch-store hack forever), `POST /chunk`, `POST /invert`, `/agent/:id/{register,encrypt,decrypt}`.
- **MCP**: one rmcp streamable-HTTP server exposing the union: `ferricula_*` tools (remember, recall, reflect, dream, query, inversion_check, ...) + `lume_*` tools (index, search, generate) + `shivvr`-lineage utilities (embed, invert). One registry, one auth, one port.
- **Auth**: pick ferricula's x25519/HKDF agent-key scheme for agent identity and keep shivvr's JWT/JWKS only on the standalone `shivvr` bin for nuts.services compatibility. Do not run two schemes in one server.
- **CLI/REPL**: ferricula's REPL is the primary console; `lume` bin preserves the document-workflow CLI (`index`, `search`, `crawl`, `eval`, `generate`).

## 5a. One sovereign agent with Wisdom-King whispers

The original `archetypes.rs` established five roles and dream-intensity activation tiers, but most behavioral effects were stubs. V2 retains that shape without pretending it is a fleet of agents:

- **One internal agent** behind an `InternalAgent` trait in `ferricula-cognition/abhidharma.rs`: `advise(AbhidharmaContext) -> Decision`. Two impls: `LlmAgent` (Anthropic, absorbing `planner.rs`'s client) and `NoAgent` (deterministic defaults — the engine runs headless with no key).
- **Five bounded perspectives** in `wisdom.rs`: Intuition, Fortune, Craft, Ethics, and Advocate. They emit typed, expiring modulation proposals; they own no memories, tools, credentials, task queues, or public identity. Steve may integrate or reject every whisper.
- **Original activation tiers survive**: minimal is thermodynamic-only, moderate invites Intuition and Fortune, and full invites all five. The tier scopes deliberation; it never grants independent agency.
- **Sovereign choice is explicit** in `agency.rs`: engage, observe, ignore, defer, or establish a boundary are all valid outcomes. A mention never compels a response. Hard policy may veto unauthorized actions but has no mechanism that can require one.
- **Invoked only where the engine genuinely needs judgment**: dream-cycle merge/prune approval, keystone promotion, ghost-echo acceptance, recall arbitration when gates are ambiguous, planner query rewrites. Everything else stays mechanical.
- **Presented through the Abhidharma model of mind.** Each invocation frames engine state in citta-vīthi terms: which stage this operation is (āvajjana adverting / javana impulsion / tadālambana registration), vedanā (the memory's emotion fields), sati (fidelity), and the resonance-gate readout. `pali.rs` supplies the terminology — the model of mind is the *presentation layer* to the agent, not a pantheon.
- **Gates keep functional names** in code — Fidelity, Lifecycle, Temporal, Capacity, Load — while the corresponding Wisdom perspective may comment on their readout. Mechanical gates and subjective counsel remain separate.

---

## 6. Kill list (dedupes resolved)

| Duplicate | Instances today | v2 survivor |
|---|---|---|
| BM25 | lume `bm25.rs` · ferricula `bb25.rs` · shivvr's vendored lume-hybrid | lume's field-aware `Bm25Index` |
| Roaring bitmaps | `roaring` crate (ferricula) · MiniRoaring (lume) | `roaring` crate |
| Tokenizers | lume analyzer chain · ferricula regex tokenizer · HF tokenizers | two, on purpose: lume analyzer (lexical/FST) + HF (embedder). Ferricula's dies |
| Cosine similarity | simsimd f32 (shivvr) · hand-rolled f64 (lume) · core impls | simsimd behind the `ml` feature; portable fallback without it |
| vec2text inversion | shivvr `inverter.rs` · lume `inversion.rs` · ferricula `inversion.rs` | shivvr's ONNX pipeline in `ferricula-semantic`; drift-check logic from ferricula folds in |
| MCP servers | ferricula rmcp · lume JSON-RPC · shivvr SSE-RPC | one rmcp server in `ferricula-server` |
| HTTP clients to shivvr | lume `hybrid.rs` ureq · ferricula `embed.rs` | `Embedder` trait; `RemoteShivvr` impl is the one remaining client |
| Chunkers | shivvr Monte Carlo · lume paragraph/25k splitter | Monte Carlo for memory ingest; lume's section splitter stays for document indexing (different jobs, both kept) |

---

## 7. Phases

Each phase compiles, passes tests, and is independently useful. Copy code **into** ferricula_v2 (sources are never edited — they remain the reference until parity is proven).

**Phase A — Scaffold & vendor-in.** Workspace `Cargo.toml`; copy the module sets per the §2 map into the five crates; mechanical fixes only (paths, `pub use`). Exit: `cargo build --workspace` green; ferricula's 92 tests + lume's tests pass inside v2. **✅ DONE 2026-07-07** — `cargo check --workspace --all-targets` green on host; 164 tests pass (core/cognition/search) + 16 (semantic, `--no-default-features`); per-crate VENDOR_NOTES.md record every deviation, including a byte-parity audit of all vendored files against the source oracles.

**Phase B — Trait seams.** Define `Embedder`, `Chunker`, `Inverter` traits in `ferricula-semantic`. Impls: `LocalOnnx` (ml feature), `RemoteShivvr` (HTTP, for GPU-box deploys), `OpenAi` (fallback). Replace ferricula `embed.rs` and lume `hybrid.rs` HTTP calls with trait calls. Add native `/embed`. Exit: full remember→recall round-trip with zero shivvr processes running.

**Phase C — Dedupe.** Execute the §6 kill list. Delete the vendored lume-hybrid, MiniRoaring, bb25, the three-way inversion split, ferricula's tokenizer. Exit: `cargo tree` shows one impl per concern; index/snapshot formats still load.

**Phase D — Unified recall + unified server.** Build the §4 pipeline; merge the three server surfaces per §5; benchmark blend modes on the Monte Cristo corpus with `lume eval`. Exit: one binary serving HTTP + MCP + REPL; recall quality ≥ the federated system on the eval set.

**Phase E — Migration & parity.** Importers: ferricula `snapshot_v4/v5.bin` + `wal.log` (format kept, so mostly a smoke test) and existing `.lume-index` dirs (format kept). shivvr sessions need **no** migration — they're ephemeral in-memory today, which is exactly the bug. Side-by-side parity harness against the old three-service stack, then the sidecars are retired from the dev loop. Exit: v2 is the daily driver; `lume` and `shivvr` bins still build for standalone use.

**Then**: the remaining v1 roadmap phases proceed on this substrate — typed event/concept/intent nodes, fidelity probe endpoint, DenseAM dream formalization, LoCoMo/LongMemEval/BEAM evals — through the §5a single-agent/council boundary. None of them are blocked by v2; all of them are cheaper on it.

---

## 8. Decisions taken (contest in review, not in code)

1. **Sync core, async edge.** The single-owner mutable engine thread survives; axum + mpsc at the boundary. No async in `ferricula-core`.
2. **GTR-T5 768-d stays default.** Dimension is config; model modernization is a later impl-swap (roadmap Phase 4), not a v2 blocker.
3. **Formats frozen for v2.0.** postcard v5 envelope + WAL, lume JSON indexes. Nobody re-indexes or re-embeds to adopt v2. Format changes come later with explicit migrations.
4. **Brute-force cosine over bitmap-filtered candidates stays.** Revisit only when ingestion latency actually hurts (roadmap decision #3).
5. **Standalone `lume` and `shivvr` bins are kept** as thin wrappers — external users (nuts.services, GPU embed hosts) keep their contracts.
6. **`ml` is a cargo feature.** Core + search + server build without ort/ONNX; a thin deploy uses `RemoteShivvr`.
7. **ferricula-arena / steve.py dashboards are out of scope** for v2 core. The `/diagnostic` contract is preserved so they keep working; the unified dashboard is the roadmap's §5, later.
8. **License: Gnosis AI-Sovereign** (ferricula's license) for the whole workspace, `LICENSE.md` at root. lume-derived code (BSD-3-Clause) gets attribution in `THIRD_PARTY_NOTICES.md`.
9. **Wisdom Kings are perspectives, not agents.** One sovereign internal agent is presented through the Abhidharma model of mind. The five inherited roles and their activation tiers cross into `wisdom.rs` only as bounded, auditable counsel (§5a).

## 9. Risks

- **ort pinning** (`=2.0.0-rc.11`) — release candidate, breaking changes likely; the trait seam contains the blast radius but the pin must be tracked.
- **ONNX model logistics** — GTR-T5 + the three vec2text models (~1.1 GB) must be present under `/models`; v2 needs a `ferricula models fetch` command instead of Dockerfile-only downloads.
- **Licensing** — resolved (decision #8): workspace ships Gnosis AI-Sovereign with BSD-3 attribution for lume-derived code in `THIRD_PARTY_NOTICES.md`. Keep the notice file honest as code moves between crates.
- **Behavioral drift during dedupe** — bb25 vs bm25 and MiniRoaring vs roaring may differ subtly; the Phase E parity harness is the tripwire, and the untouched source repos remain the oracle.
- **Windows dev / Linux deploy** — ort CPU works on both, but CUDA-on-Windows is flaky (shivvr already documents kernel-validation fallback). Keep CPU the default everywhere. **Confirmed during Phase A**: linking `ml` on Windows hits LNK2038 — prebuilt onnxruntime uses the /MD dynamic CRT while tokenizers' esaxx-rs compiles /MT static; shivvr only ever linked inside Linux containers. Until CRT flags are aligned, `ml`-linked tests are Linux-only; Windows runs `--no-default-features` (16 tests green there, 164 green across the other crates).

---

*Scans in `scan/` were produced by the Antigravity agent in Hyperia pane "Varying Viper 🪗" (read-only survey; instructed to touch nothing outside ferricula_v2). Plan authored by Claude. Sources in `lume/`, `ferricula/`, and `nuts.services/shivvr/` were read but not modified.*
