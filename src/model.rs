use serde::{Deserialize, Serialize};

/// Reference metadata for a memory — bibliographic or web source coordinates.
///
/// Works for books (`book`, `author`, `volume`, `chapter`, `page`, `section`)
/// and web pages / files (`book` = page title, `url` = web URL, `filename` =
/// local path). Numeric fields (`volume`, `chapter`, `page`) are stored as
/// `u32` for range queries; all fields are flattened to string tags via
/// [`MemoryRef::to_tags`]. When both `book` and `chapter` are present a
/// namespaced `chapter_id` tag is also emitted (e.g.
/// `"the_count_of_monte_cristo_chapter_5"`) for unique cross-book chapter
/// identification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MemoryRef {
    /// Title of the book or webpage.
    pub book: Option<String>,
    pub author: Option<String>,
    pub volume: Option<u32>,
    pub chapter: Option<u32>,
    pub page: Option<u32>,
    pub section: Option<String>,
    /// Web URL for this source (use `filename` for local files).
    pub url: Option<String>,
    /// Local file path or filename for this source (use `url` for web).
    pub filename: Option<String>,
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

impl MemoryRef {
    pub fn is_empty(&self) -> bool {
        self.book.is_none()
            && self.author.is_none()
            && self.volume.is_none()
            && self.chapter.is_none()
            && self.page.is_none()
            && self.section.is_none()
            && self.url.is_none()
            && self.filename.is_none()
    }

    /// Flatten reference fields to `(tag_key, tag_value)` pairs for the
    /// bitmap tag index. Numeric fields stringify as decimal; when both
    /// `book` and `chapter` are set, a namespaced `chapter_id` tag is
    /// also emitted (e.g. `"count_of_monte_cristo_chapter_5"`).
    pub fn to_tags(&self) -> Vec<(String, String)> {
        let mut tags = Vec::new();
        if let Some(v) = &self.book {
            tags.push(("book".into(), v.clone()));
        }
        if let Some(v) = &self.author {
            tags.push(("author".into(), v.clone()));
        }
        if let Some(v) = self.volume {
            tags.push(("volume".into(), v.to_string()));
        }
        if let Some(v) = self.chapter {
            tags.push(("chapter".into(), v.to_string()));
            if let Some(b) = &self.book {
                tags.push(("chapter_id".into(), format!("{}_chapter_{}", slugify(b), v)));
            }
        }
        if let Some(v) = self.page {
            tags.push(("page".into(), v.to_string()));
        }
        if let Some(v) = &self.section {
            tags.push(("section".into(), v.clone()));
        }
        if let Some(v) = &self.url {
            tags.push(("url".into(), v.clone()));
        }
        if let Some(v) = &self.filename {
            tags.push(("filename".into(), v.clone()));
        }
        tags
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub id: u32,
    pub tags: std::collections::BTreeMap<String, String>,
    pub vector: Vec<f32>,
    /// Optional reference metadata (book, author, page, etc.). Backward
    /// compatible with persisted rows that predate this field via
    /// `#[serde(default)]`.
    #[serde(default)]
    pub refs: Option<MemoryRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Cosine,
    L2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub id: u32,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub ids: Vec<u32>,
}
