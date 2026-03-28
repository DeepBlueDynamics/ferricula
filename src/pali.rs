//! Pāḷi translation layer — bidirectional term expansion between
//! Abhidhamma vocabulary and computational implementation terms.
//!
//! Sits between raw text and embedding: expands Pali terms to their
//! code equivalents (and vice versa) so both vocabularies land in the
//! same vector neighborhood. "anicca" and "decay" become neighbors.
//!
//! Based on the Paññatti glossary from AA.md (Abhidhamma Citta-Vīthi Architecture).

use std::collections::HashMap;

/// A bidirectional term mapping: Pali ↔ computational concepts.
struct TermPair {
    pali: &'static str,
    pali_meaning: &'static str,
    code_terms: &'static [&'static str],
}

/// The canonical Paññatti glossary.
const GLOSSARY: &[TermPair] = &[
    TermPair {
        pali: "phassa",
        pali_meaning: "contact",
        code_terms: &["ingest", "embed", "contact", "input", "sense impression"],
    },
    TermPair {
        pali: "vedanā",
        pali_meaning: "feeling-tone",
        code_terms: &["emotion", "feeling", "valence", "sentiment", "primary emotion", "secondary emotion"],
    },
    TermPair {
        pali: "saññā",
        pali_meaning: "perception",
        code_terms: &["classify", "recognition", "strata", "channel", "perception", "stems", "signals"],
    },
    TermPair {
        pali: "saṅkhāra",
        pali_meaning: "formation",
        code_terms: &["consolidate", "consolidation", "merge", "formation", "centroid", "compress"],
    },
    TermPair {
        pali: "viññāṇa",
        pali_meaning: "consciousness",
        code_terms: &["agent", "identity", "consciousness", "identity key", "agent id"],
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
        code_terms: &["decay", "impermanence", "fading", "alpha", "decay rate", "transient"],
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
        code_terms: &["dream", "dreaming", "cultivation", "dream cycle", "restructuring"],
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
        code_terms: &["embedding", "vector", "representation", "sign", "geometric meaning"],
    },
    TermPair {
        pali: "jarā",
        pali_meaning: "aging",
        code_terms: &["decay alpha", "aging", "decay rate", "alpha", "adaptive decay"],
    },
    TermPair {
        pali: "kamma",
        pali_meaning: "action",
        code_terms: &["hexagram", "identity seed", "entropy", "action seed", "casting"],
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
        code_terms: &["channel", "sense", "hearing", "seeing", "thinking", "sensory"],
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
        code_terms: &["strata", "aggregate", "component", "who what when where why how"],
    },
];

/// Build lookup tables on first use.
fn build_maps() -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut pali_to_code: HashMap<String, Vec<String>> = HashMap::new();
    let mut code_to_pali: HashMap<String, Vec<String>> = HashMap::new();

    for pair in GLOSSARY {
        // pali term → code terms
        let mut code_expansions: Vec<String> = pair.code_terms.iter().map(|s| s.to_string()).collect();
        code_expansions.push(pair.pali_meaning.to_string());
        pali_to_code.insert(pair.pali.to_string(), code_expansions);

        // Also map the English meaning → code terms
        let meaning_lower = pair.pali_meaning.to_lowercase();
        pali_to_code
            .entry(meaning_lower)
            .or_default()
            .extend(pair.code_terms.iter().map(|s| s.to_string()));

        // Each code term → pali + meaning
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

// Thread-local cached maps (built once per thread).
thread_local! {
    static MAPS: (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) = build_maps();
}

/// Expand text by appending Pali↔code equivalents for any recognized terms.
///
/// Given "the decay rate controls fidelity", returns:
/// "the decay rate controls fidelity | anicca (impermanence) jarā (aging) ojā (nutriment)"
///
/// Given "anicca governs all saṅkhāra", returns:
/// "anicca governs all saṅkhāra | decay impermanence consolidation formation"
///
/// The expansion is appended after a ` | ` separator so the embedding model
/// sees both vocabularies without corrupting the original text.
pub fn expand(text: &str) -> String {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != 'ā' && c != 'ī' && c != 'ū' && c != 'ṅ' && c != 'ñ' && c != 'ṭ' && c != 'ḍ' && c != 'ṇ' && c != 'ḷ')
        .filter(|w| w.len() >= 2)
        .collect();

    let mut expansions: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    MAPS.with(|(pali_to_code, code_to_pali)| {
        for word in &words {
            let w = word.to_string();

            // Check if it's a Pali term → expand to code terms
            if let Some(code_terms) = pali_to_code.get(&w) {
                for ct in code_terms {
                    if seen.insert(ct.clone()) {
                        expansions.push(ct.clone());
                    }
                }
            }

            // Check if it's a code term → expand to Pali
            if let Some(pali_terms) = code_to_pali.get(&w) {
                for pt in pali_terms {
                    if seen.insert(pt.clone()) {
                        expansions.push(pt.clone());
                    }
                }
            }

            // Also check multi-word code terms (e.g. "decay rate")
            // by checking bigrams
        }

        // Bigram check for multi-word terms
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
    });

    if expansions.is_empty() {
        text.to_string()
    } else {
        format!("{} | {}", text, expansions.join(" "))
    }
}

/// Check if text contains any recognized Pali terms.
pub fn has_pali(text: &str) -> bool {
    let lower = text.to_lowercase();
    MAPS.with(|(pali_to_code, _)| {
        pali_to_code.keys().any(|k| lower.contains(k.as_str()))
    })
}

/// Get the full glossary as JSON for API exposure.
pub fn glossary_json() -> String {
    let entries: Vec<serde_json::Value> = GLOSSARY
        .iter()
        .map(|pair| {
            serde_json::json!({
                "pali": pair.pali,
                "meaning": pair.pali_meaning,
                "code_terms": pair.code_terms,
            })
        })
        .collect();
    serde_json::json!({"glossary": entries, "count": entries.len()}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_pali_to_code() {
        let expanded = expand("anicca governs all memory");
        assert!(expanded.contains("decay"));
        assert!(expanded.contains("impermanence"));
    }

    #[test]
    fn expand_code_to_pali() {
        let expanded = expand("the decay rate controls fidelity");
        assert!(expanded.contains("anicca"));
        assert!(expanded.contains("ojā"));
    }

    #[test]
    fn expand_bidirectional() {
        let expanded = expand("consolidation implements saṅkhāra");
        // saṅkhāra → code terms
        assert!(expanded.contains("formation"));
        // consolidation → pali
        assert!(expanded.contains("saṅkhāra"));
    }

    #[test]
    fn no_expansion_for_unknown() {
        let text = "the quick brown fox";
        let expanded = expand(text);
        assert_eq!(expanded, text); // no ` | ` appended
    }

    #[test]
    fn expand_dream_to_bhavana() {
        let expanded = expand("dream cycle consolidation");
        assert!(expanded.contains("bhāvanā"));
        assert!(expanded.contains("saṅkhāra"));
    }

    #[test]
    fn glossary_json_valid() {
        let json = glossary_json();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(val["count"].as_u64().unwrap() > 20);
    }

    #[test]
    fn has_pali_detects() {
        assert!(has_pali("the anicca of all things"));
        assert!(has_pali("vedanā tags every memory"));
        assert!(!has_pali("the quick brown fox"));
    }
}
