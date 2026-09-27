use std::env;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::{Value, json};

use crate::config::NutNewsConfig;

/// Narrow Nuts News client. Public reads need no credential; writes remain
/// impossible unless both configuration and an environment-backed token are
/// present.
#[derive(Clone)]
pub struct NutNewsClient {
    client: Client,
    config: NutNewsConfig,
}

impl NutNewsClient {
    pub fn new(config: NutNewsConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { client, config })
    }

    pub async fn newest(&self, limit: usize) -> Result<Value> {
        self.call("newest", json!({ "limit": limit.min(100) }), false)
            .await
    }

    pub async fn get_item(&self, id: u64) -> Result<Value> {
        self.call("get_item", json!({ "id": id }), false).await
    }

    pub async fn comment(&self, item: u64, parent: Option<u64>, text: &str) -> Result<Value> {
        self.call(
            "comment",
            json!({ "item": item, "parent": parent, "text": text }),
            true,
        )
        .await
    }

    async fn call(&self, tool: &str, arguments: Value, write: bool) -> Result<Value> {
        if !self.config.enabled {
            bail!("Nuts News connector is disabled");
        }
        if write && !self.config.allow_writes {
            bail!("Nuts News writes are disabled by policy");
        }

        let body = json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        });
        let mut request = self.client.post(&self.config.mcp_url).json(&body);
        if write {
            let token = env::var(&self.config.token_env).with_context(|| {
                format!(
                    "write requires token in environment variable {}",
                    self.config.token_env
                )
            })?;
            request = request.bearer_auth(token);
        }

        let response = request.send().await?.error_for_status()?;
        let value: Value = response.json().await?;
        if let Some(error) = value.get("error") {
            bail!("Nuts News MCP error: {error}");
        }
        Ok(value.get("result").cloned().unwrap_or(value))
    }
}
