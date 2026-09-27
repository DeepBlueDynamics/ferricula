# ferricula-search ↔ memory-system integration contract

Retrieval & Ingest lane (n8-olive-lynx). Scope: `crates/ferricula-search/**`,
`crates/ferricula-semantic/**`. This document is the API contract for the
Lume × Ollaya × Abhidhamma plan milestone M5 (§8 wiring) plus the fleet's
multilingual directive; the server seam is a **proposal to the server owner
(Eldest Dog, n8-quiet-shrew)** — this lane does not edit `ferricula-server`.

## 1. ExactID and exact text (source identity)

| Item | Contract |
|---|---|
| Section identity | `hybrid::section_hash(sec)` — FNV-1a 64 over `(filename, title, body)`, hex-16. Line-number independent (moving a section never changes its identity; verified by `hybrid.rs` test `section_hash_is_stable_and_content_sensitive`). |
| Exact text | `Section.body` is verbatim source text (CRLF normalized to LF by `lines()`; nothing else). `HybridHitDetails.body` carries it out byte-identical, including CJK and Pāli diacritics. Split sections are re-joined with their adjacent parts before output. |
| Provenance | `HybridHitDetails { section_index, title, filename: Option<String>, line_number, body }` + `section_hash` (via `lume section list`) is the complete pointer set the plan §3.2 interface requires: `(lume_doc_id, section_id)` ≙ `(filename, section_hash)`. |
| Ingest threshold | `parse_markdown` retains only sections with `body.trim().len() > 100` — very short sections are never indexed. Server-side adapters must not assume every memory text becomes a section. |
| Tokenization is lossy, retrieval is not | BM25/FST consume folded tokens (`saṅkhāra`→`sankhara`, CJK unigram+bigram), but only ever for *matching*; what is returned is the untouched `body`. |

## 2. Recall path (plan §8.1)

```text
query
  → (query expansion, §5 — versioned, original retained)
  → lume hybrid search (BM25 + semantic + SKG blend, hybrid.rs::execute_hybrid_search)
  → (suppression filter, §4 — forgotten never; sealed only with opt-in)
  → [SERVER SEAM: filter agent_id + lifecycle state — plan §3.3 invariant:
      NO plaintext reaches an Ollaya gate before this filter]
  → sati-recall gate on top-N (rerank.rs, N ≈ 20)
  → rank by answers_query, relevance, hybrid_score  (× ojā applied server-side)
  → HybridHitDetails out; sati_gaṇanā++ / α recompute are memory-system effects
```

CLI surface: `lume search [--rerank] [--include-sealed] <query>`; the
MCP/agent serve path (`agent.rs lume_search`) inherits the behavior via
`LUME_SATI_RERANK=1` / `LUME_INCLUDE_SEALED=1` without schema changes.

## 3. Sati-recall gate hook (`src/rerank.rs`)

Public API:

```rust
pub struct RerankConfig { enabled, host, gate, top_n, timeout, max_chars, keep_alive }
impl RerankConfig { pub fn from_env(enabled: bool) -> Self }   // OLLAYA_HOST, LUME_SATI_*
pub enum AbstainReason { NoModel, StateTruncated, LowConfidence, ProviderError(String) }
pub struct SatiVerdict { answers_query: f32, relevance: f32 /*0..3*/,
                          abstain_reason: Option<AbstainReason>, state_truncated: bool,
                          latency_us: u128, gate: String, model: Option<String> }
pub struct RerankOutcome { verdicts: Vec<SatiVerdict> /* input-aligned */,
                           order: Vec<usize> /* permutation */, stats: RerankStats }
pub fn rerank_hits(query: &str, hits: &[HybridHitDetails], cfg: &RerankConfig) -> RerankOutcome
```

Semantics (aligned with `ferricula-cognition` API_PROPOSAL §2.4):

- One request per candidate: `POST {OLLAYA_HOST}/api/decide` with
  `{"model": gate, "state": {"query": <original>, "memory": <exact body>},
  "keep_alive": "30m"}` (plan §4.4/§8.4, Appendix A response shape).
- **Abstain keeps retrieval order** — no fabricated scores; gate down ⇒
  identity permutation with `NoModel`/`ProviderError` reasons (search never
  fails because of the gate).
- `state_truncated` (512-token window, plan §3.4): local head-truncation at
  `LUME_SATI_MAX_CHARS` (default 1800) on a char boundary, or a server-side
  truncation flag ⇒ low-trust ⇒ abstain from reordering, counted in stats.
- Ordering inside the CLI: `answers_query` desc, `relevance` desc,
  `hybrid_score` desc, original rank asc. The plan's `× ojā` multiplier is a
  memory-system signal: the server applies it when building `RawMemory`
  (ojā is not known to lume).
- `eval --rerank` (plan §10.1) runs the gate per question's top-N and reports
  judged/abstained/truncated + p50/p95 latency so every gate claim stays
  tied to actual runs.
- Mapping to cognition: `SatiVerdict { answers_query, relevance }` ≙
  `SatiRecallVerdict`; abstain reasons ≙ `AbstainReason` subset;
  `RerankStats` ≙ `GateProvenance` (latency, truncation, gate version).

## 4. Section suppression — upekkhā / nirodha / sealing (`src/suppress.rs`)

- Ledger `suppression.json` next to the index db (anchored like the session
  caches via `hybrid::set_cache_dir`); keys are section hashes, so
  suppression survives re-index and incremental semantic ingest.
- `forget` (upekkhā/nirodha): `is_searchable == false` always — BM25, the
  semantic blend, SKG expansion and **eval** all exclude it (plan §9 rule 1,
  §10.3 invariant: released text must not resurface anywhere).
- `seal`: excluded from default search; `--include-sealed` / 
  `LUME_INCLUDE_SEALED=1` opts back in (plan §8.3 "sealed index, not
  searchable by default"). Sealing a forgotten section is refused.
- `restore` (punabbhava): clears either kind.
- CLI: `lume section list | forget <hash>... | seal <hash>... | restore <hash>...`
  (`list` prints the ExactIDs; unknown hashes are rejected before mutation).
- Remote semantic chunks of suppressed sections are filtered at blend time
  by hash; the shivvr session itself is untouched (re-ingest happens on the
  next corpus change anyway). Sealed ⇒ `RawMemory.sealed = true` server-side.

## 5. Versioned query expansion (`src/query_expand.rs`)

- Query-side **only**: section text, hashes and stored vectors are never
  modified — expansion cannot change ingest identity.
- Matching is **whole-word** (whitespace/punctuation-delimited), mirroring
  cognition's `expand()`; glossary entries must be complete words. CJK
  compound words are matched as written — no substring extraction — so a
  zh↔en glossary lists whole terms ("苹果电脑" as one entry). Determinism
  over recall: substring matching would fire on every containing sentence.
- Canonical glossary: `crates/ferricula-cognition/src/pali.rs` (owner:
  cognition lane, n8-olive-crow). Snapshot copied 1:1 with identical
  `build_maps` semantics; version string
  `pali-snapshot-2026-09-26(cognition/src/pali.rs)`. **Drift contract:** a
  glossary change in cognition requires bumping `SNAPSHOT_VERSION` here (or
  shipping the JSON below); this is the one duplicated datum and it is
  recorded, not silent.
- Runtime override: `LUME_QUERY_GLOSSARY=<path>` accepts cognition's
  `glossary_json()` shape (`{"glossary":[{"pali","meaning","code_terms"}]}`)
  or a bare array; parse failure falls back to the snapshot (logged).
- Output: `QueryExpansion { original, expanded, terms_added, version }` —
  expansion appended after `" | "` (same layout cognition uses so embeddings
  see both vocabularies without corrupting the original).
- The sati gate and the result's `query` always see the **original** query.

## 6. Multilingual parity (zh / Pāli / en)

Two distinct capabilities (fleet finding, verified against
`lib.rs::tokenize`):

- **Z1 — same-language segmentation (implemented here):** CJK runs
  (Han/Hiragana/Katakana/Hangul) emit unigram + overlapping bigram tokens
  with correct byte offsets, through the single `tokenize()` used by index
  and query alike. Chinese corpus + Chinese query now works lexically
  (previously: zero tokens, BM25 blind). Pāli diacritic words emit BOTH the
  ASCII-folded form (`sankhara`) and the original form (`saṅkhāra`, case
  -folded only) so the two scripts are mutually retrievable while the
  original stays precisely searchable.
- **Z2 — cross-language retrieval (proposal only, blocked by design):** the
  hybrid semantic leg is served entirely by shivvr sessions (GTR-T5 768d,
  English-trained; the 768-d assertion lives in `embed_text`, the inversion
  debug path — the search path posts to shivvr). A multilingual embedder
  (e.g. multilingual-e5-small / paraphrase-multilingual-MiniLM, 384d) is the
  plan §3.1 one-space decision (A/B/C) owned by Eldest Dog / the human, and
  is additionally constrained by the Shivvr new-model planning-only
  restriction. No dimension-mixing is performed or assumed (no GTR↔SigLIP
  cross-space comparisons). Until decided, zh↔en bridging is the versioned
  query-translation layer of §5 fed by an external glossary.
- Benchmark lane interface: paired zh/en + pali fixtures (Technical Viper,
  n8-proud-kiwi, `audit/2026-09-26/benchmarks/fixtures/`) run against this
  crate via `lume eval`; lexical metrics are measurable now; any embedder or
  latency claim must cite an actual run.

## 7. Server seam proposal (for Eldest Dog; no server edits from this lane)

Current state (verified `ferricula-server/src/memory.rs:55-107`):
`recall_candidates` is provider-free lexical-over-tags; it already excludes
Forgiven/Archived **before** ranking (§3.3-compatible) and lowercases
Unicode.

Proposed seam (server-owned wiring, any form the owner chooses):

1. Server keeps ownership of the agent/state filter and applies it to
   candidates **before** any sati gate call (invariant §3.3; test §10.3).
2. Candidates come from this crate as `HybridHitDetails` (exact body +
   provenance); the server maps them to cognition's
   `RawMemory { id, agent, text, oja, state, vedana, pool, kind, sealed,
   keystone, retrieval_score, sati_recall }`: `text = body`,
   `retrieval_score = hybrid_score`, `sati_recall = SatiVerdict` (None on
   abstain), `sealed` from the suppression ledger state it owns.
3. `sati_gaṇanā++` and α recompute for above-threshold hits are memory-
   system effects taken from the gate verdicts, logged to the karmic sink
   with gate version and probabilities (plan §9 rule 6).
4. `lume serve` (agent.rs) already exposes `lume_search`; a thin
   `RawMemory`-shaped tool response can be added on request.

## 8. Coordination register

| Party | Item |
|---|---|
| Eldest Dog (server, release) | §7 seam decision; §3.1 embedding-space (M0); upstream publication of ferricula-search changes |
| Wren (engine/host) | serialized host build + fresh default-feature test runs of this crate; §7 persistence implications of the ledger |
| Burning Dingo (cognition) | glossary drift sync (§5); RawMemory mapping confirm (§7.2) |
| Technical Viper (benchmarks) | zh/pali fixture formats vs §5/§6 interface; lexical parity runs once Z1 lands |
| Splendid Angelfish (gates) | Ollaya `sati-recall` gate live at `OLLAYA_HOST`; CALIBRATION version surfaced in the decide response for `gate_version` provenance |

## 9. Evidence (2026-09-26, container cargo 1.98.1)

| Run | Result |
|---|---|
| `cargo test -p ferricula-search` (default features) | **67 passed / 0 failed** — incl. 5 sati-gate tests over a real loopback mock gate (TCP round-trips: reorder, abstain-keeps-order, truncated-abstains, beyond-top-N), 6 suppression-ledger tests, 7 query-expansion tests, 5 CJK/Pāli tokenizer tests, and the 52 pre-existing lib tests (synonym collapse, entity folding, spelling all still green) |
| `cargo test -p ferricula-semantic --no-default-features` | **16 passed / 0 failed** |
| `cargo test -p ferricula-semantic` (default features, `ml`) | link failure in this container: `rust-lld: undefined symbol: __isoc23_strtol` from `ort_sys` (needs glibc ≥ 2.38) — default-feature scope requires the serialized host build (Wren) |

CLI end-to-end smoke (trilingual corpus zh/en/pāli, throwaway, removed after
the run): Chinese query “苹果电脑” → BM25 hit #1 score 20.31, body returned
byte-identical; `anicca decay` → expansion line with snapshot version,
English Pāli-glossary section hit; `section seal` → default search excluded
the section, `--include-sealed` restored it, `restore` cleared the ledger;
`--rerank` with the gate unreachable → "judged 0/2, abstained 2" printed,
search succeeded with unchanged order; `eval` on 3 questions (zh→zh,
pāli→en, en→en): Hit@10 100%, MRR 1.0, nDCG@10 1.0; `eval --rerank` gate
track reported judged 0 / abstained 6 with p50/p95 latencies and deduped
provider errors. Steve instance reachable from the container at
`host.docker.internal:18875` (`GET /health` → 200 `{"ok":true}`).