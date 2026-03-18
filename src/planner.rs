use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, bail};

/// Lightweight query planner that normalizes user text into executable SQL.
/// When AGENT_KEY is set, rewrites freeform queries via Claude API on a
/// background thread so the main loop never blocks on network I/O.
/// Falls back to rule-based rewriting when no key is set or LLM call fails.
#[derive(Debug, Clone)]
pub struct Planner {
    pub agent_key: Option<String>,
}

const PLANNER_SYSTEM: &str = "\
You rewrite freeform text into ferricula SQL queries.
Available syntax:
  SELECT id FROM docs WHERE field = 'value'
  AND, OR, NOT boolean operators
  UNION, INTERSECT, EXCEPT set operations
  vector_topk_cosine(embed('search text'), k)
  vector_topk_l2(embed('search text'), k)

Use embed('text') for semantic search — NEVER pass raw vectors.
For freeform search queries, use: SELECT id FROM docs WHERE vector_topk_cosine(embed('the query'), 10)
For exact tag matches, use: SELECT id FROM docs WHERE field = 'value'

Tag fields come from memory tags (text, channel, type, path, etc).
Respond with ONLY the SQL query, nothing else.";

/// Result of an async planner rewrite.
pub struct PlannerResult {
    /// The original input text.
    pub input: String,
    /// The rewritten SQL (LLM or rule-based fallback).
    pub sql: String,
    /// Whether the LLM was used (vs rule-based fallback).
    pub llm_used: bool,
}

impl Planner {
    pub fn new(agent_key: Option<String>) -> Self {
        Self { agent_key }
    }

    /// Check if input needs LLM rewrite (not already SQL, key available).
    pub fn needs_llm(&self, input: &str) -> bool {
        if self.agent_key.is_none() {
            return false;
        }
        let lower = input.trim().to_lowercase();
        !lower.starts_with("select") && !lower.starts_with("with")
    }

    /// Synchronous rewrite — rule-based only, never blocks on network.
    /// Used for SQL passthrough and when no AGENT_KEY is set.
    pub fn rewrite_query_sync(&self, input: &str) -> Result<String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            bail!("empty query");
        }

        let lower = trimmed.to_lowercase();
        if lower.starts_with("select") || lower.starts_with("with") {
            return Ok(trimmed.to_string());
        }

        self.rule_based_rewrite(trimmed)
    }

    /// Original synchronous rewrite — tries LLM then falls back to rules.
    /// Used by REPL mode where blocking is acceptable.
    pub fn rewrite_query(&self, input: &str) -> Result<String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            bail!("empty query");
        }

        let lower = trimmed.to_lowercase();
        if lower.starts_with("select") || lower.starts_with("with") {
            return Ok(trimmed.to_string());
        }

        if let Some(ref key) = self.agent_key {
            if let Some(rewritten) = llm_rewrite_blocking(key, trimmed) {
                let rl = rewritten.trim().to_lowercase();
                if rl.starts_with("select") || rl.starts_with("with") {
                    return Ok(rewritten.trim().to_string());
                }
            }
        }

        self.rule_based_rewrite(trimmed)
    }

    /// Spawn LLM rewrite on a background thread. Returns a receiver that
    /// delivers the PlannerResult when the LLM responds (or times out).
    /// The main loop polls this receiver without blocking.
    pub fn spawn_llm_rewrite(&self, input: &str) -> mpsc::Receiver<PlannerResult> {
        let (tx, rx) = mpsc::channel();
        let key = self.agent_key.clone().unwrap_or_default();
        let input_owned = input.trim().to_string();
        let fallback = self.rule_based_rewrite(&input_owned)
            .unwrap_or_else(|_| format!(
                "SELECT id FROM docs WHERE vector_topk_cosine(embed('{}'), 10)",
                input_owned.replace('\'', "''")
            ));

        std::thread::Builder::new()
            .name("planner-llm".into())
            .spawn(move || {
                let (sql, llm_used) = if !key.is_empty() {
                    match llm_rewrite_blocking(&key, &input_owned) {
                        Some(rewritten) => {
                            let rl = rewritten.trim().to_lowercase();
                            if rl.starts_with("select") || rl.starts_with("with") {
                                (rewritten.trim().to_string(), true)
                            } else {
                                eprintln!("[planner] LLM returned non-SQL, using rule-based");
                                (fallback, false)
                            }
                        }
                        None => {
                            eprintln!("[planner] LLM failed, using rule-based");
                            (fallback, false)
                        }
                    }
                } else {
                    (fallback, false)
                };
                let _ = tx.send(PlannerResult {
                    input: input_owned,
                    sql,
                    llm_used,
                });
            })
            .expect("spawn planner-llm thread");

        rx
    }

    /// Rule-based query rewrite — no external calls.
    fn rule_based_rewrite(&self, input: &str) -> Result<String> {
        let lower = input.to_lowercase();

        // Vector literal — wrap in topk
        if lower.starts_with('[') && lower.ends_with(']') {
            return Ok(format!(
                "SELECT id FROM docs WHERE vector_topk_cosine('{input}', 10)"
            ));
        }

        // Already contains vector function call → pass through as WHERE clause
        if lower.contains("vector_topk") {
            return Ok(format!("SELECT id FROM docs WHERE {input}"));
        }

        // Contains = → tag query (possibly with boolean operators)
        if lower.contains(" = ") || lower.contains("='") {
            return Ok(format!("SELECT id FROM docs WHERE {input}"));
        }

        // Default: semantic search via embed()
        Ok(format!(
            "SELECT id FROM docs WHERE vector_topk_cosine(embed('{}'), 10)",
            input.replace('\'', "''")
        ))
    }
}

/// Blocking LLM rewrite — runs on caller's thread.
/// Used by REPL and by the spawned planner thread.
fn llm_rewrite_blocking(api_key: &str, input: &str) -> Option<String> {
    use std::io::{Read, Write};
    use std::sync::Arc;

    let body = serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 256,
        "system": PLANNER_SYSTEM,
        "messages": [{"role": "user", "content": input}]
    })
    .to_string();

    // TLS connection to api.anthropic.com:443
    let root_store = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let server_name: rustls::pki_types::ServerName =
        "api.anthropic.com".try_into().ok()?;

    let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name).ok()?;
    use std::net::ToSocketAddrs;
    let sock_addr = "api.anthropic.com:443".to_socket_addrs().ok()?.next()?;
    let mut sock = std::net::TcpStream::connect_timeout(
        &sock_addr,
        Duration::from_secs(5),
    )
    .ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;

    let mut tls = rustls::Stream::new(&mut conn, &mut sock);

    let request = format!(
        "POST /v1/messages HTTP/1.0\r\n\
         Host: api.anthropic.com\r\n\
         Content-Type: application/json\r\n\
         x-api-key: {api_key}\r\n\
         anthropic-version: 2023-06-01\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    tls.write_all(request.as_bytes()).ok()?;
    tls.flush().ok()?;

    let mut response = String::new();
    tls.read_to_string(&mut response).ok()?;

    // Split headers from body
    let body_start = response.find("\r\n\r\n").map(|i| i + 4)?;
    let resp_body = &response[body_start..];

    // Extract text from Claude response: {"content":[{"type":"text","text":"..."}]}
    let val: serde_json::Value = serde_json::from_str(resp_body).ok()?;
    val.get("content")?
        .as_array()?
        .first()?
        .get("text")?
        .as_str()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_sql() {
        let p = Planner::new(Some("test".into()));
        let r = p.rewrite_query("SELECT id FROM docs WHERE text = 'hello'").unwrap();
        assert_eq!(r, "SELECT id FROM docs WHERE text = 'hello'");
    }

    #[test]
    fn passthrough_with() {
        let p = Planner::new(Some("test".into()));
        let r = p.rewrite_query("WITH x AS (SELECT 1) SELECT * FROM x").unwrap();
        assert_eq!(r, "WITH x AS (SELECT 1) SELECT * FROM x");
    }

    #[test]
    fn empty_query() {
        let p = Planner::new(Some("test".into()));
        assert!(p.rewrite_query("").is_err());
        assert!(p.rewrite_query("   ").is_err());
    }

    #[test]
    fn vector_literal() {
        let p = Planner::new(None);
        let r = p.rewrite_query("[0.1,0.2,0.3]").unwrap();
        assert!(r.contains("vector_topk_cosine"));
    }

    #[test]
    fn tag_query() {
        let p = Planner::new(None);
        let r = p.rewrite_query("channel = 'hearing'").unwrap();
        assert_eq!(r, "SELECT id FROM docs WHERE channel = 'hearing'");
    }

    #[test]
    fn boolean_operators() {
        let p = Planner::new(None);
        let r = p.rewrite_query("channel = 'hearing' AND text = 'test'").unwrap();
        assert!(r.starts_with("SELECT id FROM docs WHERE"));
    }

    #[test]
    fn freeform_text() {
        let p = Planner::new(None);
        let r = p.rewrite_query("memories about testing").unwrap();
        assert!(r.contains("embed('memories about testing')"));
    }

    #[test]
    fn no_key_passthrough() {
        let p = Planner::new(None);
        let r = p.rewrite_query("hello world").unwrap();
        assert!(r.contains("vector_topk_cosine(embed("));
    }

    #[test]
    fn escapes_quotes() {
        let p = Planner::new(None);
        let r = p.rewrite_query("it's a test").unwrap();
        assert!(r.contains("it''s a test"));
    }

    #[test]
    fn sync_rewrite_passthrough() {
        let p = Planner::new(Some("test".into()));
        let r = p.rewrite_query_sync("SELECT id FROM docs WHERE text = 'hello'").unwrap();
        assert_eq!(r, "SELECT id FROM docs WHERE text = 'hello'");
    }

    #[test]
    fn needs_llm_detection() {
        let p = Planner::new(Some("test".into()));
        assert!(!p.needs_llm("SELECT id FROM docs WHERE text = 'hello'"));
        assert!(p.needs_llm("memories about testing"));

        let p_nokey = Planner::new(None);
        assert!(!p_nokey.needs_llm("memories about testing"));
    }
}
