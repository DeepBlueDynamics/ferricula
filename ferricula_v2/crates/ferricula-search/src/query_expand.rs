//! Versioned query-side glossary expansion (Pāli ↔ computational terms).
//!
//! Cross-language equity for retrieval: expands a query with the Pāli↔code
//! term equivalents so both vocabularies land in the same retrieval
//! neighborhood ("anicca" and "decay" become neighbors), per the Lume ×
//! Ollaya × Abhidhamma plan (§3.1 embedding-space boundary) and the
//! fleet's multilingual directive (Wren, 2026-09-26).
//!
//! Contract:
//! - **Query-side only.** Section text, `section_hash` (ExactID) and stored
//!   vectors are never modified — expansion must not change ingest identity.
//! - **Versioned + original retained.** `QueryExpansion` carries the glossary
//!   version and keeps the untouched original query; the expansion is
//!   appended after `" | "` so the embedding model sees both vocabularies
//!   without corrupting the original text (same layout cognition uses).
//! - **Canonical data lives in `crates/ferricula-cognition/src/pali.rs`**
//!   (owner: cognition lane, n8-olive-crow). The snapshot below duplicates
//!   that data 1:1 with identical `build_maps` semantics. A runtime JSON
//!   glossary (`LUME_QUERY_GLOSSARY`, in cognition's `glossary_json()`
//!   shape) overrides the snapshot without code changes; drift between the
//!   two copies is tracked in `INTEGRATION_CONTRACT.md`.
//! - **Language tracks.** Pure-Chinese / pure-Pāli evaluation tracks disable
//!   expansion (`LUME_QUERY_EXPAND=0`) so a pure track measures only its own
//!   language; expansion is a production bridge, not a track contaminant.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Snapshot provenance + version. Bump when the cognition glossary changes.
pub const SNAPSHOT_VERSION: &str = "pali-snapshot-2026-09-26(cognition/src/pali.rs)";

/// A bidirectional term mapping: Pali ↔ computational concepts.
/// Same shape as cognition `pali.rs::TermPair`.
struct TermPair {
    pali: &'static str,
    pali_meaning: &'static str,
    code_terms: &'static [&'static str],
}

/// Snapshot of the canonical Paññatti glossary from
/// `crates/ferricula-cognition/src/pali.rs` (verified equal on 2026-09-26).
const GLOSSARY: &[TermPair] = &[
    TermPair {
        pali: "phassa",
        pali_meaning: "contact",
        code_terms: &["ingest", "embed", "contact", "input", "sense impression"],
    },
    TermPair {
        pali: "vedanā",
        pali_meaning: "feeling-tone",
        code_terms: &[
            "emotion",
            "feeling",
            "valence",
            "sentiment",
            "primary emotion",
            "secondary emotion",
        ],
    },
    TermPair {
        pali: "saññā",
        pali_meaning: "perception",
        code_terms: &[
            "classify",
            "recognition",
            "strata",
            "channel",
            "perception",
            "stems",
            "signals",
        ],
    },
    TermPair {
        pali: "saṅkhāra",
        pali_meaning: "formation",
        code_terms: &[
            "consolidate",
            "consolidation",
            "merge",
            "formation",
            "centroid",
            "compress",
        ],
    },
    TermPair {
        pali: "viññāṇa",
        pali_meaning: "consciousness",
        code_terms: &[
            "agent",
            "identity",
            "consciousness",
            "identity key",
            "agent id",
        ],
    },
    TermPair {
        pali: "sati",
        pali_meaning: "mindfulness",
        code_terms: &["recall", "search", "remember", "mindfulness", "attention"],
    },
    TermPair {
        pali: "samādhi",
        pali_meaning: "concentration",
        code_terms: &["anchor", "keystone", "immovable", "concentration", "fixed"],
    },
    TermPair {
        pali: "anicca",
        pali_meaning: "impermanence",
        code_terms: &[
            "decay",
            "impermanence",
            "fading",
            "alpha",
            "decay rate",
            "transient",
        ],
    },
    TermPair {
        pali: "upekkhā",
        pali_meaning: "equanimity",
        code_terms: &["forgive", "forgiveness", "release", "equanimity", "let go"],
    },
    TermPair {
        pali: "nirodha",
        pali_meaning: "cessation",
        code_terms: &["archive", "cessation", "archived", "end", "cease"],
    },
    TermPair {
        pali: "bhāvanā",
        pali_meaning: "cultivation",
        code_terms: &[
            "dream",
            "dreaming",
            "cultivation",
            "dream cycle",
            "restructuring",
        ],
    },
    TermPair {
        pali: "cetanā",
        pali_meaning: "intention",
        code_terms: &["emotion knobs", "intention", "volition", "configuration"],
    },
    TermPair {
        pali: "ojā",
        pali_meaning: "nutriment",
        code_terms: &["fidelity", "vitality", "strength", "nutriment", "health"],
    },
    TermPair {
        pali: "nimitta",
        pali_meaning: "sign",
        code_terms: &[
            "embedding",
            "vector",
            "representation",
            "sign",
            "geometric meaning",
        ],
    },
    TermPair {
        pali: "jarā",
        pali_meaning: "aging",
        code_terms: &[
            "decay alpha",
            "aging",
            "decay rate",
            "alpha",
            "adaptive decay",
        ],
    },
    TermPair {
        pali: "kamma",
        pali_meaning: "action",
        code_terms: &[
            "hexagram",
            "identity seed",
            "entropy",
            "action seed",
            "casting",
        ],
    },
    TermPair {
        pali: "dukkha",
        pali_meaning: "suffering",
        code_terms: &["death spiral", "amnesia", "stagnation", "low fidelity"],
    },
    TermPair {
        pali: "paṭicchanna",
        pali_meaning: "concealed",
        code_terms: &["sealed", "suppressed", "hidden", "concealed"],
    },
    TermPair {
        pali: "punabbhava",
        pali_meaning: "re-becoming",
        code_terms: &["revive", "re-becoming", "rebirth", "new from archived"],
    },
    TermPair {
        pali: "āyatana",
        pali_meaning: "sense base",
        code_terms: &[
            "channel", "sense", "hearing", "seeing", "thinking", "sensory",
        ],
    },
    TermPair {
        pali: "citta",
        pali_meaning: "mind",
        code_terms: &["memory", "mind", "record", "mental state"],
    },
    TermPair {
        pali: "ākāsa",
        pali_meaning: "space",
        code_terms: &["radio", "entropy", "noise", "static", "space"],
    },
    TermPair {
        pali: "paññā",
        pali_meaning: "wisdom",
        code_terms: &["quality gate", "fidelity gate", "wisdom", "understanding"],
    },
    TermPair {
        pali: "sīla",
        pali_meaning: "virtue",
        code_terms: &["lifecycle", "rules", "constraints", "one-way transitions"],
    },
    TermPair {
        pali: "jhāna",
        pali_meaning: "absorption",
        code_terms: &["dream cycle", "absorption", "concentrated restructuring"],
    },
    TermPair {
        pali: "khandha",
        pali_meaning: "aggregate",
        code_terms: &[
            "strata",
            "aggregate",
            "component",
            "who what when where why how",
        ],
    },
    TermPair {
        pali: "javana",
        pali_meaning: "impulsion",
        // Runs 7× in each citta-vīthi — the active processing phase that
        // conditions the next moment. Determines overlap ratio (~7/17 ≈ 41%)
        // for chunked embedding.
        code_terms: &[
            "impulse",
            "processing",
            "chunk overlap",
            "context window",
            "sliding window",
            "stride",
        ],
    },
    TermPair {
        pali: "bhavanga",
        pali_meaning: "life-continuum",
        // Background stream of consciousness between active processes; in
        // chunking, the overlapping prefix providing continuity between
        // adjacent embedding windows.
        code_terms: &[
            "background",
            "continuity",
            "stream",
            "context carry",
            "overlap prefix",
            "prior state",
        ],
    },
    TermPair {
        pali: "nimitta",
        pali_meaning: "sign",
        // The initial cognitive sign/image a vithi latches onto; in
        // embedding, the dominant semantic object a chunk encodes.
        code_terms: &[
            "sign",
            "image",
            "anchor",
            "dominant topic",
            "semantic object",
            "embedding target",
        ],
    },
    TermPair {
        pali: "tadārammaṇa",
        pali_meaning: "registration",
        // Final 2 moments of each vithi — registers the completed perception
        // into the stream. In chunking: the last semantic units of each chunk
        // present in the next chunk's overlap.
        code_terms: &[
            "registration",
            "commit",
            "persist",
            "write",
            "finalize",
            "tail overlap",
        ],
    },
    TermPair {
        pali: "manodvāra",
        pali_meaning: "mind-door",
        // Internal sense-door for mental objects; in embedding, the attention
        // mechanism integrating prior context with current input.
        code_terms: &[
            "attention",
            "transformer",
            "self-attention",
            "context integration",
            "mind door",
        ],
    },
];

type Maps = (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>);

/// Build lookup tables — identical semantics to cognition
/// `pali.rs::build_maps`, including the meaning-keyed alias map and the
/// `"pali (meaning)"` reverse formatting.
fn build_maps() -> Maps {
    let mut pali_to_code: HashMap<String, Vec<String>> = HashMap::new();
    let mut code_to_pali: HashMap<String, Vec<String>> = HashMap::new();

    for pair in GLOSSARY {
        let mut code_expansions: Vec<String> =
            pair.code_terms.iter().map(|s| s.to_string()).collect();
        code_expansions.push(pair.pali_meaning.to_string());
        pali_to_code.insert(pair.pali.to_string(), code_expansions);

        let meaning_lower = pair.pali_meaning.to_lowercase();
        pali_to_code
            .entry(meaning_lower)
            .or_default()
            .extend(pair.code_terms.iter().map(|s| s.to_string()));

        for code_term in pair.code_terms {
            let ct = code_term.to_lowercase();
            code_to_pali
                .entry(ct)
                .or_default()
                .push(format!("{} ({})", pair.pali, pair.pali_meaning));
        }
    }

    (pali_to_code, code_to_pali)
}

static SNAPSHOT_MAPS: LazyLock<Maps> = LazyLock::new(build_maps);

fn snapshot_maps() -> &'static Maps {
    &SNAPSHOT_MAPS
}

/// The structured result of expanding one query. `original` is never
/// modified; `expanded` is what the retriever consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryExpansion {
    pub original: String,
    pub expanded: String,
    /// Terms appended after the ` | ` separator, in glossary order.
    pub terms_added: Vec<String>,
    /// Identifies which glossary produced the expansion.
    pub version: String,
}

impl QueryExpansion {
    fn unchanged(original: &str, version: &str) -> Self {
        Self {
            original: original.to_string(),
            expanded: original.to_string(),
            terms_added: Vec::new(),
            version: version.to_string(),
        }
    }
}

/// Whether query expansion is enabled. Default ON for production bridging;
/// `LUME_QUERY_EXPAND=0|false` disables (required for pure-language
/// evaluation tracks so they measure only their own language).
pub fn expansion_enabled() -> bool {
    match std::env::var("LUME_QUERY_EXPAND") {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

/// Identifies the active glossary: external JSON path when overridden,
/// otherwise the snapshot version.
pub fn glossary_version() -> String {
    match std::env::var("LUME_QUERY_GLOSSARY") {
        Ok(path) if !path.trim().is_empty() => format!("external:{}", path.trim()),
        _ => SNAPSHOT_VERSION.to_string(),
    }
}

/// Load the external glossary from `LUME_QUERY_GLOSSARY`, accepting either
/// cognition's `glossary_json()` shape
/// (`{"glossary":[{"pali","meaning","code_terms"}], "count": N}`) or a bare
/// array of the same objects. `meaning` falls back to the snapshot term's
/// meaning when omitted. Returns `Err` on IO/shape failure — the caller
/// decides whether to fall back to the snapshot.
fn load_external_glossary() -> Result<Vec<(String, String, Vec<String>)>, String> {
    let path = std::env::var("LUME_QUERY_GLOSSARY")
        .map_err(|_| "LUME_QUERY_GLOSSARY not set".to_string())?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path, e))?;
    let root: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse {} as JSON: {}", path, e))?;
    let arr = root
        .get("glossary")
        .and_then(|v| v.as_array())
        .cloned()
        .or_else(|| root.as_array().cloned())
        .ok_or_else(|| format!("{}: expected a glossary array", path))?;

    let mut pairs = Vec::with_capacity(arr.len());
    for entry in arr {
        let pali = entry
            .get("pali")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{}: glossary entry missing \"pali\"", path))?
            .to_string();
        let code_terms: Vec<String> = entry
            .get("code_terms")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let meaning = entry
            .get("meaning")
            .or_else(|| entry.get("pali_meaning"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
        pairs.push((pali, meaning, code_terms));
    }
    Ok(pairs)
}

/// Build maps from a loaded external glossary (same semantics as
/// `build_maps`); pairs with an empty `meaning` skip the meaning alias map.
fn build_external_maps(
    pairs: &[(String, String, Vec<String>)],
) -> Maps {
    let mut pali_to_code: HashMap<String, Vec<String>> = HashMap::new();
    let mut code_to_pali: HashMap<String, Vec<String>> = HashMap::new();
    for (pali, meaning, code_terms) in pairs {
        let mut expansions = code_terms.clone();
        if !meaning.is_empty() {
            expansions.push(meaning.clone());
            pali_to_code
                .entry(meaning.to_lowercase())
                .or_default()
                .extend(code_terms.iter().cloned());
        }
        pali_to_code.insert(pali.clone(), expansions);
        for ct in code_terms {
            code_to_pali
                .entry(ct.to_lowercase())
                .or_default()
                .push(if meaning.is_empty() {
                    pali.clone()
                } else {
                    format!("{} ({})", pali, meaning)
                });
        }
    }
    (pali_to_code, code_to_pali)
}

/// Expand a query with Pali↔code equivalents, retaining the original text.
///
/// Mirrors cognition `pali.rs::expand` semantics: lowercase word split
/// (keeping Pali diacritics), words ≥ 2 chars, both map directions plus a
/// multi-word bigram pass, deduplication, `" | "`-separated append. Returns
/// the query unchanged when expansion is disabled or nothing matches.
pub fn expand_query(query: &str) -> QueryExpansion {
    if !expansion_enabled() {
        return QueryExpansion::unchanged(query, &glossary_version());
    }


    // Prefer the external glossary; fall back to the snapshot on any error.
    // Borrowed either way — no per-query map clone.
    let external = load_external_glossary()
        .ok()
        .filter(|pairs| !pairs.is_empty())
        .map(|pairs| build_external_maps(&pairs));
    let maps: &Maps = external.as_ref().unwrap_or_else(|| snapshot_maps());
    let (pali_to_code, code_to_pali) = (&maps.0, &maps.1);
    let version = glossary_version();

    let lower = query.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| {
            !c.is_alphanumeric()
                && c != 'ā'
                && c != 'ī'
                && c != 'ū'
                && c != 'ṅ'
                && c != 'ñ'
                && c != 'ṭ'
                && c != 'ḍ'
                && c != 'ṇ'
                && c != 'ḷ'
        })
        .filter(|w| w.chars().count() >= 2)
        .collect();

    let mut expansions: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for word in &words {
        if let Some(code_terms) = pali_to_code.get(*word) {
            for ct in code_terms {
                if seen.insert(ct.clone()) {
                    expansions.push(ct.clone());
                }
            }
        }
        if let Some(pali_terms) = code_to_pali.get(*word) {
            for pt in pali_terms {
                if seen.insert(pt.clone()) {
                    expansions.push(pt.clone());
                }
            }
        }
    }

    for i in 0..words.len().saturating_sub(1) {
        let bigram = format!("{} {}", words[i], words[i + 1]);
        if let Some(pali_terms) = code_to_pali.get(&bigram) {
            for pt in pali_terms {
                if seen.insert(pt.clone()) {
                    expansions.push(pt.clone());
                }
            }
        }
    }

    if expansions.is_empty() {
        QueryExpansion::unchanged(query, &version)
    } else {
        QueryExpansion {
            original: query.to_string(),
            expanded: format!("{} | {}", query, expansions.join(" ")),
            terms_added: expansions,
            version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `std::env` is process-global and tests run in parallel; expansion is
    /// env-driven, so every env-mutating test in this module holds this lock
    /// for its full body. All vars for one test are set in ONE acquisition —
    /// `std::sync::Mutex` is not reentrant, so nested helpers would deadlock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_envs(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(var, _)| ((*var).to_string(), std::env::var(var).ok()))
            .collect();
        for (var, value) in vars {
            match value {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
        f();
        for (var, value) in saved {
            match value {
                Some(v) => std::env::set_var(&var, v),
                None => std::env::remove_var(&var),
            }
        }
    }

    const CLEAR: Option<&str> = None;

    #[test]
    fn pali_query_expands_to_code_terms() {
        with_envs(&[("LUME_QUERY_EXPAND", CLEAR), ("LUME_QUERY_GLOSSARY", CLEAR)], || {
            let exp = expand_query("anicca governs all saṅkhāra");
            assert_eq!(exp.original, "anicca governs all saṅkhāra");
            // Cognition parity: "anicca" → decay/impermanence/… ; "saṅkhāra"
            // → consolidate/merge/…
            assert!(exp.terms_added.iter().any(|t| t == "decay"), "terms: {:?}", exp.terms_added);
            assert!(exp.terms_added.iter().any(|t| t.contains("consolidate")));
            assert!(exp.expanded.starts_with("anicca governs all saṅkhāra | "));
            assert!(exp.version.starts_with("pali-snapshot-"));
        });
    }

    #[test]
    fn code_query_expands_to_pali() {
        with_envs(&[("LUME_QUERY_EXPAND", CLEAR), ("LUME_QUERY_GLOSSARY", CLEAR)], || {
            let exp = expand_query("the decay rate controls fidelity");
            assert!(exp.terms_added.iter().any(|t| t.contains("anicca")), "terms: {:?}", exp.terms_added);
            assert!(exp.expanded.starts_with("the decay rate controls fidelity | "));
        });
    }

    #[test]
    fn multi_word_bigram_expands() {
        with_envs(&[("LUME_QUERY_EXPAND", CLEAR), ("LUME_QUERY_GLOSSARY", CLEAR)], || {
            let exp = expand_query("chunk overlap window");
            assert!(exp.terms_added.iter().any(|t| t.contains("javana")), "terms: {:?}", exp.terms_added);
        });
    }

    #[test]
    fn expansion_can_be_disabled_for_pure_tracks() {
        with_envs(&[("LUME_QUERY_EXPAND", Some("0")), ("LUME_QUERY_GLOSSARY", CLEAR)], || {
            let exp = expand_query("anicca decay");
            assert_eq!(exp.expanded, "anicca decay");
            assert!(exp.terms_added.is_empty());
            assert!(!expansion_enabled());
        });
    }

    #[test]
    fn plain_query_is_unchanged() {
        with_envs(&[("LUME_QUERY_EXPAND", CLEAR), ("LUME_QUERY_GLOSSARY", CLEAR)], || {
            let exp = expand_query("Steve Jobs introduced the Macintosh");
            assert_eq!(exp.expanded, "Steve Jobs introduced the Macintosh");
            assert!(exp.terms_added.is_empty());
        });
    }

    #[test]
    fn external_glossary_overrides_snapshot() {
        let dir = std::env::temp_dir().join(format!("lume-qe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gloss.json");
        // cognition glossary_json() shape.
        std::fs::write(
            &path,
            r#"{"glossary":[{"pali":"苹果","meaning":"apple","code_terms":["computer","mac"]}],
               "count":1}"#,
        )
        .unwrap();
        with_envs(
            &[("LUME_QUERY_GLOSSARY", Some(path.to_str().unwrap())), ("LUME_QUERY_EXPAND", CLEAR)],
            || {
                let exp = expand_query("苹果 电脑");
                assert!(exp.terms_added.iter().any(|t| t == "computer"), "terms: {:?}", exp.terms_added);
                assert!(exp.version.starts_with("external:"));
                // Snapshot terms must NOT leak through while overridden.
                assert!(!exp.terms_added.iter().any(|t| t.contains("anicca")));
            },
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_external_glossary_falls_back_to_snapshot() {
        let dir = std::env::temp_dir().join(format!("lume-qe-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        with_envs(
            &[("LUME_QUERY_GLOSSARY", Some(path.to_str().unwrap())), ("LUME_QUERY_EXPAND", CLEAR)],
            || {
                let exp = expand_query("anicca");
                assert!(exp.terms_added.iter().any(|t| t == "decay"));
            },
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}