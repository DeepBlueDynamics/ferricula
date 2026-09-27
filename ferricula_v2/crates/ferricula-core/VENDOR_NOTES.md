# Vendor Notes for ferricula-core

This crate contains the core storage, engine, memory, persistent engine, prime tree, and transform systems vendored from the original `ferricula` project.

*Credit to Planned Primate for completing the initial copy of these core modules.*

## Source of Code
- Original codebase: `/workspace/DeepBlueDynamics/ferricula/src/`
- Vendored files:
  - `engine.rs`
  - `graph.rs`
  - `memory.rs`
  - `model.rs`
  - `persist.rs`
  - `prime_tree.rs`
  - `skg.rs`
  - `sparse.rs`
  - `transform.rs`
  - `lib.rs` (modified wiring)

## Deviations from Oracle
1. **Removed SQL execution**:
   - `execute_sql` and `execute_sql_with_embed` have been removed from `engine.rs` and `persist.rs` to decouple the core storage from the SQL query parsing and execution layer (which belongs to the server).
2. **ResonanceGate Renaming & Wisdom-King Doc Stripping**:
   - Stripped the doc-comments in `memory.rs` referencing Wisdom-King personas (Acala, Yamāntaka, Trailokyavijaya, Vajrayakṣa, Kuṇḍali).
   - Renamed `ResonanceGate::Craft` to `ResonanceGate::Load`.
