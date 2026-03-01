use anyhow::{Result, bail};

/// Lightweight query planner that normalizes user text into executable SQL.
/// When AGENT_KEY is set, rewrites freeform queries via Claude API.
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
  vector_topk_cosine('[1,0,0]', k)
  vector_topk_l2('[1,0,0]', k)

Tag fields come from memory tags (text, channel, type, path, etc).
Respond with ONLY the SQL query, nothing else.";

impl Planner {
    pub fn new(agent_key: Option<String>) -> Self {
        Self { agent_key }
    }

    /// Rewrite freeform input into a canonical SQL string.
    /// With AGENT_KEY: tries LLM rewrite first, falls back to rules.
    /// Without AGENT_KEY: rule-based only.
    pub fn rewrite_query(&self, input: &str) -> Result<String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            bail!("empty query");
        }

        // Already SQL? Pass through.
        let lower = trimmed.to_lowercase();
        if lower.starts_with("select") || lower.starts_with("with") {
            return Ok(trimmed.to_string());
        }

        // If we have an agent key, try LLM rewrite
        if let Some(ref key) = self.agent_key {
            if let Some(rewritten) = self.llm_rewrite(key, trimmed) {
                let rl = rewritten.trim().to_lowercase();
                // Sanity check: LLM should return SQL
                if rl.starts_with("select") || rl.starts_with("with") {
                    return Ok(rewritten.trim().to_string());
                }
            }
            // Fall through to rule-based if LLM fails or returns non-SQL
        }

        self.rule_based_rewrite(trimmed)
    }

    /// Rule-based query rewrite — no external calls.
    fn rule_based_rewrite(&self, input: &str) -> Result<String> {
        let lower = input.to_lowercase();

        // Vector literal
        if lower.starts_with('[') && lower.ends_with(']') {
            return Ok(format!(
                "SELECT id FROM docs WHERE vector_topk_cosine('{input}', 10)"
            ));
        }

        // Contains operator keywords or vector function → likely a WHERE clause
        if lower.contains(" and ") || lower.contains(" or ") || lower.contains("vector_topk") {
            return Ok(format!("SELECT id FROM docs WHERE {input}"));
        }

        // Contains = → tag query
        if lower.contains(" = ") || lower.contains("='") {
            return Ok(format!("SELECT id FROM docs WHERE {input}"));
        }

        // Default: exact match on text tag
        Ok(format!(
            "SELECT id FROM docs WHERE text = '{}'",
            input.replace('\'', "''")
        ))
    }

    /// Rewrite via Claude API (HTTPS POST to api.anthropic.com).
    /// Returns None on any failure — caller falls back to rule-based.
    fn llm_rewrite(&self, api_key: &str, input: &str) -> Option<String> {
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
        let mut sock = std::net::TcpStream::connect_timeout(
            &"api.anthropic.com:443".parse().ok()?,
            std::time::Duration::from_secs(10),
        )
        .ok()?;
        sock.set_read_timeout(Some(std::time::Duration::from_secs(15)))
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
        // No AGENT_KEY → rule-based
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
        assert!(r.contains("text = 'memories about testing'"));
    }

    #[test]
    fn no_key_passthrough() {
        // Without AGENT_KEY, rule-based always applies
        let p = Planner::new(None);
        let r = p.rewrite_query("hello world").unwrap();
        assert!(r.starts_with("SELECT id FROM docs WHERE text = "));
    }

    #[test]
    fn escapes_quotes() {
        let p = Planner::new(None);
        let r = p.rewrite_query("it's a test").unwrap();
        assert!(r.contains("it''s a test"));
    }
}
