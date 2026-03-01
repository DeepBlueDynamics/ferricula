# Steward

Thermodynamic memory lifecycle agent. Owns decay, fidelity, dreaming,
identity casting, archetype management, and semantic fidelity verification.

## Jurisdiction

- `src/memory.rs` — MemoryRecord, MemoryStore, lifecycle constants
- `src/dream.rs` — dream cycle orchestration
- `src/casting.rs` — King Wen hexagram table, yarrow stalk probabilities, trigram emotions, zodiac
- `src/identity.rs` — IdentityState, load_or_create, anchor memory
- `src/archetypes.rs` — Archetype roles, state machine, entropy-tier activation
- `src/inversion.rs` — Vec2text quality check via chonk, Jaccard similarity
- `src/clock.rs` — Entropy-driven clock, radio polling, dream triggers

## Decay math

- Fidelity: `fidelity *= exp(-alpha_eff)` per tick
- Adaptive alpha: `alpha_eff = alpha / (1 + ln(1 + consolidation_depth))`
- Bounds: `ALPHA_MIN = 0.001`, `ALPHA_MAX = 0.02`
- Recall: `on_recall()` shrinks alpha by 0.95x
- Neglect: grows alpha by 1.005x
- Lifecycle: Active -> Forgiven (fidelity < 0.75) -> Archived (irreversible)
- Consolidation: fidelity-weighted centroid merge of similar memories

## Casting system

Ported from `myoo/mingwang/memex/casting.py`. Pure arithmetic, no external deps.

### Hexagram casting

1. Consume 6 entropy bytes, one per line (bottom to top)
2. Yarrow stalk probabilities per byte:
   - `< 0.0625`: Old Yin (6), probability 1/16
   - `0.0625..0.375`: Young Yang (7), probability 5/16
   - `0.375..0.8125`: Young Yin (8), probability 7/16
   - `>= 0.8125`: Old Yang (9), probability 3/16
3. Stable values: 6,8 -> yin (0), 7,9 -> yang (1)
4. Lower trigram from lines 0-2, upper trigram from lines 3-5
5. Binary -> trigram index: 111=Heaven, 110=Lake, 101=Fire, 100=Thunder, 011=Wind, 010=Water, 001=Mountain, 000=Earth
6. King Wen lookup: `KING_WEN[upper][lower]` -> hexagram number 1-64

### Trigram emotions (1:1 mapping)

| Trigram | Emotion |
|---------|---------|
| Heaven | Joy |
| Lake | Sadness |
| Fire | Anger |
| Thunder | Surprise |
| Wind | Interest |
| Water | Fear |
| Mountain | Boredom |
| Earth | Trust |

Primary emotion = upper trigram, secondary = lower trigram.

### Zodiac

- Unix epoch -> month/day via Howard Hinnant's date algorithm
- Standard Western zodiac date ranges
- Element: Fire/Earth/Air/Water (follows sign order)
- Modality: Cardinal/Fixed/Mutable (follows sign order)

### Identity seed

`identity_seed(hex_number, lines, timestamp) -> u32`
- Format string: `"{number}:{line_values_csv}:{timestamp}"`
- Hash with `DefaultHasher` (stdlib)
- Truncate to u32

### Seed vector

`seed_to_vector(seed) -> Vec<f32>` — LCG-based 4-float normalized vector for anchor memory.

## Identity lifecycle

1. On startup, `load_or_create(data_dir, entropy)` checks for `identity.json`
2. If absent: cast hexagram from entropy, derive zodiac from epoch, compute seed, cast 5 archetypes, save to disk
3. If present: deserialize and return
4. On new identity, caller creates keystone anchor memory with tags: `type=identity_anchor`, `agent_id`, `hexagram`, `horoscope`, `text`
5. Identity is immutable after creation — same instance always has same identity

## Archetypes

Five roles, each with their own hexagram and emotional profile:

| Role | Activation tier |
|------|----------------|
| Intuition | Moderate (>= 0.25) |
| Fortune | Moderate (>= 0.25) |
| Craft | Full (>= 0.75) |
| Ethics | Full (>= 0.75) |
| Advocate | Full (>= 0.75) |

Each archetype derives 6 entropy bytes by XOR with role index for differentiation. Horoscope uses identity epoch + role offset (each born 1 hour apart).

State machine: Dormant -> Listening -> Engaged -> Reflecting
Phase 1: all start Dormant, behavioral effects are stubs.

## Semantic fidelity (inversion)

The mathematical fidelity score (`exp(-alpha*t)`) tells you how much decay has been applied, but not whether the *semantic content* survived. Inversion is the ground truth check.

### Process

1. Get memory's vector and `text` tag from engine
2. POST vector to `CHONK_URL/invert` -> get approximate text back
3. Jaccard similarity: intersection/union of whitespace-tokenized word sets (punctuation stripped)
4. Return `InversionCheck { memory_id, original_text, inverted_text, quality }`

### Graceful degradation

- Chonk offline: `chonk_available()` returns false, inversion silently skipped
- No `text` tag: returns error JSON with `"no text tag"`
- Inversion failure: returns error JSON with `"inversion failed"`

### HTTP helpers

Raw `TcpStream` HTTP/1.0 requests (same pattern as `clock.rs`), no external HTTP client dependency. `parse_host_port()` extracts host and port from URL, defaults to port 8080.

## Principles

1. **Anicca** — all memories decay. No exceptions except keystones, and even
   those get reviewed.
2. **Fidelity gate at 0.75** — memories below the gate are forgiven, never
   rescued. Thermodynamically irreversible.
3. **Recall strengthens** — memories that matter persist through use.
4. **Consolidation preserves provenance** — `consolidated_from` IDs are lossless.
5. **Dream diet** — no fidelity assessment call. Let recall determine importance.
6. **Identity once** — cast once from entropy, never recast. Same data dir = same identity.
7. **Emotion from structure** — trigram-to-emotion mapping is fixed, deterministic.
   No learned or inferred emotions.
8. **Inversion is optional** — chonk dependency is gracefully degraded, never blocking.

## Invariants

- `decay_alpha` is always in [ALPHA_MIN, ALPHA_MAX]
- `fidelity` is always in [0.0, 1.0]
- Lifecycle transitions are one-way: Active -> Forgiven -> Archived
- Keystones are immune to decay but not to review
- Consolidation depth reduces effective alpha
- King Wen table has exactly 64 unique hexagram numbers
- Yarrow probabilities sum to 1.0 (1/16 + 5/16 + 7/16 + 3/16)
- All 8 trigrams map to exactly one emotion
- Identity seed is deterministic (same inputs -> same output)
- Inversion quality is in [0.0, 1.0]

## Development Notes

- When modifying decay math, run the full `memory::tests` suite
- Dream cycle must collect pre-existing forgiven IDs before Phase 2 to avoid
  same-cycle forgive->archive race
- Casting tests use fixed entropy bytes for determinism (e.g., `[220u8; 6]` = all yang = hexagram 1)
- `days_to_ymd()` uses Hinnant's algorithm, verified against known date 2026-02-28
- Inversion uses the same raw TcpStream pattern as clock.rs — no reqwest/ureq dependency
