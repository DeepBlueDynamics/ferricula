# Vendor Notes for ferricula-search

This crate contains the vendored implementation of document search, spelling, semantic mesh, and crawling from `lume`.

## Source of Code
- Original codebase: `/workspace/DeepBlueDynamics/lume/src/`
- Vendored files:
  - `agent.rs`
  - `answer.rs`
  - `bm25.rs`
  - `crawl.rs`
  - `eval.rs`
  - `fast_retrieval.rs`
  - `graph_search.rs`
  - `hybrid.rs`
  - `inversion.rs`
  - `lib.rs`
  - `main.rs`
  - `regex.rs`
  - `semantic_mesh.rs`
  - `spelling.rs`
  - `stream.rs`

## Deviations and Additions
1. **Added `fusion.rs`**:
   - Extracted the Reciprocal Rank Fusion (RRF) and FST intent-boost scoring mathematics verbatim from `temp_store.rs` in `shivvr`.
   - Decoupled the scoring logic from the specific `Chunk` store types to run standalone over ranked ID lists and text mappings.
   - Added `test_rrf_fusion_and_boosting` to verify correct RRF blending and FST intent boosting behavior.
2. **Module Declarations**:
   - Added `pub mod fusion;` inside `lib.rs`.
3. **Crate References**:
   - Upstream imports pointing to `lume-hybrid` or `lume_hybrid` have been removed or redirected to reference `ferricula-search` itself.
4. **Format string braces escaping in `main.rs`**:
   - Escaped JSON braces in help texts inside format strings at line 326 (`{{question, answer}}`) and lines 1916–1918 (`{{type:"frame", ...}}`) to prevent Rust compilation errors.
5. **Crate alias in `main.rs`**:
   - Added `extern crate ferricula_search as lume;` to `main.rs` and restored the `cands` type reference at line 1799 back to `lume::stream::Candidate` (the original form) while allowing correct compilation within the `ferricula-search` package.

## Mechanical Audit & Restorations
- Date: July 7, 2026.
- Audited all files against the read-only oracle `/workspace/DeepBlueDynamics/lume/src/`.
- Found unintended drift in multiple files (including missing escaped braces in `main.rs` format strings and minor formatting changes elsewhere).
- Action taken: Overwrote all 15 files (`agent.rs`, `answer.rs`, `bm25.rs`, `crawl.rs`, `eval.rs`, `fast_retrieval.rs`, `graph_search.rs`, `hybrid.rs`, `inversion.rs`, `lib.rs`, `main.rs`, `regex.rs`, `semantic_mesh.rs`, `spelling.rs`, `stream.rs`) with byte-for-byte original copies, and re-applied only the target import/alias/module configurations on top.
- Current status: 100% parity with zero unintended changes.


