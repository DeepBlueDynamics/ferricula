# Vendor Notes for ferricula-cognition

This crate contains the casting, dream, identity, pali, planner, and clock systems vendored from the original `ferricula` project.

*Credit to Planned Primate for completing the initial copy of these cognition modules.*

## Source of Code
- Original codebase: `/workspace/DeepBlueDynamics/ferricula/src/`
- Vendored files:
  - `casting.rs`
  - `clock.rs`
  - `dream.rs`
  - `identity.rs`
  - `pali.rs`
  - `planner.rs`
  - `lib.rs` (modified wiring)

## New Additions
1. **Added `abhidharma.rs`**:
   - Created per spec to define the functional internal agent interface replacing the Wisdom-King archetype layer.
   - Declares `AbhidharmaContext`, `CittaVithi` stages, `Decision` enum, and `InternalAgent` trait.
   - Implements `NoAgent` with deterministic defaults (Approve if Fidelity gate passes, Defer otherwise) and includes unit tests.
   - Implements `LlmAgent` stub wrapping `Planner` to be wired to Claude in a future phase.

## Deviations from Oracle
1. **Did NOT Copy `archetypes.rs`**:
   - Left behind the persona-based archetypes module entirely.
2. **Identity (`identity.rs`)**:
   - Removed `archetypes` field from `IdentityState` and removed its casting/initialization logic in `load_or_create`.
   - Inlined `public_from_private` helper function using `x25519_dalek` directly.
   - Pointed `VectorTransform` references to `ferricula_core::transform::VectorTransform`.
   - Updated `active_resonance_gates` to return a deterministic default list of all 5 gates: `Fidelity`, `Lifecycle`, `Temporal`, `AgentCapacity`, and `Load` (representing `Craft`).
3. **Dream (`dream.rs`)**:
   - Replaced imports of `archetypes` with `activation_roles(intensity)` classification returning active role strings deterministically (intensity `< 0.25` -> none; `0.25..0.75` -> `["Intuition", "Fortune"]`; `>= 0.75` -> all five roles).
   - Stubbed out `extract_ghost_echo` as a `None` return since the HTTP client `inversion` module is not vendored (to be replaced with direct trait-based in-process calls to `ferricula-semantic`'s `Inverter` in a future phase).
4. **Added Dependencies**:
   - Declared `rustls = { version = "0.23", features = ["ring"] }` and `webpki-roots = "0.26"` in `Cargo.toml` to satisfy compile requirements of the hand-rolled TLS client in `planner.rs` / `clock.rs`.

