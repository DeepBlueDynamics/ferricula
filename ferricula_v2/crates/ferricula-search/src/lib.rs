//! FST-based text tagger — Rust port of the `App.java` reference from
//! <https://github.com/jsclosures/fstguardrails>.
//!
//! Pipeline (matches the Java analyzer chain):
//!
//!   1. Hyphen/dash stripping (so `sw-lucene` ≡ `swlucene`)
//!   2. ASCII folding (so `Zürich` ≡ `Zurich`)
//!   3. Tokenisation on non-alphanumeric boundaries, lowercased
//!   4. Tokens are joined with `0x1E` (Lucene's `SEP_LABEL`) to form the
//!      canonical FST key bytes
//!
//! Tagging walks the FST arc-by-arc over the input's analyzed token
//! stream, emitting the longest match at each cursor position (Solr's
//! `LONGEST_DOMINANT_RIGHT` / Java's forward-maximum-match). When two
//! dictionary phrases analyze to the same byte sequence (Zürich/Zurich),
//! every record sharing that key is emitted at the matched span.

use std::io;
use std::path::Path;

use tantivy_fst::raw::{Fst, Node, Output};
use tantivy_fst::MapBuilder;

pub mod bm25;
pub mod fast_retrieval;
pub mod graph_search;
pub mod semantic_mesh;
pub mod eval;
pub mod stream;
pub mod answer;
pub mod regex;
pub mod spelling;
pub mod inversion;
pub mod hybrid;
pub mod agent;
pub mod crawl;
// pub mod cli;
pub mod fusion;
pub mod query_expand;
pub mod rerank;
pub mod suppress;


/// Token separator used inside FST keys. Matches Lucene's
/// `ConcatenateGraphFilter.SEP_LABEL` (U+001E).
const SEP: u8 = 0x1E;

/// One dictionary entry: the phrase, the `kind` it belongs to (CSV
/// filename stem when loaded from a `DATA` directory), an opaque record
/// id, and an optional canonical `output` token. If `output` is `None`
/// the tagger derives it from the phrase (uppercase + alphanumeric).
#[derive(Debug, Clone)]
pub struct Entry {
    pub phrase: String,
    pub kind: String,
    pub id: String,
    pub output: Option<String>,
    pub is_regex: bool,
}

impl Entry {
    pub fn new(
        phrase: impl Into<String>,
        kind: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            phrase: phrase.into(),
            kind: kind.into(),
            id: id.into(),
            output: None,
            is_regex: false,
        }
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }

    pub fn with_regex(mut self, is_regex: bool) -> Self {
        self.is_regex = is_regex;
        self
    }
}

/// A single tag emitted by the tagger. Mirrors the Java `Tag` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Byte offset of the first matched character in the input.
    pub start: usize,
    /// Byte offset one past the last matched character in the input.
    pub end: usize,
    /// The matched span sliced verbatim from the input text.
    pub surface: String,
    /// The record id (UUID v4 when loaded from a `DATA` CSV).
    pub id: String,
    /// The record kind / type label (CSV filename stem).
    pub kind: String,
    /// Canonical normalized token for the match — from the `action`
    /// column when present, otherwise derived from the phrase.
    pub output: String,
}

/// What to do when several dictionary phrases match at the same position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlapPolicy {
    All,
    LongestOnly,
}

#[derive(Debug, Clone)]
struct MetaRecord {
    kind: String,
    id: String,
    output: String,
}

/// A compiled text tagger backed by an FST.
pub struct Tagger {
    fst: Fst<Vec<u8>>,
    /// FST value -> all records sharing that key (synonyms collapse here).
    groups: Vec<Vec<MetaRecord>>,
    regex_patterns: Vec<(crate::regex::Nfa, MetaRecord)>,
    phrases: Vec<String>,
}

impl Tagger {
    /// Build a tagger from a list of [`Entry`]s.
    pub fn build<I>(entries: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = Entry>,
    {
        let mut static_entries = Vec::new();
        let mut regex_patterns = Vec::new();
        let mut phrases = Vec::new();

        for e in entries {
            phrases.push(e.phrase.clone());
            let is_regex = e.is_regex || auto_detect_regex(&e.phrase);
            let output = e.output.clone().unwrap_or_else(|| derive_output(&e.phrase));
            let meta = MetaRecord {
                kind: e.kind.clone(),
                id: e.id.clone(),
                output,
            };

            if is_regex {
                match crate::regex::Nfa::compile(&e.phrase) {
                    Ok(nfa) => {
                        regex_patterns.push((nfa, meta));
                    }
                    Err(err) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Failed to compile regex '{}': {}", e.phrase, err),
                        ));
                    }
                }
            } else {
                static_entries.push((e, meta));
            }
        }

        // Normalize each phrase to its FST key bytes, drop empties.
        let mut prepared: Vec<(Vec<u8>, MetaRecord)> = static_entries
            .into_iter()
            .filter_map(|(e, meta)| {
                let key = normalize_key(&e.phrase);
                if key.is_empty() {
                    return None;
                }
                Some((key, meta))
            })
            .collect();

        prepared.sort_by(|a, b| a.0.cmp(&b.0));

        // Group records that share the same key (synonyms).
        let mut groups: Vec<Vec<MetaRecord>> = Vec::new();
        let mut keyed: Vec<(Vec<u8>, u64)> = Vec::new();
        for (key, rec) in prepared {
            if let Some(last) = keyed.last() {
                if last.0 == key {
                    let idx = last.1 as usize;
                    // dedupe exact duplicates (same id + kind + output) —
                    // matches Java's LinkedHashSet<id + "\t" + type + "\t" + output>
                    if !groups[idx]
                        .iter()
                        .any(|r| r.id == rec.id && r.kind == rec.kind && r.output == rec.output)
                    {
                        groups[idx].push(rec);
                    }
                    continue;
                }
            }
            let idx = groups.len() as u64;
            groups.push(vec![rec]);
            keyed.push((key, idx));
        }

        let mut builder = MapBuilder::memory();
        for (key, idx) in &keyed {
            builder
                .insert(key, *idx)
                .map_err(io::Error::other)?;
        }
        let bytes = builder
            .into_inner()
            .map_err(io::Error::other)?;
        let fst = Fst::new(bytes).map_err(io::Error::other)?;
        Ok(Self { fst, groups, regex_patterns, phrases })
    }

    /// Access the original loaded dictionary phrases
    pub fn phrases(&self) -> &[String] {
        &self.phrases
    }

    /// Number of distinct FST keys.
    pub fn len(&self) -> usize {
        self.fst.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fst.is_empty()
    }

    /// Total number of records (including synonyms collapsed onto the
    /// same FST key).
    pub fn record_count(&self) -> usize {
        self.groups.iter().map(|g| g.len()).sum()
    }

    /// Distinct kinds in the dictionary, sorted.
    pub fn kinds(&self) -> Vec<String> {
        let mut ks: Vec<String> = self
            .groups
            .iter()
            .flat_map(|g| g.iter().map(|r| r.kind.clone()))
            .collect();
        ks.sort();
        ks.dedup();
        ks
    }

    /// Build a tagger from a TSV file: each line is `phrase<TAB>id`.
    /// The file stem becomes the `kind` for every entry.
    pub fn from_tsv_file(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let kind = file_stem(path);
        let dict = std::fs::read_to_string(path)?;
        let entries: Vec<Entry> = dict
            .lines()
            .enumerate()
            .filter_map(|(i, line)| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let (phrase, id) = match line.split_once('\t') {
                    Some((p, v)) => (p.to_string(), v.trim().to_string()),
                    None => (line.to_string(), (i + 1).to_string()),
                };
                Some(Entry::new(phrase, kind.clone(), id))
            })
            .collect();
        Self::build(entries)
    }

    /// Load every `*.csv` file in `dir` (Java parity).
    ///
    /// Per `App.java::loadCsvData`:
    ///   * filenames are sorted alphabetically (case-insensitive)
    ///   * the first row is the header
    ///   * the first column is the phrase
    ///   * an `action` column (if present) becomes the record's `output`
    ///   * each record gets a fresh UUID v4 id
    ///   * the filename without the `.csv` extension is the record `kind`
    pub fn from_data_dir(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.eq_ignore_ascii_case("csv"))
                        .unwrap_or(false)
            })
            .collect();
        paths.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));

        // Empty DATA directory is a no-op (matches Java's loadCsvData,
        // which logs and returns without writing any documents).
        let mut entries: Vec<Entry> = Vec::new();
        for path in paths {
            let kind = file_stem(&path);
            let text = std::fs::read_to_string(&path)?;
            let mut lines = text.lines();
            let header_line = match lines.next() {
                Some(h) => h,
                None => continue,
            };
            let headers = parse_csv_line(header_line);
            let action_col = headers
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case("action"));
            let is_regex_col = headers
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case("is_regex"));

            for raw in lines {
                let cells = parse_csv_line(raw);
                let phrase = cells.first().map(|s| s.trim()).unwrap_or("");
                if phrase.is_empty() {
                    continue;
                }
                let output_override = action_col
                    .and_then(|i| cells.get(i))
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                let is_regex_val = is_regex_col
                    .and_then(|i| cells.get(i))
                    .map(|s| s.trim().eq_ignore_ascii_case("true"))
                    .unwrap_or(false);

                let mut entry = Entry::new(phrase, kind.clone(), uuid_v4())
                    .with_regex(is_regex_val);
                if let Some(o) = output_override {
                    entry = entry.with_output(o);
                }
                entries.push(entry);
            }
        }
        Self::build(entries)
    }

    /// If the `DATA` env var is set, build from that directory; otherwise
    /// `Ok(None)` so callers can fall back.
    pub fn from_env() -> io::Result<Option<Self>> {
        match std::env::var("DATA") {
            Ok(dir) if !dir.is_empty() => Self::from_data_dir(dir).map(Some),
            _ => Ok(None),
        }
    }

    /// Tag with the default policy (longest match per start position).
    pub fn tag(&self, text: &str) -> Vec<Tag> {
        self.tag_with(text, OverlapPolicy::LongestOnly)
    }

    /// Tag with an explicit overlap policy.
    pub fn tag_with(&self, text: &str, policy: OverlapPolicy) -> Vec<Tag> {
        let mut out: Vec<Tag> = Vec::new();

        // 1. Run FST dictionary matching (extracting all possible matches)
        let tokens = tokenize(text);
        for i in 0..tokens.len() {
            let mut node: Node = self.fst.root();
            let mut output: Output = Output::zero();

            for j in i..tokens.len() {
                if j > i {
                    match step(&self.fst, &node, SEP, output) {
                        Some((n, o)) => {
                            node = n;
                            output = o;
                        }
                        None => break,
                    }
                }

                let mut dead = false;
                for &b in &tokens[j].bytes {
                    match step(&self.fst, &node, b, output) {
                        Some((n, o)) => {
                            node = n;
                            output = o;
                        }
                        None => {
                            dead = true;
                            break;
                        }
                    }
                }
                if dead {
                    break;
                }

                if node.is_final() {
                    let idx = output.cat(node.final_output()).value();
                    self.emit(&mut out, &tokens, i, j, idx, text);
                }
            }
        }

        // 2. Run NFA Regex matching
        for (nfa, meta) in &self.regex_patterns {
            let char_spans = nfa.matches(text);
            let byte_spans = char_to_byte_offsets(text, &char_spans);
            for (start_byte, end_byte) in byte_spans {
                let surface = text[start_byte..end_byte].to_string();
                out.push(Tag {
                    start: start_byte,
                    end: end_byte,
                    surface,
                    id: meta.id.clone(),
                    kind: meta.kind.clone(),
                    output: meta.output.clone(),
                });
            }
        }

        // 3. Resolve overlaps if policy is LongestOnly
        let resolved = match policy {
            OverlapPolicy::All => {
                out.sort_by(|a, b| {
                    a.start.cmp(&b.start).then_with(|| {
                        let len_a = a.end - a.start;
                        let len_b = b.end - b.start;
                        len_b.cmp(&len_a)
                    })
                });
                out
            }
            OverlapPolicy::LongestOnly => resolve_longest_only(out),
        };
        dedup_identical_tags(resolved)
    }

    fn emit(
        &self,
        out: &mut Vec<Tag>,
        tokens: &[Token],
        i: usize,
        j: usize,
        idx: u64,
        text: &str,
    ) {
        let start = tokens[i].start;
        let end = tokens[j].end;
        let surface = text[start..end].to_string();
        for rec in &self.groups[idx as usize] {
            out.push(Tag {
                start,
                end,
                surface: surface.clone(),
                id: rec.id.clone(),
                kind: rec.kind.clone(),
                output: rec.output.clone(),
            });
        }
    }
}

fn char_to_byte_offsets(text: &str, char_spans: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let char_boundaries: Vec<usize> = text.char_indices().map(|(idx, _)| idx).collect();
    let mut byte_spans = Vec::new();
    for &(start_char, end_char) in char_spans {
        let start_byte = if start_char < char_boundaries.len() {
            char_boundaries[start_char]
        } else {
            text.len()
        };
        let end_byte = if end_char < char_boundaries.len() {
            char_boundaries[end_char]
        } else {
            text.len()
        };
        byte_spans.push((start_byte, end_byte));
    }
    byte_spans
}

/// Drops tags that are duplicates in EVERY field (span + output + kind + id),
/// i.e. duplicate dictionary rows that collapsed to the same FST key. Synonym
/// records sharing a span but carrying distinct ids are a feature and survive.
fn dedup_identical_tags(tags: Vec<Tag>) -> Vec<Tag> {
    let mut seen: std::collections::HashSet<(usize, usize, String, String, String)> =
        std::collections::HashSet::new();
    tags.into_iter()
        .filter(|t| seen.insert((t.start, t.end, t.output.clone(), t.kind.clone(), t.id.clone())))
        .collect()
}

fn resolve_longest_only(mut tags: Vec<Tag>) -> Vec<Tag> {
    if tags.is_empty() {
        return Vec::new();
    }
    tags.sort_by(|a, b| {
        a.start.cmp(&b.start).then_with(|| {
            let len_a = a.end - a.start;
            let len_b = b.end - b.start;
            len_b.cmp(&len_a)
        }).then_with(|| {
            a.id.cmp(&b.id)
        })
    });
    
    let mut resolved = Vec::new();
    let mut last_end = 0;
    let mut last_accepted_start = None;
    let mut last_accepted_end = None;
    
    for tag in tags {
        let is_exact_same_span = Some(tag.start) == last_accepted_start && Some(tag.end) == last_accepted_end;
        if tag.start >= last_end || is_exact_same_span {
            last_end = tag.end;
            last_accepted_start = Some(tag.start);
            last_accepted_end = Some(tag.end);
            resolved.push(tag);
        }
    }
    resolved
}

fn auto_detect_regex(phrase: &str) -> bool {
    phrase.chars().any(|c| matches!(c, '[' | ']' | '\\' | '*' | '+' | '?' | '|' | '(' | ')'))
}

fn step<'a>(
    fst: &'a Fst<Vec<u8>>,
    node: &Node<'a>,
    b: u8,
    output: Output,
) -> Option<(Node<'a>, Output)> {
    let idx = node.find_input(b)?;
    let t = node.transition(idx);
    Some((fst.node(t.addr), output.cat(t.out)))
}

// ─── Analyzer / tokenizer (folding-aware) ───────────────────────────────

#[derive(Debug, Clone)]
pub struct Token {
    pub bytes: Vec<u8>,
    pub start: usize,
    pub end: usize,
}


fn is_hyphen(c: char) -> bool {
    matches!(
        c,
        '\u{002D}' | '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}'
    )
}

/// ASCII-fold a single Unicode char into its lowercase ASCII form.
/// Returns `None` for chars not in the fold table (caller treats them as
/// separators).
fn fold_latin(c: char) -> Option<&'static str> {
    Some(match c {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
        'Æ' | 'æ' => "ae",
        'Ç' | 'ç' => "c",
        'È' | 'É' | 'Ê' | 'Ë' | 'è' | 'é' | 'ê' | 'ë' => "e",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'ì' | 'í' | 'î' | 'ï' => "i",
        'Ð' | 'ð' => "d",
        'Ñ' | 'ñ' => "n",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => "o",
        'Œ' | 'œ' => "oe",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'ù' | 'ú' | 'û' | 'ü' => "u",
        'Ý' | 'ý' | 'ÿ' => "y",
        'Þ' | 'þ' => "th",
        'ß' => "ss",
        // Pāli/Sanskrit diacritics (macrons and dot-below letters). Without
        // these, "saṅkhāra" splits at ṅ and "vedanā" splits at ā — the Pāli
        // vocabulary must fold to ASCII like the Latin-1 set does.
        'Ā' | 'ā' => "a",
        'Ī' | 'ī' => "i",
        'Ū' | 'ū' => "u",
        'Ṝ' | 'ṛ' => "r",
        'Ḷ' | 'ḷ' => "l",
        'Ṅ' | 'ṅ' => "n",
        'Ṇ' | 'ṇ' => "n",
        'Ṁ' | 'ṁ' => "m",
        'Ṃ' | 'ṃ' => "m",
        'Ṭ' | 'ṭ' => "t",
        'Ḍ' | 'ḍ' => "d",
        'Ḥ' | 'ḥ' => "h",
        _ => return None,
    })
}

/// Fold-only tokenizer: hyphen-strip → ASCII fold → lowercase, splitting on
/// non-alphanumeric output. This is the **key builder** — FST dictionary
/// keys, the tagger's input walk, and SKG entity keys — where spelling
/// variants must collapse to one key ("Zürich" ≡ "Zurich" ≡ "zürich"),
/// rather than carry both forms like [`tokenize`] does for retrieval.
/// CJK yields no tokens here (upstream behavior).
pub fn tokenize_folds(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut cur: Option<Token> = None;
    for (start, c) in text.char_indices() {
        let end = start + c.len_utf8();
        if is_hyphen(c) {
            continue;
        }
        let folded: Option<Vec<u8>> = if c.is_ascii_alphanumeric() {
            Some(vec![(c as u8).to_ascii_lowercase()])
        } else {
            fold_latin(c).map(|s| s.bytes().collect())
        };
        match folded {
            Some(bytes) => match cur.as_mut() {
                Some(t) => {
                    t.bytes.extend_from_slice(&bytes);
                    t.end = end;
                }
                None => {
                    cur = Some(Token { bytes, start, end });
                }
            },
            None => {
                if let Some(t) = cur.take() {
                    tokens.push(t);
                }
            }
        }
    }
    if let Some(t) = cur {
        tokens.push(t);
    }
    tokens
}

/// Whether `c` is a CJK ideograph/syllable (Han, Hiragana, Katakana,
/// Hangul). These carry meaning per character and have no space
/// boundaries, so they are indexed as unigrams plus overlapping bigrams —
/// the standard CJK-analyzer fallback when no dictionary segmenter is
/// available. This is **segmentation for same-language corpora**; it does
/// not translate across languages (multilingual parity needs the
/// embedder-space decision, plan §3.1).
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}'      // CJK Unified Ideographs Extension A
        | '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{F900}'..='\u{FAFF}'   // CJK Compatibility Ideographs
        | '\u{3040}'..='\u{309F}'   // Hiragana
        | '\u{30A0}'..='\u{30FF}'   // Katakana
        | '\u{31F0}'..='\u{31FF}'   // Katakana Phonetic Extensions
        | '\u{FF66}'..='\u{FF9D}'   // Halfwidth Katakana
        | '\u{AC00}'..='\u{D7AF}'   // Hangul Syllables
        | '\u{20000}'..='\u{2A6DF}' // CJK Extension B
    )
}

/// Tokenize with the analyzer chain (hyphen-strip → ASCII fold → lowercase),
/// extended with two exact-text-preserving behaviors:
///
/// 1. **CJK runs** become unigram + overlapping bigram tokens (Lucene's
///    CJK-analyzer fallback). Before this, every CJK char hit the
///    separator branch and Chinese text produced ZERO tokens — BM25 was
///    blind to it.
/// 2. **Dual-form Pāli/diacritic words**: a token containing non-ASCII
///    (e.g. `saṅkhāra`) emits its ASCII-folded form (`sankhara`) AND its
///    original diacritic form (`saṅkhāra`) — case-folded, bytes otherwise
///    verbatim — so a diacritic query finds ASCII text (via the fold) and
///    a diacritic corpus stays precisely searchable (via the original
///    form). Pure-ASCII words emit one token, exactly as before.
///
/// Index side and query side both pass through this one function, so the
/// token streams always agree.
pub fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut cur: Option<Token> = None;

    // Flush the in-progress Latin token, emitting the original-form twin
    // for diacritic words. `text` keeps the exact source bytes.
    fn flush(
        cur: &mut Option<Token>,
        tokens: &mut Vec<Token>,
        text: &str,
    ) {
        if let Some(tok) = cur.take() {
            let has_non_ascii = tok.bytes.is_empty() || text
                [tok.start..tok.end]
                .contains(|c: char| !c.is_ascii());
            if has_non_ascii {
                if let Some(raw) = text.get(tok.start..tok.end) {
                    let lowered = raw.to_lowercase();
                    // Emit the folded form first, then the original form.
                    tokens.push(tok.clone());
                    tokens.push(Token {
                        bytes: lowered.into_bytes(),
                        start: tok.start,
                        end: tok.end,
                    });
                    return;
                }
            }
            tokens.push(tok);
        }
    }

    for (ci, &(start, c)) in chars.iter().enumerate() {
        let end = start + c.len_utf8();
        if is_cjk(c) {
            flush(&mut cur, &mut tokens, text);
            // Unigram (exact original bytes — CJK chars need no folding).
            tokens.push(Token {
                bytes: c.to_string().into_bytes(),
                start,
                end,
            });
            // Overlapping bigram when the next char is also CJK.
            if let Some(&(_, next)) = chars.get(ci + 1) {
                if is_cjk(next) {
                    let mut bytes = c.to_string().into_bytes();
                    bytes.extend_from_slice(next.to_string().as_bytes());
                    tokens.push(Token {
                        bytes,
                        start,
                        end: end + next.len_utf8(),
                    });
                }
            }
        } else if is_hyphen(c) {
            // Hyphens are stripped, not separators: "sw-lucene" ≡ "swlucene".
            continue;
        } else if c.is_ascii_alphanumeric() {
            match cur.as_mut() {
                Some(t) => {
                    t.bytes.push((c as u8).to_ascii_lowercase());
                    t.end = end;
                }
                None => {
                    cur = Some(Token {
                        bytes: vec![(c as u8).to_ascii_lowercase()],
                        start,
                        end,
                    });
                }
            }
        } else if let Some(folded) = fold_latin(c) {
            for b in folded.bytes() {
                match cur.as_mut() {
                    Some(t) => {
                        t.bytes.push(b);
                        t.end = end;
                    }
                    None => {
                        cur = Some(Token {
                            bytes: vec![b],
                            start,
                            end,
                        });
                    }
                }
            }
        } else {
            // Whitespace / punctuation / unhandled — token separator.
            flush(&mut cur, &mut tokens, text);
        }
    }
    flush(&mut cur, &mut tokens, text);
    tokens
}

/// Build the canonical FST key bytes for a phrase: folded tokens joined
/// with `SEP` (0x1E).
fn normalize_key(s: &str) -> Vec<u8> {
    let toks = tokenize_folds(s);
    let mut out: Vec<u8> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        out.extend_from_slice(&t.bytes);
    }
    out
}

/// Java's `deriveOutput`: uppercase every char then strip everything that
/// isn't `A-Z` or `0-9`. Latin folding is applied first so "Zürich" →
/// "ZURICH".
pub fn derive_output(phrase: &str) -> String {
    let mut out = String::with_capacity(phrase.len());
    for c in phrase.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if let Some(folded) = fold_latin(c) {
            for fc in folded.chars() {
                out.push(fc.to_ascii_uppercase());
            }
        }
    }
    out
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string()
}

/// Minimal RFC-4180-ish CSV line parser. Supports quoted fields, embedded
/// commas, and `""` escapes — same scope as Java's `splitCsv` plus quote
/// handling.
pub fn parse_csv_line(line: &str) -> Vec<String> {
    let line = line.trim_end_matches('\r');
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' && cur.is_empty() {
            in_quotes = true;
        } else if c == ',' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// UUID v4 string (RFC 4122). Uses `/dev/urandom` when available, falls
/// back to a high-resolution timestamp otherwise.
pub fn uuid_v4() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    } else {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let lo = (nanos as u64).to_le_bytes();
        let hi = ((nanos >> 64) as u64).to_le_bytes();
        buf[..8].copy_from_slice(&lo);
        buf[8..].copy_from_slice(&hi);
    }
    buf[6] = (buf[6] & 0x0F) | 0x40;
    buf[8] = (buf[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        buf[0], buf[1], buf[2], buf[3],
        buf[4], buf[5],
        buf[6], buf[7],
        buf[8], buf[9],
        buf[10], buf[11], buf[12], buf[13], buf[14], buf[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tagger {
        Tagger::build(vec![
            Entry::new("New York", "CITY", "geo:nyc"),
            Entry::new("New York City", "CITY", "geo:nyc"),
            Entry::new("San Francisco", "CITY", "geo:sf"),
            Entry::new("Apache Lucene", "PRODUCT", "sw:lucene"),
            Entry::new("Lucene", "PRODUCT", "sw:lucene"),
            Entry::new("Zürich", "CITY", "geo:zur"),
            Entry::new("Zurich", "CITY", "geo:zur"),
        ])
        .unwrap()
    }

    #[test]
    fn longest_match_wins() {
        let t = sample();
        let tags = t.tag("I love New York City");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].surface, "New York City");
        assert_eq!(tags[0].id, "geo:nyc");
        assert_eq!(tags[0].output, "NEWYORKCITY");
    }

    #[test]
    fn ascii_folding_zurich() {
        let t = sample();
        // Dictionary has both "Zürich" and "Zurich" with the same id —
        // they collapse to one record (matches Java's
        // LinkedHashSet<id+type+output> dedup), and that record matches
        // a plain "zurich" in the input.
        let tags = t.tag("visit zurich tomorrow");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].surface, "zurich");
        assert_eq!(tags[0].output, "ZURICH");
        assert_eq!(tags[0].id, "geo:zur");
    }

    #[test]
    fn hyphen_stripping() {
        // Build a tiny dict and check that "sw-lucene" in input matches
        // dictionary phrase "swlucene".
        let t = Tagger::build(vec![Entry::new("swlucene", "P", "sw:lucene")]).unwrap();
        let tags = t.tag("we use sw-lucene here");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].surface, "sw-lucene");
        assert_eq!(tags[0].output, "SWLUCENE");
    }

    #[test]
    fn synonyms_collapse_to_one_span() {
        // Two records that analyze to the same FST key (here both fold
        // to "zurich") share a node — both records are emitted at the
        // matched span. Different ids ⇒ not deduped.
        let t = Tagger::build(vec![
            Entry::new("Zürich", "CITY", "geo:zur-de"),
            Entry::new("Zurich", "CITY", "geo:zur-en"),
        ])
        .unwrap();
        let tags = t.tag("hello zurich");
        assert_eq!(tags.len(), 2);
        let ids: Vec<&str> = tags.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"geo:zur-de") && ids.contains(&"geo:zur-en"));
        assert_eq!(tags[0].start, tags[1].start);
        assert_eq!(tags[0].end, tags[1].end);
    }

    #[test]
    fn csv_line_parses_quotes_and_commas() {
        let cells = parse_csv_line(r#""Smith, John",42,"He said ""hi""""#);
        assert_eq!(cells, vec!["Smith, John", "42", r#"He said "hi""#]);
    }

    #[test]
    fn loads_data_dir_with_action_column() {
        let tmp = std::env::temp_dir().join("lume_test_data_v2");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("intent.csv"),
            "intent,action,response\nbuy,BUY,Buying\nview,VIEW,Viewing\n",
        )
        .unwrap();

        let tagger = Tagger::from_data_dir(&tmp).unwrap();
        let tags = tagger.tag("I want to buy a thing");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].surface, "buy");
        assert_eq!(tags[0].kind, "intent");
        assert_eq!(tags[0].output, "BUY", "action column became output");
        assert_eq!(tags[0].id.len(), 36, "UUID v4 id");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn regex_and_fst_hybrid_tagging() {
        let tagger = Tagger::build(vec![
            Entry::new("SKU", "TAG", "static_sku"),
            Entry::new("SKU-\\d{5}", "PRODUCT", "regex_sku"),
            Entry::new("MC-\\d{4}", "BOOK", "regex_book"),
        ])
        .unwrap();

        // Standard tagging (longest match wins):
        // "SKU-12345" matches both static "SKU" (5..8) and regex "SKU-\d{5}" (5..14).
        // "SKU-\d{5}" is longer, so it should cleanly win and suppress the static match!
        let tags = tagger.tag("item SKU-12345 and MC-9876 are listed");
        assert_eq!(tags.len(), 2);
        
        assert_eq!(tags[0].surface, "SKU-12345");
        assert_eq!(tags[0].id, "regex_sku");
        assert_eq!(tags[0].kind, "PRODUCT");

        assert_eq!(tags[1].surface, "MC-9876");
        assert_eq!(tags[1].id, "regex_book");
        assert_eq!(tags[1].kind, "BOOK");
    }

    #[test]
    fn latin_tokenization_is_unchanged() {
        // Pre-CJK behavior must be bit-identical: fold, lowercase,
        // hyphen-strip, separator split.
        let toks: Vec<Vec<u8>> = tokenize("Zurich Zürich sw-lucene café 42")
            .into_iter()
            .map(|t| t.bytes)
            .collect();
        assert_eq!(
            toks,
            vec![
                b"zurich".to_vec(),
                b"zurich".to_vec(),
                "zürich".as_bytes().to_vec(),
                b"swlucene".to_vec(),
                b"cafe".to_vec(),
                "café".as_bytes().to_vec(),
                b"42".to_vec(),
            ],
            "folded form first, original-form twin second for diacritic words"
        );
    }

    #[test]
    fn cjk_text_produces_unigrams_and_bigrams() {
        let toks = tokenize("苹果电脑");
        let surfaces: Vec<(usize, usize, String)> = toks
            .iter()
            .map(|t| (t.start, t.end, String::from_utf8_lossy(&t.bytes).into_owned()))
            .collect();
        // Unigrams in order, each followed by its overlapping bigram.
        assert_eq!(
            surfaces,
            vec![
                (0, 3, "苹".into()),
                (0, 6, "苹果".into()),
                (3, 6, "果".into()),
                (3, 9, "果电".into()),
                (6, 9, "电".into()),
                (6, 12, "电脑".into()),
                (9, 12, "脑".into()),
            ]
        );
    }

    #[test]
    fn pure_chinese_query_shares_tokens_with_chinese_text() {
        // The parity bug this fixes: both sides tokenize through one
        // function, so a zh query hits a zh corpus lexically.
        let query_tokens: std::collections::HashSet<Vec<u8>> =
            tokenize("苹果电脑").into_iter().map(|t| t.bytes).collect();
        let doc_tokens: std::collections::HashSet<Vec<u8>> = tokenize(
            "史蒂夫·乔布斯在 1976 年与合伙人共同创立了苹果电脑公司，重新定义了个人计算机。",
        )
        .into_iter()
        .map(|t| t.bytes)
        .collect();
        let shared: usize = query_tokens.intersection(&doc_tokens).count();
        assert!(shared >= 4, "expected shared unigram/bigram tokens, got {shared}");
    }

    #[test]
    fn pali_diacritic_and_ascii_forms_mutually_retrievable() {
        // ASCII query finds diacritic text via the folded form...
        let q = tokenize("sankhara");
        let doc = tokenize("saṅkhāra formations accumulate");
        let q_set: std::collections::HashSet<Vec<u8>> = q.into_iter().map(|t| t.bytes).collect();
        assert!(doc.iter().any(|t| q_set.contains(&t.bytes)));
        // ...and the diacritic original stays precisely searchable.
        let surfaces: Vec<Vec<u8>> = doc.into_iter().map(|t| t.bytes).collect();
        assert!(surfaces.iter().any(|b| b.as_slice() == "saṅkhāra".as_bytes()));
    }

    #[test]
    fn cjk_and_latin_mix_tokenizes_both() {
        let toks: Vec<Vec<u8>> = tokenize("Macintosh 苹果")
            .into_iter()
            .map(|t| t.bytes)
            .collect();
        assert_eq!(
            toks,
            vec![
                b"macintosh".to_vec(),
                "苹".as_bytes().to_vec(),
                "苹果".as_bytes().to_vec(),
                "果".as_bytes().to_vec(),
            ]
        );
    }
}
