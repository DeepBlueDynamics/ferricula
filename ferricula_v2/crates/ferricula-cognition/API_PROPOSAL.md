# ferricula-cognition — public API proposal (Cognition & Curator lane)

Author: Burning Dingo 🥓 (nemesis8/n8-olive-crow) · 2026-09-26 · Status: proposal, pre-wiring
Owner of `crates/ferricula-cognition/**` per Coordinator (Eldest Dog 🚀, msg 21:42Z).

## 0. Findings from direct code read (verified)

| # | Finding | Source |
|---|---|---|
| F1 | `dream_cycle` Phase 2 moves every Active record below `FIDELITY_GATE` to Forgiven automatically | `dream.rs:194-203` |
| F2 | Phase 6 archives Forgiven records older than 1h, then **deletes** Archived records with fidelity < ε (`store.remove`, `graph.remove_node`) | `dream.rs:265-314` |
| F3 | `consolidate_group` keeps one survivor, `graph.remove_node` on every absorbed member, then forgive+archive them; no index record | `dream.rs:416-490` |
| F4 | Dream mutates `MemoryStore`/graph directly; nothing writes the WAL; `dream_cycle` is not called by the server | audit; `runtime.rs` has no caller |
| F5 | No Curator, no sati monitor, no papañca detector, no reconstruction overlay, no task_succeeded/pool concept anywhere in crates | grep, audit |
| F6 | Server recall = lexical over tags, non-mutating; text arrives in `MemoryHit.tags["text"]`; `on_recall` / `resonates()` never called in production | `server/src/memory.rs:55-102`, `core/memory.rs:145,208` |
| F7 | `ferricula-episode/src/causal.rs:254 decay_may_erase_text() -> false` already encodes Addendum §A.1.1; dream.rs contradicts it | causal.rs |

Spec anchors: Addendum A §A.1.1 (decay = priority; consolidation = index; upekkhā/nirodha only text loss), §A.2 (curator, k=3, ephemeral briefing), §A.3 (success pool / failures index, task_succeeded strict), §A.5.3 (five drift components + monitors), §A.6 Rules 7–10, §A.7 CI invariants.

## 1. Module layout (all new files are additive; existing modules untouched except `dream.rs` and `lib.rs`)

```
src/
  lib.rs           re-exports below
  bhavana.rs       NEW  source-preserving nightly cycle (replaces dream_cycle semantics)
  curator.rs       NEW  read-time Curator: candidate selection, JitMem prompt, extractive fallback, Briefing
  sati.rs          NEW  mindfulness monitors + papañca detector + return-to-object + dial presets
  gates.rs         NEW  Gate trait + verdict types (contract for Ollaya gates; heuristic impls only)
  outcome.rs       NEW  task_succeeded rule, Pool assignment, self-judgment gap
  karmic.rs        NEW  KarmicEntry / KarmicSink (append-only event contract; versions on every entry)
  dream.rs         KEPT as thin wrapper: `dream_cycle` == `bhavana_cycle` with default policy; Phase 6 prune and auto-forgive removed
```

## 2. Types and traits

### 2.1 bhavana.rs — consolidation as index, decay as priority

```rust
pub struct BhavanaPolicy {
    pub consolidation_threshold: f32,   // 0.85 (parity with dream.rs)
    pub min_cluster: usize,             // 2 today; plan §2 says n ≥ 3 for the merge gate — configurable
    pub neglect_seconds: u64,           // 86_400
    pub contradiction_review_tau: f32,  // clusters with contradiction ≥ τ go to review, never merge (plan §4.3)
    pub release: ReleaseMode,           // ProposeOnly (default). No variant auto-deletes.
}
pub enum ReleaseMode { ProposeOnly }

/// A cluster node that POINTS at raw members. Members keep text, state and vectors.
pub struct ClusterIndex {
    pub cluster_id: u64,                // stable hash of sorted member ids + operator_version
    pub members: Vec<u32>,
    pub weights: Vec<f32>,              // ojā (fidelity) per member; same_truth p if a merge gate ran
    pub centroid: Vec<f32>,             // weighted mean of member vectors (nimitta space), for retrieval only
    pub same_truth: Option<f32>,        // from sankhara-merge gate; None on Abstain
    pub contradiction: Option<f32>,
    pub operator_version: &'static str, // "bhavana/2.0.0-alpha.0"
    pub created_at: u64,
}

pub struct ReleaseProposal { pub id: u32, pub kind: ReleaseKind, pub reason: ReleaseReason }
pub enum ReleaseKind { Upekkha /* Active→Forgiven */, Nirodha /* Forgiven→Archived */ }
pub enum ReleaseReason { BelowGate { fidelity: f32 }, ForgivenStale { seconds: u64 } }
pub struct ReleaseDecision { pub id: u32, pub kind: ReleaseKind, pub approved: bool, pub decided_by: String }

pub struct BhavanaReport {  // superset of DreamReport; DreamReport fields kept for parity
    pub decayed_ids: Vec<u32>, pub halo_touched: u32, pub edges_created: u32,
    pub clusters: Vec<ClusterIndex>,           // NEW: index built, no member removed
    pub review_clusters: Vec<ClusterIndex>,    // NEW: contradiction ≥ τ, routed to review
    pub release_proposals: Vec<ReleaseProposal>, // NEW: replaces forgiven/archived/pruned counts
    pub karmic: Vec<KarmicEntry>,
    // legacy counters retained: ticks, decayed, consolidated, keystones_reviewed, keystones_promoted, skg_summary, dream_imagery_candidates
    // legacy `forgiven`, `archived`, `pruned` are always 0 and marked #[deprecated]
}

pub fn bhavana_cycle(
    store: &mut MemoryStore, engine: &Engine, graph: &mut MemoryGraph,
    skg: &mut SkgState, prime_tree: &PrimeTree,
    policy: &BhavanaPolicy, merge_gate: Option<&dyn MergeGate>,
    intensity: f32, entropy_seed: &[u8],
) -> BhavanaReport;

/// The only path that changes lifecycle state. Cognition never touches Row text or vectors;
/// text removal on upekkhā/nirodha is Engine's (Wren) commit, keyed by the returned ids.
pub fn apply_release(store: &mut MemoryStore, decisions: &[ReleaseDecision]) -> Vec<KarmicEntry>;
```

Semantics vs today: Phase 1 decay unchanged (fidelity = retrieval priority). Phase 2 → `release_proposals` (state untouched). Phase 3 → `ClusterIndex` + `consolidation_depth += 1` on every member (α falls: "becoming structure", `effective_alpha` already does this) + `Provenance` untouched + member↔member `Semantic` edge `"co-member:<cluster_id>"`; no `remove_node`. Phase 6 → gone. Invariants tested: `store.len()` and `graph.node_count()` never decrease inside a cycle; every `Row` text tag is byte-identical before/after.

### 2.2 curator.rs — read-time curation (JitMem, §A.2)

```rust
pub enum Pool { Success, Failure, Unclassified /* legacy facts; recalled by default */ }
pub enum MemoryKind { Raw, Reconstruction { parent_id: u32, curator_version: String, task: String } }
pub struct RawMemory {
    pub id: u32, pub text: String, pub oja: f32, pub state: LifecycleState,
    pub vedana: Option<Vedana>, pub pool: Pool, pub kind: MemoryKind,
    pub sealed: bool, pub retrieval_score: f32, pub sati_recall: Option<SatiRecallVerdict>,
}
pub struct CurationRequest { pub task: String, pub cue: RecallCue, pub candidates: Vec<RawMemory>, pub k: usize /* 3 */, pub include_failures: bool /* false */ }

pub struct Selection { pub chosen: Vec<RawMemory>, pub excluded: Vec<(u32, ExcludeReason)> }
pub enum ExcludeReason { FailurePool, Sealed, Archived, OverlayWeightFloor, BeyondK }
pub fn select_candidates(req: &CurationRequest, dial: &Dial) -> Selection;   // pure, deterministic

/// Ephemeral by construction: NOT Serialize, no Clone into storage types (Rule 7).
pub struct Briefing { pub task: String, pub used: Vec<BriefingUse>, pub guidance: String, pub curator_version: String }
pub struct BriefingUse { pub id: u32, pub why: String, pub is_reconstruction: bool }
impl Briefing { pub fn render(&self) -> String; pub fn receipt(&self) -> BriefingReceipt; }
/// Log-safe: ids, hash, versions, token estimate. No text.
#[derive(Serialize)] pub struct BriefingReceipt { pub memory_ids: Vec<u32>, pub text_sha256: [u8; 32], pub curator_version: String, pub task_sha256: [u8; 32], pub tokens_estimate: u32 }

pub trait Curator { fn curate(&self, req: &CurationRequest, sel: &Selection) -> Result<Briefing>; }
pub struct ExtractiveCurator;            // no model: relevance-ordered excerpts, specifics redacted, reconstructions labelled
pub fn build_prompt(req: &CurationRequest, sel: &Selection) -> GenerativeRequest;   // JitMem Appendix A text, verbatim from Addendum §A.2.3
pub fn parse_generative_response(resp: &GenerativeResponse, req: &CurationRequest, sel: &Selection) -> Result<Briefing>;
```
The generative call (Ollama/vLLM, Qwen3-8B per §A.2.2) is made by the server; cognition builds the prompt and parses the reply, so the crate stays provider-neutral and testable.

### 2.3 sati.rs — monitors, papañca, return-to-object (§A.5.3, Rule 10)

```rust
pub enum Dial { MindModel, Service }
pub struct SatiConfig {
    pub enabled: bool,          // true. `SatiConfig::research_unmonitored()` is the ONLY way to get false; S11 only.
    pub dial: Dial,
    pub lambda_cetana: f32,     // MindModel 0.3–0.6, Service 0.0–0.1 (§A.5.4 starting points)
    pub tau_skew: f32, pub n_consecutive: usize,
    pub d_max: u32,             // MindModel 5–8, Service 2
    pub rho_max: f32, pub window: usize,
}
pub enum CueSource { ExternalInput, Task, SelfOutput, Dream }
pub struct RecallCue { pub source: CueSource, pub query_sha256: [u8; 32], pub ts: u64 }
pub enum Valence { Sukha, Dukkha, Neutral }
pub struct RecallObservation { pub cue: RecallCue, pub retrieved: Vec<Valence>, pub store_hist: [u32; 3] }

pub enum NotingEvent {
    ValenceSkew { skew_kl: f32, retrieved_dukkha: f32, store_dukkha: f32, streak: usize, action: SatiAction },
    Papanca { chain_depth: u32, self_ref_ratio: f32, action: SatiAction },
    SelfJudgmentGap { self_verdict: f32, evidence_verdict: f32, running_mean_gap: f32 },
}
pub enum SatiAction { None /* enabled=false or below bound */, DampCetana { from: f32, to: f32 }, ReturnToObject }
pub enum RecallAdmission { Admit, Refuse(ReturnToObject) }

pub struct SatiMonitor { /* streaks, window ring, pending_return, lambda */ }
impl SatiMonitor {
    pub fn new(cfg: SatiConfig) -> Self;
    pub fn admit(&mut self, cue: &RecallCue) -> RecallAdmission;         // call BEFORE recall
    pub fn observe(&mut self, obs: &RecallObservation) -> Vec<NotingEvent>; // call AFTER recall; events go to KarmicSink
    pub fn lambda_cetana(&self) -> f32;                                    // current (possibly damped) mood bias
    pub fn snapshot(&self) -> SatiSnapshot;                                // Serialize: metrics for dashboards / benchmarks §6.9
}
pub fn valence_skew_kl(retrieved: &[Valence], store_hist: [u32; 3]) -> f32;
```
With `enabled=false` every measurement and event is still emitted (Dukkha "fully understood"), `action` is always `None`, and every `KarmicEntry` carries `sati_enabled=false`. Normal runtime cannot construct that config (Eldest Dog: "normal runtime no monitor-off").

### 2.4 gates.rs — decision-gate contract (Angelfish implements; cognition consumes). Decision ≠ generation.

```rust
/// Every decision gate returns a typed answer OR a typed abstention, never a fake judgment.
pub enum Verdict<T> { Answer(T), Abstain { reason: AbstainReason } }
pub enum AbstainReason { NoModel, StateTruncated, LowConfidence { p_max: f32 }, ProviderError(String), NotApplicable }
pub struct GateProvenance { pub gate: &'static str, pub gate_version: String, pub calibration_sha256: Option<[u8; 32]>, pub features_sha256: [u8; 32], pub latency_us: u64, pub state_truncated: bool, pub cost: Option<UsageCost> }
pub struct UsageCost { pub input_tokens: u32, pub output_tokens: u32, pub usd: Option<f64>, pub profile: String /* local | api:<name> */ }
pub struct Judged<T> { pub verdict: Verdict<T>, pub provenance: GateProvenance }

pub struct SatiRecallVerdict   { pub answers_query: f32, pub relevance: f32 /* expected level 0..3 */ }
pub struct MergeVerdict        { pub same_truth: f32, pub contradiction: f32 }
pub struct TaskSucceededVerdict{ pub p: f32 }
pub struct VedanaVerdict       { pub valence: Valence, pub intensity: f32 /* 0..4 */, pub p: f32 }

pub trait SatiRecallGate    { fn judge(&self, query: &str, memory: &str) -> Judged<SatiRecallVerdict>; }
pub trait MergeGate         { fn judge(&self, members: &[&str]) -> Judged<MergeVerdict>; }
pub trait TaskSucceededGate { fn judge(&self, transcript: &str, evidence: &[Evidence]) -> Judged<TaskSucceededVerdict>; }
pub trait VedanaGate        { fn judge(&self, text: &str) -> Judged<VedanaVerdict>; }

pub struct NoModelGate;            // implements all four; ALWAYS returns Verdict::Abstain { NoModel }. Never an echo.
pub struct CosineOnlyMergeGate;    // heuristic: same_truth = mean pairwise cosine; contradiction → Abstain{NotApplicable} (cosine cannot see contradiction)
```
Consumers handle `Abstain` explicitly: bhavana keeps a cluster out of merge on Abstain (routes to review); curator keeps retrieval order on Abstain; outcome assigns `Pool::Failure` on Abstain (strict rule).

Generative side (curator) is a separate request/response pair, no verdict type:
```rust
pub struct GenerativeRequest  { pub system: String, pub user: String, pub max_output_tokens: u32, pub temperature: f32, pub purpose: &'static str /* "curator" */ }
pub struct GenerativeResponse { pub text: String, pub model: String, pub cost: Option<UsageCost>, pub truncated: bool }
// server harness adapter (Eldest Dog) maps GenerativeRequest → ModelRouter.complete_with_budget → GenerativeResponse
// cognition: build_prompt(..) -> GenerativeRequest ; parse_generative_response(&GenerativeResponse, ..) -> Briefing
```

### 2.5 outcome.rs — task_succeeded rule, pools, self-judgment gap (§A.3, §A.5.3(4))

```rust
pub enum Evidence { ToolResult { ok: bool, summary: String }, StateChange { summary: String }, TestPass { name: String }, UserAcceptance }
pub struct TaskOutcome { pub task_id: String, pub self_verdict: Option<bool>, pub evidence: Vec<Evidence>, pub gate: Option<GateVerdict<TaskSucceededVerdict>>, pub ground_truth: Option<bool> }
/// Strict: success only with external evidence AND gate p ≥ τ; ambiguous/partial → Failure. No evidence → Failure (never Unclassified: that is reserved for legacy non-task memories).
pub fn assign_pool(outcome: &TaskOutcome, tau: f32) -> Pool;
pub fn self_judgment_gap(outcome: &TaskOutcome) -> Option<f32>;   // self − evidence, when both exist
```

### 2.6 karmic.rs — append-only event contract (Rule 6, Rule 10)

```rust
#[derive(Serialize, Deserialize)] pub struct KarmicEntry { pub ts: u64, pub component: &'static str, pub version: &'static str, pub gate_version: Option<String>, pub sati_enabled: bool, pub event: KarmicEvent }
pub enum KarmicEvent { Noting(NotingEvent), ClusterBuilt { cluster_id: u64, members: Vec<u32> }, ClusterReview { .. }, ReleaseProposed(ReleaseProposal), ReleaseApplied(ReleaseDecision), Briefing(BriefingReceipt), PoolAssigned { task_id: String, pool: Pool }, ReconstructionProposed { parent_id: u32, task_sha256: [u8;32], curator_version: String } }
pub trait KarmicSink { fn append(&mut self, e: KarmicEntry); }
pub struct VecSink(pub Vec<KarmicEntry>);
```
Wren: this is the payload I will hand to the WAL append-only envelope; I don't write the WAL myself.

## 3. Ownership seams (no shared-file edits by me)

| Need | Owner | What I consume / emit |
|---|---|---|
| Persist `ClusterIndex`, apply text removal on approved `ReleaseDecision`, store `Reconstruction` overlay | Engine (Wren) | I emit typed proposals; Engine commits. Until core has a cluster record, `BhavanaReport.clusters` is the carrier. |
| Recall candidates with text + provenance + scores | Retrieval (Penguin) | `RawMemory` built by server from hits; today from `MemoryHit.tags["text"]` |
| Gate implementations + calibration | Gates (Angelfish) | traits in §2.4; heuristic impls ship with cognition so tests need no Ollaya |
| S11 harness, §6.9 drift tests | Benchmarks (Viper) | `SatiConfig::research_unmonitored()`, `SatiSnapshot`, karmic JSONL |
| Server wiring: admit → recall → curate → act → outcome | Coordinator (Eldest Dog) | after this proposal is accepted |

## 4. Regression tests (all inside ferricula-cognition; no Ollaya, no network)

bhavana: (1) below-gate record stays Active, appears in `release_proposals`; (2) archived record with fidelity 0 survives the cycle (`store.len()`, `graph.node_count()` unchanged); (3) two similar rows → one `ClusterIndex` with both members, both still Active with text unchanged, `consolidation_depth` +1 each; (4) contradiction ≥ τ → `review_clusters`, no depth change; (5) `apply_release` with `approved=false` is a no-op and logs; approved Upekkhā → Forgiven only; (6) legacy counters `forgiven/archived/pruned` == 0; (7) `dream_cycle` wrapper equals `bhavana_cycle` defaults.
curator: (8) Failure-pool memory never selected unless `include_failures`; (9) sealed/archived excluded; (10) k=3 cap; (11) reconstruction is labelled in `Briefing.used` and rendered text; (12) `Briefing` does not implement Serialize (compile-time `static_assertions`-style test); (13) receipt carries no text; (14) extractive curator redacts numbers/dates/emails/urls; (15) prompt text matches §A.2.3 rules block.
sati: (16) KL skew 0 for matching distributions; (17) streak ≥ N over τ → `ValenceSkew` with `DampCetana` when enabled, `None` when research-unmonitored; (18) chain of SelfOutput cues > d_max → `Papanca{ReturnToObject}` and next SelfOutput cue is refused, ExternalInput admitted; (19) ratio > ρ_max → same; (20) Service dial d_max=2 vs MindModel; (21) unmonitored config still emits events, never acts; (22) `SatiConfig::default().enabled == true`.
outcome: (23) no evidence → Failure even if self_verdict true; (24) evidence + p ≥ τ → Success; (25) gap = self − evidence.
karmic: (26) every event from (1)–(25) appears in the sink with component+version.

## 5. Open questions for Coordinator

Q1 `min_cluster`: keep 2 (parity with today) or move to 3 (plan §2 "cosine clusters n ≥ 3")? Default proposed: 2, gate-clustered merges require 3.
Q2 Reconstruction overlay storage: reuse `ferricula-episode` overlay (operator-approved) or a new core record kind? I only emit `ReconstructionProposed`.
Q3 Legacy-fact pool: `Pool::Unclassified` recalled by default (my read of your instruction). Confirm.
Q4 May I make `dream_cycle` non-destructive now (behaviour change inside my crate, 14 existing tests updated), or keep the old function under `#[deprecated]` and add the new one beside it? Proposed: change it; deletion has no caller.
