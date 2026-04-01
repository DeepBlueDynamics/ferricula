//! Semantic fidelity verification via vec2text inversion.
//!
//! After consolidation warps a memory's vector, invert it back to text
//! via shivvr and compare with the original text tag. This measures whether
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

/// Call shivvr to embed text, return the embedding vector.
/// Applies Pali translation layer before embedding so both vocabularies
/// (Abhidhamma Pali + computational English) land in the same vector space.
pub fn embed_text(shivvr_url: &str, text: &str) -> Option<Vec<f32>> {
    let expanded = crate::pali::expand(text);
    let body = serde_json::json!({ "text": expanded }).to_string();
    let response = shivvr_post(shivvr_url, "/memory/_mcp/ingest", &body)?;
    let val: serde_json::Value = serde_json::from_str(&response).ok()?;
    // Top-level embedding or nested in chunks[0]
    if let Some(emb) = val.get("embedding").and_then(|v| v.as_array()) {
        return Some(
            emb.iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect(),
        );
    }
    val.get("chunks")?
        .as_array()?
        .first()?
        .get("embedding")?
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect()
        })
}

/// Call shivvr `/invert` with a vector, return approximate text.
pub fn invert_vector(shivvr_url: &str, vector: &[f32]) -> Option<String> {
    let body = serde_json::json!({ "embedding": vector }).to_string();
    let response = shivvr_post(shivvr_url, "/invert", &body)?;
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

/// Check if shivvr is reachable.
pub fn shivvr_available(shivvr_url: &str) -> bool {
    shivvr_get(shivvr_url, "/health").is_some()
}

/// Full inversion check for a memory.
/// Needs the vector and original text from the engine.
pub fn check_inversion_with_data(
    shivvr_url: &str,
    memory_id: u32,
    original_text: &str,
    vector: &[f32],
) -> Option<InversionCheck> {
    let inverted_text = invert_vector(shivvr_url, vector)?;
    let quality = text_similarity(original_text, &inverted_text);
    Some(InversionCheck {
        memory_id,
        original_text: original_text.to_string(),
        inverted_text,
        quality,
    })
}

// --- HTTP/HTTPS helpers ---

struct ParsedUrl {
    host: String,
    port: u16,
    tls: bool,
}

fn parse_url(url: &str) -> Option<ParsedUrl> {
    let tls = url.starts_with("https://");
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .trim_end_matches('/');

    if let Some(colon) = stripped.rfind(':') {
        let h = &stripped[..colon];
        let p = stripped[colon + 1..].parse::<u16>().ok()?;
        Some(ParsedUrl { host: h.to_string(), port: p, tls })
    } else {
        let port = if tls { 443 } else { 8080 };
        Some(ParsedUrl { host: stripped.to_string(), port, tls })
    }
}

/// Send an HTTP request over plain TCP or TLS, return response body.
fn http_request(base_url: &str, method: &str, path: &str, body: Option<&str>) -> Option<String> {
    let parsed = parse_url(base_url)?;
    let addr = format!("{}:{}", parsed.host, parsed.port);

    let request = if let Some(body) = body {
        format!(
            "{method} {path} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            parsed.host, body.len()
        )
    } else {
        format!(
            "{method} {path} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            parsed.host
        )
    };

    // Resolve hostname to SocketAddr — prefer IPv4 (Docker containers often lack IPv6)
    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = addr.to_socket_addrs().ok()?.collect();
    let sock_addr = addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()).copied()?;

    let mut response = String::new();

    if parsed.tls {
        use std::sync::Arc;
        let root_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let host_owned = parsed.host.clone();
        let server_name: rustls::pki_types::ServerName =
            host_owned.try_into().ok()?;
        let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name).ok()?;
        let mut sock = TcpStream::connect_timeout(&sock_addr, Duration::from_millis(5000)).ok()?;
        sock.set_read_timeout(Some(Duration::from_millis(15000))).ok()?;
        let mut tls = rustls::Stream::new(&mut conn, &mut sock);
        tls.write_all(request.as_bytes()).ok()?;
        tls.flush().ok()?;
        // Read in chunks — HTTP/1.0 servers close without TLS close_notify,
        // which makes read_to_string return Err. Read what we can instead.
        let mut buf = [0u8; 4096];
        loop {
            match tls.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => response.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionAborted => break,
                Err(_) => break,
            }
        }
    } else {
        let mut stream = TcpStream::connect_timeout(&sock_addr, Duration::from_millis(2000)).ok()?;
        stream.set_read_timeout(Some(Duration::from_millis(5000))).ok()?;
        stream.write_all(request.as_bytes()).ok()?;
        stream.flush().ok()?;
        stream.read_to_string(&mut response).ok()?;
    }

    let body_start = response.find("\r\n\r\n").map(|i| i + 4)?;
    Some(response[body_start..].to_string())
}

fn shivvr_post(base_url: &str, path: &str, body: &str) -> Option<String> {
    http_request(base_url, "POST", path, Some(body))
}

fn shivvr_get(base_url: &str, path: &str) -> Option<String> {
    http_request(base_url, "GET", path, None)
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
    fn parse_url_http() {
        let p = parse_url("http://localhost:8080").unwrap();
        assert_eq!(p.host, "localhost");
        assert_eq!(p.port, 8080);
        assert!(!p.tls);
    }

    #[test]
    fn parse_url_http_no_port() {
        let p = parse_url("http://localhost").unwrap();
        assert_eq!(p.host, "localhost");
        assert_eq!(p.port, 8080);
        assert!(!p.tls);
    }

    #[test]
    fn parse_url_https() {
        let p = parse_url("https://shivvr.nuts.services").unwrap();
        assert_eq!(p.host, "shivvr.nuts.services");
        assert_eq!(p.port, 443);
        assert!(p.tls);
    }

    #[test]
    fn parse_url_https_with_port() {
        let p = parse_url("https://example.com:8443").unwrap();
        assert_eq!(p.host, "example.com");
        assert_eq!(p.port, 8443);
        assert!(p.tls);
    }
}
