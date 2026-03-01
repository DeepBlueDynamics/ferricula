//! Semantic fidelity verification via vec2text inversion.
//!
//! After consolidation warps a memory's vector, invert it back to text
//! via chonk and compare with the original text tag. This measures whether
//! the manifold warping preserved semantic content.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Result of an inversion quality check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InversionCheck {
    pub memory_id: u32,
    pub original_text: String,
    pub inverted_text: String,
    pub quality: f32,
}

/// Call chonk `/invert` with a vector, return approximate text.
pub fn invert_vector(chonk_url: &str, vector: &[f32]) -> Option<String> {
    let body = serde_json::json!({ "embedding": vector }).to_string();
    let response = chonk_post(chonk_url, "/invert", &body)?;
    let val: serde_json::Value = serde_json::from_str(&response).ok()?;
    val.get("text")
        .or_else(|| val.get("hypothesis"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Jaccard similarity on whitespace-tokenized word sets.
pub fn text_similarity(a: &str, b: &str) -> f32 {
    let set_a: HashSet<&str> = a
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    let set_b: HashSet<&str> = b
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();

    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }
    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    intersection as f32 / union as f32
}

/// Check if chonk is reachable.
pub fn chonk_available(chonk_url: &str) -> bool {
    chonk_get(chonk_url, "/health").is_some()
}

/// Full inversion check for a memory.
/// Needs the vector and original text from the engine.
pub fn check_inversion_with_data(
    chonk_url: &str,
    memory_id: u32,
    original_text: &str,
    vector: &[f32],
) -> Option<InversionCheck> {
    let inverted_text = invert_vector(chonk_url, vector)?;
    let quality = text_similarity(original_text, &inverted_text);
    Some(InversionCheck {
        memory_id,
        original_text: original_text.to_string(),
        inverted_text,
        quality,
    })
}

// --- HTTP helpers (raw TcpStream, same pattern as clock.rs) ---

fn parse_host_port(url: &str) -> Option<(String, u16)> {
    let stripped = url.strip_prefix("http://").unwrap_or(url);
    if let Some(colon) = stripped.rfind(':') {
        let h = &stripped[..colon];
        let p = stripped[colon + 1..]
            .trim_end_matches('/')
            .parse::<u16>()
            .ok()?;
        Some((h.to_string(), p))
    } else {
        Some((stripped.trim_end_matches('/').to_string(), 8080))
    }
}

fn chonk_post(base_url: &str, path: &str, body: &str) -> Option<String> {
    let (host, port) = parse_host_port(base_url)?;
    let addr = format!("{host}:{port}");
    let mut stream =
        TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(2000)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(5000)))
        .ok()?;

    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    let body_start = response.find("\r\n\r\n").map(|i| i + 4)?;
    Some(response[body_start..].to_string())
}

fn chonk_get(base_url: &str, path: &str) -> Option<String> {
    let (host, port) = parse_host_port(base_url)?;
    let addr = format!("{host}:{port}");
    let mut stream =
        TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(1000)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(2000)))
        .ok()?;

    let request = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    let body_start = response.find("\r\n\r\n").map(|i| i + 4)?;
    Some(response[body_start..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_similarity_identical() {
        assert!((text_similarity("hello world", "hello world") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn text_similarity_disjoint() {
        assert!((text_similarity("hello world", "foo bar") - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn text_similarity_partial() {
        // "hello world" vs "hello there" -> intersection={"hello"}, union={"hello","world","there"}
        let sim = text_similarity("hello world", "hello there");
        assert!((sim - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn text_similarity_empty() {
        assert!((text_similarity("", "") - 1.0).abs() < f32::EPSILON);
        assert!((text_similarity("hello", "") - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn text_similarity_ignores_punctuation() {
        let sim = text_similarity("hello, world!", "hello world");
        assert!((sim - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_host_port_standard() {
        let (h, p) = parse_host_port("http://localhost:8080").unwrap();
        assert_eq!(h, "localhost");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_no_port() {
        let (h, p) = parse_host_port("http://localhost").unwrap();
        assert_eq!(h, "localhost");
        assert_eq!(p, 8080);
    }
}
