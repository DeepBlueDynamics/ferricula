//! Native streamable-HTTP MCP server.
//!
//! Mounted at `/mcp` on `MCP_PORT` (or `serve_port + 1`). Tools proxy to the
//! existing ferricula tiny_http API on `serve_port`. Embeddings (for
//! `remember`/`reflect`/`observe`) are obtained from shivvr at
//! `{shivvr_url}/temp/ferricula/ingest` before storage.

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
};
use rmcp::transport::streamable_http_server::{StreamableHttpService, StreamableHttpServerConfig};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

// --- Request structs ---------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberRequest {
    pub text: String,
    pub channel: Option<String>,
    pub importance: Option<f32>,
    pub keystone: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallRequest {
    pub query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReflectRequest {
    pub thought: String,
    pub importance: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ObserveRequest {
    pub path: String,
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InspectRequest {
    pub id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeystoneRequest {
    pub id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NeighborsRequest {
    pub id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InversionRequest {
    pub id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConnectRequest {
    pub a: u32,
    pub b: u32,
    pub label: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DisconnectRequest {
    pub a: u32,
    pub b: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryRequest {
    pub sql: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmbodyRequest {
    pub memories: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OfferEntropyRequest {
    pub source: Option<String>,
}

// --- Helpers -----------------------------------------------------------------

fn channel_alpha(channel: &str) -> f32 {
    match channel {
        "hearing" | "seeing" => 0.010,
        "thinking" => 0.015,
        "body" => 0.020,
        "taste" => 0.018,
        "smell" => 0.020,
        _ => 0.015,
    }
}

fn gen_id() -> u32 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    (millis % u32::MAX as u128) as u32
}

fn ok_text(s: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![Content::text(s.into())]))
}

// --- Server ------------------------------------------------------------------

#[derive(Clone)]
pub struct FerriulaMcp {
    pub base_url: String,
    pub shivvr_url: String,
    pub radio_url: String,
    pub client: reqwest::Client,
    tool_router: ToolRouter<Self>,
}

impl FerriulaMcp {
    pub fn new(ferricula_port: u16, shivvr_url: String, radio_url: String) -> Self {
        Self {
            base_url: format!("http://localhost:{ferricula_port}"),
            shivvr_url,
            radio_url,
            client: reqwest::Client::new(),
            tool_router: Self::tool_router(),
        }
    }

    async fn fget(&self, path: &str) -> String {
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        match self.client.get(&url).send().await {
            Ok(r) => r.text().await.unwrap_or_else(|e| format!("error reading body: {e}")),
            Err(e) => format!("error: {e}"),
        }
    }

    async fn fpost(&self, path: &str, body: &str) -> String {
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        match self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
        {
            Ok(r) => r.text().await.unwrap_or_else(|e| format!("error reading body: {e}")),
            Err(e) => format!("error: {e}"),
        }
    }

    async fn fpost_text(&self, path: &str, body: &str) -> String {
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        match self
            .client
            .post(&url)
            .header("Content-Type", "text/plain")
            .body(body.to_string())
            .send()
            .await
        {
            Ok(r) => r.text().await.unwrap_or_else(|e| format!("error reading body: {e}")),
            Err(e) => format!("error: {e}"),
        }
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let url = format!(
            "{}/temp/ferricula/ingest",
            self.shivvr_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(json!({ "text": text }).to_string())
            .send()
            .await
            .map_err(|e| format!("shivvr embed request failed: {e}"))?;
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("shivvr embed parse failed: {e}"))?;
        let arr = data
            .get("chunks")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("embedding"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| "shivvr embed: missing chunks[0].embedding".to_string())?;
        let mut out = Vec::with_capacity(arr.len());
        for v in arr {
            let f = v
                .as_f64()
                .ok_or_else(|| "shivvr embed: embedding contains non-number".to_string())?;
            out.push(f as f32);
        }
        Ok(out)
    }
}

#[tool_router]
impl FerriulaMcp {
    #[tool(description = "Store a memory in ferricula. Embeds the text via shivvr, then writes a record with the given channel (hearing/seeing/thinking/body/taste/smell). Optional importance (0..1) and keystone flag. Returns the stored row id.")]
    async fn ferricula_remember(
        &self,
        Parameters(req): Parameters<RememberRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let channel = req.channel.unwrap_or_else(|| "thinking".to_string());
        let alpha = channel_alpha(&channel);
        let vector = match self.embed(&req.text).await {
            Ok(v) => v,
            Err(e) => return ok_text(format!("embed error: {e}")),
        };
        let id = gen_id();
        let preview: String = req.text.chars().take(200).collect();
        let mut body = json!({
            "id": id,
            "tags": { "channel": channel, "text": preview },
            "vector": vector,
            "decay_alpha": alpha,
        });
        if let Some(imp) = req.importance {
            body["importance"] = json!(imp);
        }
        if let Some(ks) = req.keystone {
            body["keystone"] = json!(ks);
        }
        let resp = self.fpost("/remember", &body.to_string()).await;
        ok_text(resp)
    }

    #[tool(description = "Recall memories matching a natural-language query. Ferricula handles embedding internally and returns ranked hits.")]
    async fn ferricula_recall(
        &self,
        Parameters(req): Parameters<RecallRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = json!({ "query": req.query }).to_string();
        let resp = self.fpost("/recall", &body).await;
        ok_text(resp)
    }

    #[tool(description = "Record a reflective thought. Stored on the 'thinking' channel with alpha=0.015. Optional importance.")]
    async fn ferricula_reflect(
        &self,
        Parameters(req): Parameters<ReflectRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let vector = match self.embed(&req.thought).await {
            Ok(v) => v,
            Err(e) => return ok_text(format!("embed error: {e}")),
        };
        let id = gen_id();
        let preview: String = req.thought.chars().take(200).collect();
        let mut body = json!({
            "id": id,
            "tags": { "channel": "thinking", "text": preview },
            "vector": vector,
            "decay_alpha": 0.015f32,
        });
        if let Some(imp) = req.importance {
            body["importance"] = json!(imp);
        }
        let resp = self.fpost("/remember", &body.to_string()).await;
        ok_text(resp)
    }

    #[tool(description = "Record an observation about a file or external artifact. Stored as a keystone on the 'seeing' channel with type=file and the given path.")]
    async fn ferricula_observe(
        &self,
        Parameters(req): Parameters<ObserveRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let summary = req.summary.unwrap_or_else(|| req.path.clone());
        let vector = match self.embed(&summary).await {
            Ok(v) => v,
            Err(e) => return ok_text(format!("embed error: {e}")),
        };
        let id = gen_id();
        let preview: String = summary.chars().take(200).collect();
        let body = json!({
            "id": id,
            "tags": {
                "channel": "seeing",
                "text": preview,
                "type": "file",
                "path": req.path,
            },
            "vector": vector,
            "decay_alpha": channel_alpha("seeing"),
            "keystone": true,
        });
        let resp = self.fpost("/remember", &body.to_string()).await;
        ok_text(resp)
    }

    #[tool(description = "Inspect a memory record by id. Returns the full row.")]
    async fn ferricula_inspect(
        &self,
        Parameters(req): Parameters<InspectRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self.fget(&format!("/inspect/{}", req.id)).await;
        ok_text(resp)
    }

    #[tool(description = "Get ferricula runtime status (counts, last events, thermodynamic state).")]
    async fn ferricula_status(&self) -> Result<CallToolResult, ErrorData> {
        let resp = self.fget("/status").await;
        ok_text(resp)
    }

    #[tool(description = "Get ferricula's identity (agent_id, name, archetype, casting).")]
    async fn ferricula_identity(&self) -> Result<CallToolResult, ErrorData> {
        let resp = self.fget("/identity").await;
        ok_text(resp)
    }

    #[tool(description = "Combined health check: ferricula /status plus shivvr /health.")]
    async fn ferricula_health(&self) -> Result<CallToolResult, ErrorData> {
        let f_status = self.fget("/status").await;
        let shivvr_url = format!("{}/health", self.shivvr_url.trim_end_matches('/'));
        let s_health = match self.client.get(&shivvr_url).send().await {
            Ok(r) => r.text().await.unwrap_or_else(|e| format!("error reading body: {e}")),
            Err(e) => format!("error: {e}"),
        };
        ok_text(format!("=== ferricula ===\n{f_status}\n\n=== shivvr ===\n{s_health}"))
    }

    #[tool(description = "Trigger a dream cycle (consolidation pass). Returns the dream report.")]
    async fn ferricula_dream(&self) -> Result<CallToolResult, ErrorData> {
        let resp = self.fpost("/dream", "{}").await;
        ok_text(resp)
    }

    #[tool(description = "Mark a memory as a keystone (resists decay). Provide its id.")]
    async fn ferricula_keystone(
        &self,
        Parameters(req): Parameters<KeystoneRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self.fpost(&format!("/keystone/{}", req.id), "").await;
        ok_text(resp)
    }

    #[tool(description = "List graph neighbors of a memory (ids and edge labels).")]
    async fn ferricula_neighbors(
        &self,
        Parameters(req): Parameters<NeighborsRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self.fget(&format!("/neighbors/{}", req.id)).await;
        ok_text(resp)
    }

    #[tool(description = "Connect two memories with an optional label and edge kind.")]
    async fn ferricula_connect(
        &self,
        Parameters(req): Parameters<ConnectRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = json!({
            "a": req.a,
            "b": req.b,
            "label": req.label,
            "kind": req.kind,
        });
        let resp = self.fpost("/connect", &body.to_string()).await;
        ok_text(resp)
    }

    #[tool(description = "Remove the edge between two memories.")]
    async fn ferricula_disconnect(
        &self,
        Parameters(req): Parameters<DisconnectRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = json!({ "a": req.a, "b": req.b });
        let resp = self.fpost("/disconnect", &body.to_string()).await;
        ok_text(resp)
    }

    #[tool(description = "Get clock/cadence telemetry (heat, drift, dream cadence).")]
    async fn ferricula_clock(&self) -> Result<CallToolResult, ErrorData> {
        let resp = self.fget("/clock").await;
        ok_text(resp)
    }

    #[tool(description = "Force a checkpoint of the durable engine to disk.")]
    async fn ferricula_checkpoint(&self) -> Result<CallToolResult, ErrorData> {
        let resp = self.fpost("/checkpoint", "").await;
        ok_text(resp)
    }

    #[tool(description = "List vocabulary terms tracked by ferricula's tokenizer.")]
    async fn ferricula_terms(&self) -> Result<CallToolResult, ErrorData> {
        let resp = self.fget("/terms").await;
        ok_text(resp)
    }

    #[tool(description = "Run a SQL query against the memory store. Pass the query as 'sql'.")]
    async fn ferricula_query(
        &self,
        Parameters(req): Parameters<QueryRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = json!({ "query": req.sql }).to_string();
        let resp = self.fpost("/query", &body).await;
        ok_text(resp)
    }

    #[tool(description = "Run an inversion check on a memory id (compares stored vector against re-embedding to detect drift).")]
    async fn ferricula_inversion_check(
        &self,
        Parameters(req): Parameters<InversionRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self.fget(&format!("/inversion/{}", req.id)).await;
        ok_text(resp)
    }

    #[tool(description = "Embody: gather identity, status, and the latest dream into a single contextual snapshot. Optional 'memories' is reserved for future expansion.")]
    async fn ferricula_embody(
        &self,
        Parameters(_req): Parameters<EmbodyRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let identity = self.fget("/identity").await;
        let status = self.fget("/status").await;
        let dream = self.fget("/dream/latest").await;
        ok_text(format!(
            "=== identity ===\n{identity}\n\n=== status ===\n{status}\n\n=== latest dream ===\n{dream}"
        ))
    }

    #[tool(description = "Offer entropy to the engine. With source='radio', fetches 64 bytes from the radio entropy API; otherwise pass a hex string as 'source' to be offered directly.")]
    async fn ferricula_offer_entropy(
        &self,
        Parameters(req): Parameters<OfferEntropyRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let source = req.source.unwrap_or_else(|| "radio".to_string());
        let payload = if source == "radio" {
            let url = format!(
                "{}/api/entropy?bytes=64&format=json",
                self.radio_url.trim_end_matches('/')
            );
            let resp = match self.client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => return ok_text(format!("radio fetch error: {e}")),
            };
            let data: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => return ok_text(format!("radio parse error: {e}")),
            };
            match data.get("entropy_hex").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => return ok_text("radio: missing entropy_hex".to_string()),
            }
        } else {
            source
        };
        let resp = self.fpost_text("/offer", &payload).await;
        ok_text(resp)
    }
}

#[tool_handler]
impl ServerHandler for FerriulaMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "ferricula".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: None,
                description: None,
                website_url: None,
                icons: None,
            },
            instructions: Some(
                "Ferricula MCP — agent memory with thermodynamic decay.\n\
                 Tools:\n\
                 - ferricula_remember(text, channel?, importance?, keystone?): store a memory (embeds via shivvr).\n\
                 - ferricula_recall(query): semantic recall.\n\
                 - ferricula_reflect(thought, importance?): record on 'thinking' channel.\n\
                 - ferricula_observe(path, summary?): keystone observation of a file.\n\
                 - ferricula_inspect(id): full row by id.\n\
                 - ferricula_status / ferricula_identity / ferricula_health: introspection.\n\
                 - ferricula_dream: trigger consolidation.\n\
                 - ferricula_keystone(id): pin a memory against decay.\n\
                 - ferricula_neighbors(id) / ferricula_connect(a,b,...) / ferricula_disconnect(a,b): graph ops.\n\
                 - ferricula_clock / ferricula_checkpoint / ferricula_terms.\n\
                 - ferricula_query(sql): raw SQL.\n\
                 - ferricula_inversion_check(id): drift check.\n\
                 - ferricula_embody(memories?): identity + status + latest dream snapshot.\n\
                 - ferricula_offer_entropy(source?): feed entropy from radio or a hex string."
                    .to_string(),
            ),
        }
    }
}

// --- Service entry points ----------------------------------------------------

pub fn streamable_http_service(
    ferricula_port: u16,
    shivvr_url: String,
    radio_url: String,
) -> StreamableHttpService<FerriulaMcp> {
    StreamableHttpService::new(
        move || Ok(FerriulaMcp::new(ferricula_port, shivvr_url.clone(), radio_url.clone())),
        Default::default(),
        StreamableHttpServerConfig::default(),
    )
}

pub async fn run_mcp_server(
    ferricula_port: u16,
    mcp_port: u16,
    shivvr_url: String,
    radio_url: String,
) -> anyhow::Result<()> {
    let app = axum::Router::new()
        .nest_service("/mcp", streamable_http_service(ferricula_port, shivvr_url, radio_url));
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{mcp_port}")).await?;
    eprintln!(
        "[mcp] MCP server on :{mcp_port} -> claude mcp add ferricula --sse http://localhost:{mcp_port}/mcp"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
