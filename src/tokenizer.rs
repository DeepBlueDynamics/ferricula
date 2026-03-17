//! Word-level tokenizer + Porter stemmer for BM25 indexing.
//! Self-contained — no external crates required.

use std::collections::HashSet;

/// Tokenize text into lowercase words, filtering stopwords and short tokens.
pub fn tokenize(text: &str) -> Vec<String> {
    let stops = stopwords();
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2 && !stops.contains(*w))
        .map(String::from)
        .collect()
}

/// Tokenize + stem: the primary pipeline for term extraction.
pub fn extract_terms(text: &str) -> Vec<String> {
    tokenize(text).into_iter().map(|t| stem(&t)).collect()
}

// ---------------------------------------------------------------------------
// Porter stemmer (5-step algorithm)
// ---------------------------------------------------------------------------

/// Apply the Porter stemming algorithm to a single word.
pub fn stem(word: &str) -> String {
    if word.len() <= 2 {
        return word.to_string();
    }
    let mut s: Vec<char> = word.chars().collect();

    step1a(&mut s);
    step1b(&mut s);
    step1c(&mut s);
    step2(&mut s);
    step3(&mut s);
    step4(&mut s);
    step5(&mut s);

    s.into_iter().collect()
}

fn is_consonant(s: &[char], i: usize) -> bool {
    match s[i] {
        'a' | 'e' | 'i' | 'o' | 'u' => false,
        'y' => {
            if i == 0 {
                true
            } else {
                !is_consonant(s, i - 1)
            }
        }
        _ => true,
    }
}

/// Measure m — number of VC sequences in s[0..len].
fn measure(s: &[char]) -> usize {
    let n = s.len();
    if n == 0 {
        return 0;
    }
    let mut i = 0;
    // skip initial consonants
    while i < n && is_consonant(s, i) {
        i += 1;
    }
    let mut m = 0;
    loop {
        // skip vowels
        while i < n && !is_consonant(s, i) {
            i += 1;
        }
        if i >= n {
            break;
        }
        m += 1;
        // skip consonants
        while i < n && is_consonant(s, i) {
            i += 1;
        }
        if i >= n {
            break;
        }
    }
    m
}

/// Does the stem contain a vowel?
fn has_vowel(s: &[char]) -> bool {
    (0..s.len()).any(|i| !is_consonant(s, i))
}

/// Does the stem end with a double consonant?
fn ends_double_consonant(s: &[char]) -> bool {
    let n = s.len();
    n >= 2 && s[n - 1] == s[n - 2] && is_consonant(s, n - 1)
}

/// *o — the stem ends cvc, where the second c is not w, x, or y.
fn ends_cvc(s: &[char]) -> bool {
    let n = s.len();
    if n < 3 {
        return false;
    }
    is_consonant(s, n - 3)
        && !is_consonant(s, n - 2)
        && is_consonant(s, n - 1)
        && !matches!(s[n - 1], 'w' | 'x' | 'y')
}

fn ends_with(s: &[char], suffix: &str) -> bool {
    let sc: Vec<char> = suffix.chars().collect();
    if s.len() < sc.len() {
        return false;
    }
    let start = s.len() - sc.len();
    s[start..] == sc[..]
}

fn replace_suffix(s: &mut Vec<char>, suffix: &str, replacement: &str) {
    let slen = suffix.chars().count();
    s.truncate(s.len() - slen);
    s.extend(replacement.chars());
}

fn replace_suffix_if(s: &mut Vec<char>, suffix: &str, replacement: &str, min_m: usize) {
    if ends_with(s, suffix) {
        let slen = suffix.chars().count();
        let stem_part: Vec<char> = s[..s.len() - slen].to_vec();
        if measure(&stem_part) > min_m {
            replace_suffix(s, suffix, replacement);
        }
    }
}

// Step 1a: plurals
fn step1a(s: &mut Vec<char>) {
    if ends_with(s, "sses") {
        replace_suffix(s, "sses", "ss");
    } else if ends_with(s, "ies") {
        replace_suffix(s, "ies", "i");
    } else if !ends_with(s, "ss") && ends_with(s, "s") {
        s.pop();
    }
}

// Step 1b: -ed, -ing
fn step1b(s: &mut Vec<char>) {
    if ends_with(s, "eed") {
        let stem_part: Vec<char> = s[..s.len() - 3].to_vec();
        if measure(&stem_part) > 0 {
            replace_suffix(s, "eed", "ee");
        }
        return;
    }

    let mut did_trim = false;
    if ends_with(s, "ed") {
        let stem_part: Vec<char> = s[..s.len() - 2].to_vec();
        if has_vowel(&stem_part) {
            replace_suffix(s, "ed", "");
            did_trim = true;
        }
    } else if ends_with(s, "ing") {
        let stem_part: Vec<char> = s[..s.len() - 3].to_vec();
        if has_vowel(&stem_part) {
            replace_suffix(s, "ing", "");
            did_trim = true;
        }
    }

    if did_trim {
        if ends_with(s, "at") {
            s.push('e');
        } else if ends_with(s, "bl") {
            s.push('e');
        } else if ends_with(s, "iz") {
            s.push('e');
        } else if ends_double_consonant(s) && !matches!(s.last(), Some('l' | 's' | 'z')) {
            s.pop();
        } else if measure(s) == 1 && ends_cvc(s) {
            s.push('e');
        }
    }
}

// Step 1c: y → i
fn step1c(s: &mut Vec<char>) {
    if ends_with(s, "y") {
        let stem_part: Vec<char> = s[..s.len() - 1].to_vec();
        if has_vowel(&stem_part) {
            let n = s.len();
            s[n - 1] = 'i';
        }
    }
}

// Step 2: derivational suffixes (m > 0)
fn step2(s: &mut Vec<char>) {
    let rules: &[(&str, &str)] = &[
        ("ational", "ate"),
        ("tional", "tion"),
        ("enci", "ence"),
        ("anci", "ance"),
        ("izer", "ize"),
        ("abli", "able"),
        ("alli", "al"),
        ("entli", "ent"),
        ("eli", "e"),
        ("ousli", "ous"),
        ("ization", "ize"),
        ("ation", "ate"),
        ("ator", "ate"),
        ("alism", "al"),
        ("iveness", "ive"),
        ("fulness", "ful"),
        ("ousness", "ous"),
        ("aliti", "al"),
        ("iviti", "ive"),
        ("biliti", "ble"),
    ];
    for &(suffix, replacement) in rules {
        if ends_with(s, suffix) {
            let slen = suffix.chars().count();
            let stem_part: Vec<char> = s[..s.len() - slen].to_vec();
            if measure(&stem_part) > 0 {
                replace_suffix(s, suffix, replacement);
            }
            return;
        }
    }
}

// Step 3: more derivational (m > 0)
fn step3(s: &mut Vec<char>) {
    let rules: &[(&str, &str)] = &[
        ("icate", "ic"),
        ("ative", ""),
        ("alize", "al"),
        ("iciti", "ic"),
        ("ical", "ic"),
        ("ful", ""),
        ("ness", ""),
    ];
    for &(suffix, replacement) in rules {
        if ends_with(s, suffix) {
            let slen = suffix.chars().count();
            let stem_part: Vec<char> = s[..s.len() - slen].to_vec();
            if measure(&stem_part) > 0 {
                replace_suffix(s, suffix, replacement);
            }
            return;
        }
    }
}

// Step 4: long suffixes (m > 1)
fn step4(s: &mut Vec<char>) {
    let suffixes: &[&str] = &[
        "al", "ance", "ence", "er", "ic", "able", "ible", "ant", "ement", "ment", "ent",
        "ion", "ou", "ism", "ate", "iti", "ous", "ive", "ize",
    ];
    for &suffix in suffixes {
        if ends_with(s, suffix) {
            let slen = suffix.chars().count();
            let stem_part: Vec<char> = s[..s.len() - slen].to_vec();
            if measure(&stem_part) > 1 {
                // Special: -ion requires stem ending in s or t
                if suffix == "ion" {
                    if let Some(&c) = stem_part.last() {
                        if c != 's' && c != 't' {
                            return;
                        }
                    }
                }
                replace_suffix(s, suffix, "");
            }
            return;
        }
    }
}

// Step 5: cleanup
fn step5(s: &mut Vec<char>) {
    // 5a: remove trailing e
    if ends_with(s, "e") {
        let stem_part: Vec<char> = s[..s.len() - 1].to_vec();
        let m = measure(&stem_part);
        if m > 1 || (m == 1 && !ends_cvc(&stem_part)) {
            s.pop();
        }
    }
    // 5b: ll → l if m > 1
    if ends_with(s, "ll") && measure(s) > 1 {
        s.pop();
    }
}

// ---------------------------------------------------------------------------
// Stopwords
// ---------------------------------------------------------------------------

fn stopwords() -> HashSet<&'static str> {
    [
        "a", "about", "above", "after", "again", "against", "all", "am", "an", "and",
        "any", "are", "aren", "arent", "as", "at", "be", "because", "been", "before",
        "being", "below", "between", "both", "but", "by", "can", "cannot", "could",
        "couldn", "couldnt", "did", "didn", "didnt", "do", "does", "doesn", "doesnt",
        "doing", "don", "dont", "down", "during", "each", "few", "for", "from",
        "further", "get", "got", "had", "hadn", "hadnt", "has", "hasn", "hasnt",
        "have", "haven", "havent", "having", "he", "her", "here", "hers", "herself",
        "him", "himself", "his", "how", "if", "in", "into", "is", "isn", "isnt",
        "it", "its", "itself", "just", "ll", "me", "might", "more", "most", "mustn",
        "mustnt", "my", "myself", "need", "no", "nor", "not", "now", "of", "off",
        "on", "once", "only", "or", "other", "our", "ours", "ourselves", "out",
        "over", "own", "re", "same", "shan", "shant", "she", "should", "shouldn",
        "shouldnt", "so", "some", "such", "than", "that", "the", "their", "theirs",
        "them", "themselves", "then", "there", "these", "they", "this", "those",
        "through", "to", "too", "under", "until", "up", "ve", "very", "was",
        "wasn", "wasnt", "we", "were", "weren", "werent", "what", "when", "where",
        "which", "while", "who", "whom", "why", "will", "with", "won", "wont",
        "would", "wouldn", "wouldnt", "you", "your", "yours", "yourself", "yourselves",
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        let tokens = tokenize("The Weber bracket departs from Newton");
        assert!(tokens.contains(&"weber".to_string()));
        assert!(tokens.contains(&"bracket".to_string()));
        assert!(tokens.contains(&"departs".to_string()));
        assert!(tokens.contains(&"newton".to_string()));
        // "the" and "from" are stopwords
        assert!(!tokens.contains(&"the".to_string()));
        assert!(!tokens.contains(&"from".to_string()));
    }

    #[test]
    fn stem_plurals() {
        assert_eq!(stem("caresses"), "caress");
        assert_eq!(stem("ponies"), "poni");
        assert_eq!(stem("cats"), "cat");
    }

    #[test]
    fn stem_past_tenses() {
        assert_eq!(stem("agreed"), "agre");
        assert_eq!(stem("plastered"), "plaster");
        assert_eq!(stem("hopping"), "hop");
        assert_eq!(stem("hoping"), "hope");
    }

    #[test]
    fn stem_derivational() {
        assert_eq!(stem("relational"), "relat");
        assert_eq!(stem("conditional"), "condit");
        assert_eq!(stem("electrical"), "electr");
    }

    #[test]
    fn extract_terms_pipeline() {
        let terms = extract_terms("The Weber bracket departs from Newton");
        assert!(terms.contains(&"weber".to_string()));
        assert!(terms.contains(&"bracket".to_string()));
        assert!(terms.contains(&"newton".to_string()));
        // "departs" should be stemmed to "depart"
        assert!(terms.contains(&"depart".to_string()));
    }
}
