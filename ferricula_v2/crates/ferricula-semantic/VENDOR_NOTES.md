# Vendor Notes for ferricula-semantic

This crate contains the vendored implementation of embedding, chunking, vec2text inversion, similarity, and cryptographic matrix keys from `shivvr`.

## Source of Code
- Original codebase: `/workspace/DeepBlueDynamics/nuts.services/shivvr/src/`
- Vendored files:
  - `embedder.rs`
  - `chunker.rs`
  - `inverter.rs`
  - `similarity.rs`
  - `crypto.rs`
  - `openai.rs`
  - `lib.rs` (relevant parts only)

## Deviations and Additions
1. **Omitted Server/Store Files**:
   - `store.rs`, `temp_store.rs`, `api.rs`, `auth.rs`, `agent.rs`, and `main.rs` were omitted from this crate, as session/temporary stores are deleted (ingestion goes straight to core durable storage) and API/Auth interfaces will be unified in the workspace.
2. **Minimal Local `Chunk` Struct**:
   - Rather than porting the entire `store.rs` containing ephemeral `Store` logic, we defined `store::Chunk` inside `src/lib.rs` to satisfy compilation requirements of `chunker.rs` and `inverter.rs` while keeping the crate clean.
3. **Decoupled from `vendor/lume-hybrid`**:
   - References to the `lume-hybrid` vendored directory inside `shivvr` were removed and redirected to the sibling crate `ferricula-search`.
4. **Feature Gating**:
   - Kept the `ml` feature flag (`ort`, `tokenizers`, `simsimd` optional dependencies) default enabled, ensuring `--no-default-features` builds successfully without compiling any ONNX/ML library bindings.

## Mechanical Audit & Restorations
- Date: July 7, 2026.
- Audited all files against the read-only oracle `/workspace/DeepBlueDynamics/nuts.services/shivvr/src/`.
- Action taken: Overwrote all 6 files (`chunker.rs`, `crypto.rs`, `embedder.rs`, `inverter.rs`, `openai.rs`, `similarity.rs`) with byte-for-byte original copies, and confirmed no changes are required to compile these specific files natively.
- Current status: 100% parity with zero unintended changes.

