//! MCP Streamable HTTP endpoint for Steve (`POST /mcp`).
//!
//! Protocol: MCP **2025-06-18** JSON-RPC 2.0, JSON response mode (no SSE).
//! Advertise only this revision. Older initialize proposals receive
//! **2025-06-18**; the client decides compatibility. Subsequent
//! `MCP-Protocol-Version` headers other than **2025-06-18** are rejected.
//! Tools: `steve_status`, `steve_recall` (read-only), `steve_chat` (operator chat).
//! Operator bearer auth required on `POST /mcp`. Damascus is not a launch blocker.
//!
//! 2025-03-26 Streamable HTTP allowed JSON-RPC **batches** on POST.
//! 2025-06-18: POST body **MUST** be a **single** JSON-RPC message.
//! This server follows 2025-06-18 and rejects arrays (do not negotiate
//! 2025-03-26 while rejecting its permitted batch transport).

use std::sync::Arc;

use axum::extract::State;
use uuid::Uuid;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::runtime::SteveRuntime;

/// Advertised MCP protocol revision (only).
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// JSON-RPC / MCP endpoint path.
pub const MCP_PATH: &str = "/mcp";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const MAX_RECALL_LIMIT: usize = 64;
const MAX_QUERY_BYTES: usize = 8192;
const MAX_CHAT_MESSAGE_BYTES: usize = 8192;
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

/// Unstated router for Goldfish to merge onto `Arc<SteveRuntime>`.
pub fn router() -> Router<Arc<SteveRuntime>> {
    Router::new().route(MCP_PATH, post(mcp_post).get(mcp_get).delete(mcp_delete))
}

#[derive(Debug)]
enum McpHttp {
    Unauthorized,
    ForbiddenOrigin,
    Accepted,
    MethodNotAllowed,
    Json(StatusCode, Value),
}

impl IntoResponse for McpHttp {
    fn into_response(self) -> Response {
        match self {
            McpHttp::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "operator authorization required" })),
            )
                .into_response(),
            McpHttp::ForbiddenOrigin => (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "origin not allowed" })),
            )
                .into_response(),
            McpHttp::Accepted => StatusCode::ACCEPTED.into_response(),
            McpHttp::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "POST")],
                Json(json!({
                    "error": "SSE GET/DELETE sessions are not offered; POST JSON-RPC to /mcp"
                })),
            )
                .into_response(),
            McpHttp::Json(status, value) => (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                value.to_string(),
            )
                .into_response(),
        }
    }
}

async fn mcp_get() -> McpHttp {
    McpHttp::MethodNotAllowed
}

async fn mcp_delete() -> McpHttp {
    McpHttp::MethodNotAllowed
}

async fn mcp_post(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    body: String,
) -> McpHttp {
    handle_post(runtime, &headers, &body).await
}

async fn handle_post(runtime: Arc<SteveRuntime>, headers: &HeaderMap, body: &str) -> McpHttp {
    match classify_origin(headers) {
        OriginCheck::Absent | OriginCheck::Allowed => {}
        OriginCheck::Denied | OriginCheck::Invalid => return McpHttp::ForbiddenOrigin,
    }
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if !runtime.authorize(authorization) {
        return McpHttp::Unauthorized;
    }
    if body.trim().is_empty() {
        return McpHttp::Json(
            StatusCode::BAD_REQUEST,
            rpc_error(Value::Null, PARSE_ERROR, "Parse error"),
        );
    }
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => {
            return McpHttp::Json(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, PARSE_ERROR, "Parse error"),
            );
        }
    };
    if parsed.is_array() {
        return McpHttp::Json(
            StatusCode::BAD_REQUEST,
            rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "JSON-RPC batches are not accepted (MCP 2025-06-18 POST must be a single message)",
            ),
        );
    }
    if let Err(response) = check_protocol_header(headers) {
        return response;
    }
    match handle_rpc(runtime, &parsed).await {
        RpcOut::Accepted => McpHttp::Accepted,
        RpcOut::Body(value) => McpHttp::Json(StatusCode::OK, value),
        RpcOut::BadRequest(value) => McpHttp::Json(StatusCode::BAD_REQUEST, value),
    }
}

fn check_protocol_header(headers: &HeaderMap) -> Result<(), McpHttp> {
    match headers.get(PROTOCOL_HEADER) {
        None => Ok(()),
        Some(value) => {
            let Ok(raw) = value.to_str() else {
                return Err(McpHttp::Json(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "invalid MCP-Protocol-Version header" }),
                ));
            };
            if raw == PROTOCOL_VERSION {
                Ok(())
            } else {
                Err(McpHttp::Json(
                    StatusCode::BAD_REQUEST,
                    json!({
                        "error": "unsupported MCP-Protocol-Version",
                        "supported": [PROTOCOL_VERSION],
                        "requested": raw
                    }),
                ))
            }
        }
    }
}

#[derive(Debug)]
enum RpcOut {
    Accepted,
    Body(Value),
    BadRequest(Value),
}

async fn handle_rpc(runtime: Arc<SteveRuntime>, msg: &Value) -> RpcOut {
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        return RpcOut::BadRequest(rpc_error(id, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
    }
    let method = match msg.get("method").and_then(Value::as_str) {
        Some(method) => method,
        None => {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            return RpcOut::BadRequest(rpc_error(id, INVALID_REQUEST, "missing method"));
        }
    };
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    if method.starts_with("notifications/") || (id.is_none() && method == "initialized") {
        return RpcOut::Accepted;
    }
    let Some(id) = id else {
        return RpcOut::Accepted;
    };

    let result = match method {
        "initialize" => initialize_result(&runtime, &params),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(&runtime, &params).await,
        _ => Err(rpc_error(
            id.clone(),
            METHOD_NOT_FOUND,
            &format!("Method not found: {method}"),
        )),
    };
    match result {
        Ok(value) => RpcOut::Body(rpc_ok(id, value)),
        Err(mut error) => {
            if let Some(obj) = error.as_object_mut() {
                obj.insert("id".into(), id);
            }
            RpcOut::Body(error)
        }
    }
}

fn initialize_result(runtime: &SteveRuntime, _params: &Value) -> Result<Value, Value> {
    Ok(json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "ferricula-steve",
            "title": "Steve Jobs",
            "version": env!("CARGO_PKG_VERSION"),
            "agent_id": runtime.inspection.agent_id
        },
        "instructions": "Steve MCP. steve_status and steve_recall are read-only and require operator auth. Recall returns metadata (ids/tags/refs), not hydrated source text. steve_chat sends one operator message through POST /chat and returns reply plus conversation_id. Damascus work-broker is not mounted."
    }))
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "steve_status",
                "description": "Read-only Steve runtime status (identity, mode, memory counts). No memory text.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                },
                "annotations": {
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                }
            },
            {
                "name": "steve_recall",
                "description": "Read-only lexical memory candidate lookup. Returns ids, scores, tags, and refs — not source bodies.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Non-empty lexical query"
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_RECALL_LIMIT,
                            "default": 10
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                },
                "annotations": {
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                }
            },
            {
                "name": "steve_chat",
                "description": "Send one operator message to Steve (POST /chat). Returns the model reply and conversation_id. Does not hydrate source text.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Operator message, 1 to 8192 UTF-8 bytes"
                        },
                        "conversation_id": {
                            "type": "string",
                            "description": "Optional UUID of an existing conversation. Omitted starts a new one."
                        }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                },
                "annotations": {
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": true
                }
            }
        ]
    })
}

async fn tools_call(runtime: &Arc<SteveRuntime>, params: &Value) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "steve_status" => {
            let status = runtime.status();
            let payload = serde_json::to_value(&status)
                .map_err(|err| rpc_error(Value::Null, INTERNAL_ERROR, &err.to_string()))?;
            Ok(tool_text(&payload, false))
        }
        "steve_recall" => {
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if query.is_empty() {
                return Ok(tool_text(
                    &json!({ "error": "query cannot be empty" }),
                    true,
                ));
            }
            if query.len() > MAX_QUERY_BYTES {
                return Ok(tool_text(
                    &json!({ "error": "query exceeds 8192 bytes" }),
                    true,
                ));
            }
            let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let limit = limit.clamp(1, MAX_RECALL_LIMIT);
            let hits = runtime.recall_candidates(query, limit);
            let payload = json!({ "query": query, "hits": hits });
            Ok(tool_text(&payload, false))
        }
        "steve_chat" => steve_chat(runtime, &arguments).await,
        "" => Err(rpc_error(
            Value::Null,
            INVALID_PARAMS,
            "tools/call missing name",
        )),
        other => Err(rpc_error(
            Value::Null,
            INVALID_PARAMS,
            &format!("Unknown tool: {other}"),
        )),
    }
}

async fn steve_chat(runtime: &Arc<SteveRuntime>, arguments: &Value) -> Result<Value, Value> {
    let Some(obj) = arguments.as_object() else {
        return Ok(tool_text(&json!({"error": "arguments must be an object"}), true));
    };
    for key in obj.keys() {
        if key != "message" && key != "conversation_id" {
            return Ok(tool_text(
                &json!({"error": format!("unknown argument: {key}")}),
                true,
            ));
        }
    }
    let Some(message) = obj.get("message").and_then(Value::as_str) else {
        return Ok(tool_text(&json!({"error": "message is required"}), true));
    };
    if message.trim().is_empty() || message.len() > MAX_CHAT_MESSAGE_BYTES {
        return Ok(tool_text(
            &json!({"error": "message must contain 1 to 8192 UTF-8 bytes"}),
            true,
        ));
    }
    let conversation_id = match obj.get("conversation_id") {
        None | Some(Value::Null) => Uuid::new_v4(),
        Some(Value::String(raw)) => match Uuid::parse_str(raw.trim()) {
            Ok(id) => id,
            Err(_) => {
                return Ok(tool_text(
                    &json!({"error": "conversation_id must be a uuid"}),
                    true,
                ));
            }
        },
        Some(_) => {
            return Ok(tool_text(
                &json!({"error": "conversation_id must be a string"}),
                true,
            ));
        }
    };
    let request = crate::runtime::ChatRequest {
        request_id: Uuid::new_v4(),
        conversation_id,
        message: message.to_string(),
        reported_origin: crate::runtime::InputOrigin::Agent,
    };
    match runtime.converse(request).await {
        Ok(turn) => {
            let failed = turn.reply.is_none() || turn.status != "completed";
            Ok(tool_text(
                &json!({
                    "reply": turn.reply,
                    "conversation_id": turn.request.conversation_id.to_string(),
                }),
                failed,
            ))
        }
        Err(err) => Ok(tool_text(
            &json!({
                "error": err.to_string(),
                "conversation_id": conversation_id.to_string(),
            }),
            true,
        )),
    }
}

fn tool_text(payload: &Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": payload.to_string() }],
        "isError": is_error
    })
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[derive(Debug, PartialEq, Eq)]
enum OriginCheck {
    Absent,
    Allowed,
    Denied,
    Invalid,
}

fn classify_origin(headers: &HeaderMap) -> OriginCheck {
    let Some(value) = headers.get(header::ORIGIN) else {
        return OriginCheck::Absent;
    };
    let Ok(raw) = value.to_str() else {
        return OriginCheck::Invalid;
    };
    match parse_origin_host(raw) {
        None => OriginCheck::Invalid,
        Some(host) if host_is_loopback(&host) => OriginCheck::Allowed,
        Some(_) => OriginCheck::Denied,
    }
}

/// RFC 6454 Origin: `scheme://host[:port]` only. No userinfo, path, query, or fragment.
fn parse_origin_host(origin: &str) -> Option<String> {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    if rest.contains('@') || rest.contains('/') || rest.contains('?') || rest.contains('#') {
        return None;
    }
    if rest.starts_with('[') {
        let end = rest.find(']')?;
        let host = rest[..=end].to_ascii_lowercase();
        let after = &rest[end + 1..];
        if !after.is_empty() {
            let port = after.strip_prefix(':')?;
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
        }
        return Some(host);
    }
    let host = match rest.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => rest,
    };
    if host.is_empty() || host.contains(':') {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn host_is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "[::1]"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeConfig;
    use crate::inspect_data_dir;
    use std::fs;
    use uuid::Uuid;

    fn json_body(http: McpHttp) -> Value {
        match http {
            McpHttp::Json(_, value) => value,
            other => panic!("expected JSON body, got {other:?}"),
        }
    }

    fn status_of(http: &McpHttp) -> StatusCode {
        match http {
            McpHttp::Unauthorized => StatusCode::UNAUTHORIZED,
            McpHttp::ForbiddenOrigin => StatusCode::FORBIDDEN,
            McpHttp::Accepted => StatusCode::ACCEPTED,
            McpHttp::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            McpHttp::Json(status, _) => *status,
        }
    }

    fn test_runtime() -> Arc<SteveRuntime> {
        let root = std::env::temp_dir().join(format!("ferricula-mcp-{}", Uuid::new_v4()));
        let memory = root.join("memory");
        let state = root.join("state");
        fs::create_dir_all(&memory).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(
            memory.join("identity.json"),
            r#"{"agent_id":"ferricula-stevejobs","name":"Steve Jobs"}"#,
        )
        .unwrap();
        let mut config = RuntimeConfig::default();
        config.memory_dir = memory.clone();
        config.state_dir = state.clone();
        config.overlay.path = state.join("overlay.json");
        config.schedule.enabled = false;
        config.require_operator_auth = false;
        let inspection = inspect_data_dir(&memory).unwrap();
        SteveRuntime::open(config, inspection).expect("test SteveRuntime")
    }

    fn post(runtime: &Arc<SteveRuntime>, origin: Option<&str>, protocol: Option<&str>, body: &str) -> McpHttp {
        let mut headers = HeaderMap::new();
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
        }
        if let Some(protocol) = protocol {
            headers.insert(PROTOCOL_HEADER, protocol.parse().unwrap());
        }
        let runtime = Arc::clone(runtime);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mcp test runtime")
            .block_on(handle_post(runtime, &headers, body))
    }

    #[test]
    fn origin_rejects_suffix_spoof_userinfo_and_opaque_header() {
        assert_eq!(
            classify_origin(&HeaderMap::new()),
            OriginCheck::Absent
        );
        for denied in [
            "http://localhost.evil",
            "http://127.0.0.1.evil",
            "https://localhost.evil.example",
            "http://user@localhost",
            "http://127.0.0.1:18875/path",
            "null",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, denied.parse().unwrap());
            let check = classify_origin(&headers);
            assert!(
                matches!(check, OriginCheck::Denied | OriginCheck::Invalid),
                "{denied} => {check:?}"
            );
        }
        for allowed in [
            "http://127.0.0.1:18875",
            "http://localhost",
            "https://localhost:443",
            "http://[::1]:8875",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, allowed.parse().unwrap());
            assert_eq!(classify_origin(&headers), OriginCheck::Allowed, "{allowed}");
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap());
        assert_eq!(classify_origin(&headers), OriginCheck::Invalid);
    }

    #[test]
    fn handler_forbidden_on_spoofed_origin() {
        let runtime = test_runtime();
        let http = post(
            &runtime,
            Some("http://localhost.evil"),
            None,
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        );
        assert_eq!(status_of(&http), StatusCode::FORBIDDEN);
    }

    #[test]
    fn handler_ping_initialize_list_and_unknown_tool() {
        let runtime = test_runtime();
        let ping = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#,
        );
        let ping = json_body(ping);
        assert_eq!(ping["result"], json!({}));
        assert_eq!(ping["id"], "p");

        let init = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        );
        let init = json_body(init);
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(init["result"]["serverInfo"]["agent_id"], "ferricula-stevejobs");

        let list = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        );
        let list = json_body(list);
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["steve_status", "steve_recall", "steve_chat"]);

        let unknown = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        );
        let unknown = json_body(unknown);
        assert_eq!(unknown["id"], 3);
        assert_eq!(unknown["error"]["code"], INVALID_PARAMS);
        assert!(unknown["error"]["message"].as_str().unwrap().contains("Unknown tool"));
    }

    #[test]
    fn handler_status_and_empty_recall() {
        let runtime = test_runtime();
        let status = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"steve_status","arguments":{}}}"#,
        );
        let status = json_body(status);
        assert_eq!(status["result"]["isError"], false);
        let text = status["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["agent_id"], "ferricula-stevejobs");

        let recall = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"steve_recall","arguments":{"query":"   "}}}"#,
        );
        let recall = json_body(recall);
        assert_eq!(recall["result"]["isError"], true);
    }

    #[test]
    fn handler_steve_chat_rejects_bad_arguments_without_calling_the_model() {
        let runtime = test_runtime();
        let missing = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"steve_chat","arguments":{}}}"#,
        );
        let missing = json_body(missing);
        assert_eq!(missing["result"]["isError"], true);
        let text = missing["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("message is required"));

        let bad_id = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"steve_chat","arguments":{"message":"hello","conversation_id":"not-a-uuid"}}}"#,
        );
        let bad_id = json_body(bad_id);
        assert_eq!(bad_id["result"]["isError"], true);
        let text = bad_id["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("conversation_id must be a uuid"));
    }

    #[test]
    fn handler_rejects_batches_and_unsupported_protocol_header() {
        let runtime = test_runtime();
        let batch = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        );
        assert_eq!(status_of(&batch), StatusCode::BAD_REQUEST);
        let body = json_body(batch);
        assert_eq!(body["error"]["code"], INVALID_REQUEST);

        let bad_ver = post(
            &runtime,
            None,
            Some("1999-01-01"),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        );
        assert_eq!(status_of(&bad_ver), StatusCode::BAD_REQUEST);

        let legacy_header = post(
            &runtime,
            None,
            Some("2025-03-26"),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        );
        assert_eq!(status_of(&legacy_header), StatusCode::BAD_REQUEST);

        let note = post(
            &runtime,
            None,
            Some(PROTOCOL_VERSION),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert_eq!(status_of(&note), StatusCode::ACCEPTED);
    }

    #[test]
    fn initialize_older_proposal_returns_supported_version() {
        let runtime = test_runtime();
        let init = post(
            &runtime,
            None,
            None,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        );
        let init = json_body(init);
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
    }
}
