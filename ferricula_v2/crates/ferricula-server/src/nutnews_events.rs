//! Bounded, **read-only** Nuts News `events_since` client primitive.
//!
//! Implements the poll half of `docs/NUTS_EVENT_CONTRACT.md` for mention
//! wakeups, aligned with `research/nuts-news/CAPABILITY_AUDIT.md`:
//! - pinned origin only (no caller-supplied arbitrary URLs, no redirects)
//! - `limit ∈ 1..=500` enforced as a client error (never silently clamped)
//! - response body size and request timeout caps
//! - page parse + monotonic / uniqueness / gap consistency validation
//! - **no write methods** (comment/submit/profile stay out of this module)
//! - **no fallback** to SSE `/events` or MCP `newest` / front_page (lossy;
//!   they must never become a durable mention cursor)
//!
//! ## Server status (capability audit)
//!
//! The reference Nuts News server **does not yet expose `events_since`**.
//! Current public reads are `front_page`, `newest`, `get_item`, `whoami`,
//! plus a live-tail SSE stream at `GET /events` (body only; no durable
//! cursor / gap bit). This client is therefore **ready but server-blocked**:
//! it will call the contract tool and fail **clearly** on unknown-tool /
//! legacy shapes rather than inventing a silent `newest` poll.
//!
//! When the server ships `events_since`, ledger `event` objects are expected
//! to match the real tagged `Event` enum (`type: comment_posted|item_submitted|…`
//! with `by` / `item` / `id` fields). Bridge-friendly views are produced by
//! [`normalize_event_fields`].
//!
//! Network I/O is confined to the [`EventsTransport`] trait so unit tests
//! never leave the process. Delivery of a page is **consideration only** —
//! never a command to speak or publish.
//!
//! This file is intentionally not wired into `lib.rs` yet; integrate after
//! the server capability work lands.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ── Contract / safety bounds ────────────────────────────────────────────────

/// Contract minimum `events_since` limit (§3.1 / §6).
pub const MIN_EVENTS_LIMIT: u32 = 1;
/// Contract maximum `events_since` limit (§3.1 / §6).
pub const MAX_EVENTS_LIMIT: u32 = 500;
/// Sensible default under load (§6: clients SHOULD prefer 50–100).
pub const DEFAULT_EVENTS_LIMIT: u32 = 50;

/// Default production instance id (§1).
pub const DEFAULT_INSTANCE: &str = "news.nuts.services";
/// Default pinned origin (scheme + host only).
pub const DEFAULT_ORIGIN: &str = "https://news.nuts.services";
/// Default MCP JSON-RPC path under the pinned origin.
pub const DEFAULT_MCP_PATH: &str = "/mcp";

/// Default request timeout (milliseconds).
pub const DEFAULT_TIMEOUT_MS: u64 = 15_000;
/// Hard ceiling on configurable timeouts.
pub const MAX_TIMEOUT_MS: u64 = 60_000;
/// Floor so callers cannot set a zero timeout by mistake.
pub const MIN_TIMEOUT_MS: u64 = 250;

/// Default max HTTP response body size before parse (1 MiB).
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1_048_576;
/// Absolute ceiling for response body caps.
pub const MAX_RESPONSE_BYTES_CEILING: usize = 4 * 1_048_576;

/// Byte cap on `instance` strings (matches mention_ingest identity bound).
pub const MAX_INSTANCE_BYTES: usize = 128;

/// Live-tail SSE path on the reference server — **not** a replay cursor.
pub const FORBIDDEN_SSE_PATH: &str = "/events";
/// Projection listing path — **not** a ledger cursor (lossy under concurrency).
pub const FORBIDDEN_NEWEST_PATH: &str = "/newest";

/// Human-readable capability note for operators / status surfaces.
pub const SERVER_EVENTS_SINCE_STATUS: &str = "\
ready_client_server_blocked: reference Nuts News exposes SSE /events and \
MCP newest/front_page/get_item/whoami but not events_since; durable mention \
wakeups require the server patch in research/nuts-news/CAPABILITY_AUDIT.md \
§3.A. Do not substitute newest or SSE for cursored replay.";

// ── Configuration ───────────────────────────────────────────────────────────

/// Immutable, validated settings for a pinned-origin events poller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NutsEventsConfig {
    /// Canonical origin: `scheme://host[:port]` only (no path, query, or userinfo).
    pub origin: String,
    /// Absolute path of the MCP endpoint (e.g. `/mcp`).
    pub mcp_path: String,
    /// Fully joined request URL, derived once at construction (never recomputed
    /// from untrusted input).
    pub request_url: String,
    /// Instance id the bridge expects; page mismatches are hard errors.
    pub expected_instance: String,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
    pub default_limit: u32,
}

impl Default for NutsEventsConfig {
    fn default() -> Self {
        Self::pin(
            DEFAULT_ORIGIN,
            DEFAULT_MCP_PATH,
            DEFAULT_INSTANCE,
            DEFAULT_TIMEOUT_MS,
            DEFAULT_MAX_RESPONSE_BYTES,
            DEFAULT_EVENTS_LIMIT,
        )
        .expect("built-in defaults validate")
    }
}

impl NutsEventsConfig {
    /// Build a config with a **pinned** origin. Rejects credentials, fragments,
    /// query strings on the origin, non-http(s) schemes, relative paths that
    /// escape the origin, and out-of-range limits/timeouts/sizes.
    pub fn pin(
        origin: impl AsRef<str>,
        mcp_path: impl AsRef<str>,
        expected_instance: impl AsRef<str>,
        timeout_ms: u64,
        max_response_bytes: usize,
        default_limit: u32,
    ) -> Result<Self> {
        let origin = normalize_origin(origin.as_ref())?;
        let mcp_path = normalize_mcp_path(mcp_path.as_ref())?;
        let expected_instance = normalize_instance(expected_instance.as_ref())?;
        validate_timeout_ms(timeout_ms)?;
        validate_max_response_bytes(max_response_bytes)?;
        validate_limit(default_limit)?;

        let request_url = format!("{origin}{mcp_path}");
        // Defense in depth: joined URL must still parse and share origin host.
        assert_url_pinned_to_origin(&request_url, &origin)?;

        Ok(Self {
            origin,
            mcp_path,
            request_url,
            expected_instance,
            timeout_ms,
            max_response_bytes,
            default_limit,
        })
    }

    /// Convenience: production-like defaults with a custom origin/instance pair.
    pub fn pinned_instance(origin: &str, instance: &str) -> Result<Self> {
        Self::pin(
            origin,
            DEFAULT_MCP_PATH,
            instance,
            DEFAULT_TIMEOUT_MS,
            DEFAULT_MAX_RESPONSE_BYTES,
            DEFAULT_EVENTS_LIMIT,
        )
    }
}

// ── Transport ───────────────────────────────────────────────────────────────

/// Fully prepared, origin-pinned `events_since` HTTP request.
///
/// Callers never supply a URL: the client fills [`PreparedEventsSince::url`]
/// exclusively from [`NutsEventsConfig::request_url`].
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedEventsSince {
    pub url: String,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
    /// Cursor argument: return `seq > after`.
    pub after: u64,
    pub limit: u32,
    /// JSON-RPC 2.0 `tools/call` body for MCP.
    pub body: Value,
}

/// Raw transport response (status + bytes). Body length must already respect
/// [`PreparedEventsSince::max_response_bytes`] (enforced by production transport
/// and checked again by the client before parse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// Parsed `Retry-After` delay in seconds when the server sent one (429).
    pub retry_after_secs: Option<u64>,
    /// Lowercased `Content-Type` header value when the transport captured it.
    /// Used to refuse SSE (`text/event-stream`) and HTML legacy pages.
    pub content_type: Option<String>,
}

impl RawHttpResponse {
    pub fn json_ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            retry_after_secs: None,
            content_type: Some("application/json".into()),
        }
    }
}

/// Injectable HTTP surface. Production uses [`ReqwestEventsTransport`]; tests
/// use [`MockEventsTransport`]. Implementations MUST NOT follow redirects and
/// MUST NOT accept a URL other than the one on [`PreparedEventsSince`].
pub trait EventsTransport {
    fn execute(&self, request: &PreparedEventsSince) -> Result<RawHttpResponse>;
}

/// Production blocking transport: pinned URL POST, no redirects, hard timeout,
/// response body capped by content-length and streaming length.
#[derive(Debug, Default)]
pub struct ReqwestEventsTransport;

impl EventsTransport for ReqwestEventsTransport {
    fn execute(&self, request: &PreparedEventsSince) -> Result<RawHttpResponse> {
        // Refuse to fire at anything other than an absolute http(s) URL that
        // matches the prepared request (belt and braces vs. a buggy caller).
        assert_https_or_http_absolute(&request.url)?;

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(request.timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build events_since HTTP client")?;

        let response = client
            .post(&request.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&request.body)
            .send()
            .context("events_since transport send")?;

        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after_secs);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_ascii_lowercase());

        if let Some(len) = response.content_length()
            && len as usize > request.max_response_bytes
        {
            bail!(
                "events_since Content-Length {len} exceeds max_response_bytes {}",
                request.max_response_bytes
            );
        }

        // Cap read: reject bodies that grow past the bound. `Read::take`
        // consumes the response and stops after max+1 bytes so a huge body
        // cannot force unbounded allocation.
        use std::io::Read;
        let mut body = Vec::new();
        let mut limited = response.take(request.max_response_bytes as u64 + 1);
        limited
            .read_to_end(&mut body)
            .context("events_since body read")?;
        if body.len() > request.max_response_bytes {
            bail!(
                "events_since response body exceeds max_response_bytes {}",
                request.max_response_bytes
            );
        }

        Ok(RawHttpResponse {
            status,
            body,
            retry_after_secs,
            content_type,
        })
    }
}

// ── Page model ──────────────────────────────────────────────────────────────

/// One ledger entry as returned by `events_since` (§3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub seq: u64,
    /// Opaque event object; untrusted data.
    ///
    /// Real Nuts News shape (tagged enum from `events.rs`):
    /// `{ "type": "comment_posted", "id", "item", "parent"?, "by", "text", "ts" }`
    /// or `{ "type": "item_submitted", "id", "by", "title", "url"?, "text"?, "ts" }`, …
    ///
    /// Contract-shaped alternative also accepted:
    /// `{ "kind": "comment", "item_id", "comment_id"?, "actor", "text", … }`.
    pub event: Value,
}

/// Validated `EventPage` (§3.2) after client-side bounds checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub instance: String,
    pub ledger_floor: u64,
    pub ledger_height: u64,
    pub gap: bool,
    pub events: Vec<LedgerEvent>,
}

impl EventPage {
    /// Highest sequence present on the page, if any.
    pub fn max_seq(&self) -> Option<u64> {
        self.events.iter().map(|e| e.seq).max()
    }
}

/// Bridge-friendly view of one ledger event body.
///
/// Maps both the contract envelope (`kind` / `item_id` / `actor`) and the
/// real Nuts `Event` tagged shape (`type` / `by` / `item` / `id`) so
/// mention classification does not depend on which spelling the server uses
/// once `events_since` ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedEventFields {
    /// Normalized kind: `comment`, `item`, `vote`, `handle_change`,
    /// `classification`, `source`, `other`, …
    pub kind: String,
    /// Original `type` or `kind` string from the wire.
    pub raw_type: String,
    pub item_id: Option<u64>,
    pub comment_id: Option<u64>,
    pub parent_comment_id: Option<u64>,
    /// Author handle (`by` or `actor`). Empty only for non-actor events.
    pub actor: String,
    /// Primary text body (comment text, item text, or item title fallback).
    pub text: String,
    pub title: Option<String>,
    /// Unix seconds when present (`ts` or numeric `created_at`).
    pub created_at: Option<u64>,
}

impl NormalizedEventFields {
    /// Votes / handle churn / classification / crawl provenance are never
    /// mention candidates (contract §4.2 + real Event variants).
    pub fn is_ignorable_noise(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "vote"
                | "handle_change"
                | "classification"
                | "source"
                | "about"
                | "item_retitled"
                | "comment_flagged"
                | "comment_unflagged"
        )
    }
}

/// Map a raw ledger `event` object to bridge fields. Does not invent sequences.
pub fn normalize_event_fields(event: &Value) -> Result<NormalizedEventFields> {
    if !event.is_object() {
        bail!("ledger event body must be a JSON object");
    }

    // Prefer real Nuts tagged enum (`type`), then contract `kind`.
    let raw_type = event
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| event.get("kind").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string();
    if raw_type.is_empty() {
        bail!("ledger event missing both `type` (Nuts Event) and `kind` (contract)");
    }

    let kind = map_event_kind(&raw_type);
    let actor = event
        .get("by")
        .and_then(Value::as_str)
        .or_else(|| event.get("actor").and_then(Value::as_str))
        .or_else(|| event.get("handle").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    let (item_id, comment_id, parent_comment_id, text, title) = match kind.as_str() {
        "comment" => {
            // comment_posted / comment_edited / contract comment
            let comment_id = event.get("comment_id").and_then(Value::as_u64).or_else(|| {
                // Real Event uses `id` as the comment id for comment_*.
                if raw_type.starts_with("comment") || raw_type == "comment" || raw_type == "reply" {
                    event.get("id").and_then(Value::as_u64)
                } else {
                    None
                }
            });
            let item_id = event
                .get("item_id")
                .and_then(Value::as_u64)
                .or_else(|| event.get("item").and_then(Value::as_u64));
            let parent = event
                .get("parent_comment_id")
                .and_then(Value::as_u64)
                .or_else(|| event.get("parent").and_then(Value::as_u64));
            let text = event
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (item_id, comment_id, parent, text, None)
        }
        "item" => {
            let item_id = event
                .get("item_id")
                .and_then(Value::as_u64)
                .or_else(|| event.get("id").and_then(Value::as_u64));
            let title = event
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string);
            let text = event
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| title.clone())
                .unwrap_or_default();
            (item_id, None, None, text, title)
        }
        "vote" => {
            let target = event
                .get("item_id")
                .and_then(Value::as_u64)
                .or_else(|| event.get("target").and_then(Value::as_u64));
            (target, None, None, String::new(), None)
        }
        "classification" => {
            let item_id = event
                .get("item_id")
                .and_then(Value::as_u64)
                .or_else(|| event.get("item").and_then(Value::as_u64));
            (item_id, None, None, String::new(), None)
        }
        _ => {
            let item_id = event
                .get("item_id")
                .and_then(Value::as_u64)
                .or_else(|| event.get("item").and_then(Value::as_u64));
            let comment_id = event.get("comment_id").and_then(Value::as_u64);
            let parent = event
                .get("parent_comment_id")
                .and_then(Value::as_u64)
                .or_else(|| event.get("parent").and_then(Value::as_u64));
            let text = event
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (item_id, comment_id, parent, text, None)
        }
    };

    let created_at = event
        .get("ts")
        .and_then(Value::as_u64)
        .or_else(|| event.get("created_at").and_then(Value::as_u64));

    Ok(NormalizedEventFields {
        kind,
        raw_type,
        item_id,
        comment_id,
        parent_comment_id,
        actor,
        text,
        title,
        created_at,
    })
}

fn map_event_kind(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "comment" | "reply" | "comment_posted" | "comment_edited" => "comment".into(),
        "item" | "story" | "post" | "submit" | "item_submitted" => "item".into(),
        "vote" | "upvote" | "downvote" | "vote_cast" => "vote".into(),
        "handle" | "handle_change" | "profile" | "handle_claimed" => "handle_change".into(),
        "classification" | "classify" | "item_classified" => "classification".into(),
        "source_fetched" | "source_provided" | "source" => "source".into(),
        "about_set" | "about" => "about".into(),
        "item_retitled" => "item_retitled".into(),
        "comment_flagged" => "comment_flagged".into(),
        "comment_unflagged" => "comment_unflagged".into(),
        other => other.to_string(),
    }
}

/// Operator-facing status string for live acceptance dashboards.
pub fn capability_status() -> &'static str {
    SERVER_EVENTS_SINCE_STATUS
}

/// Outcome of a successful transport + parse + validate cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct EventsSinceResult {
    pub page: EventPage,
    /// Echo of the request cursor (`after`).
    pub after: u64,
    pub limit: u32,
    pub http_status: u16,
}

// ── Errors surfaced as structured client failures ───────────────────────────

// ── Client ──────────────────────────────────────────────────────────────────

/// Read-only `events_since` poller. Generic over transport for offline tests.
#[derive(Debug, Clone)]
pub struct NutsEventsClient<T> {
    config: NutsEventsConfig,
    transport: T,
}

impl<T> NutsEventsClient<T> {
    pub fn new(config: NutsEventsConfig, transport: T) -> Self {
        Self { config, transport }
    }

    pub fn config(&self) -> &NutsEventsConfig {
        &self.config
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Pure request construction: validates `limit`, never touches the network.
    pub fn prepare_events_since(&self, after: u64, limit: u32) -> Result<PreparedEventsSince> {
        validate_limit(limit)?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": new_request_id(),
            "method": "tools/call",
            "params": {
                "name": "events_since",
                "arguments": {
                    "after": after,
                    "limit": limit
                }
            }
        });
        Ok(PreparedEventsSince {
            url: self.config.request_url.clone(),
            timeout_ms: self.config.timeout_ms,
            max_response_bytes: self.config.max_response_bytes,
            after,
            limit,
            body,
        })
    }

    /// `events_since(after, limit)` using the configured default limit.
    pub fn events_since_default(&self, after: u64) -> Result<EventsSinceResult>
    where
        T: EventsTransport,
    {
        self.events_since(after, self.config.default_limit)
    }
}

impl<T: EventsTransport> NutsEventsClient<T> {
    /// Poll `events_since` and return a validated page.
    ///
    /// On `429`, returns an error wrapping [`RateLimited`] guidance — the
    /// caller's durable cursor must not advance. Transient 5xx and transport
    /// errors leave the cursor unchanged (caller retries with the same `after`).
    pub fn events_since(&self, after: u64, limit: u32) -> Result<EventsSinceResult> {
        let prepared = self.prepare_events_since(after, limit)?;
        let raw = self
            .transport
            .execute(&prepared)
            .context("events_since transport")?;
        interpret_response(
            &raw,
            after,
            limit,
            &self.config.expected_instance,
            self.config.max_response_bytes,
        )
    }
}

// ── Parse + validate ────────────────────────────────────────────────────────

/// Interpret a raw HTTP response into a validated page (pure; offline-testable).
pub fn interpret_response(
    raw: &RawHttpResponse,
    after: u64,
    limit: u32,
    expected_instance: &str,
    max_response_bytes: usize,
) -> Result<EventsSinceResult> {
    if raw.body.len() > max_response_bytes {
        bail!(
            "events_since body length {} exceeds max_response_bytes {max_response_bytes}",
            raw.body.len()
        );
    }

    reject_lossy_content_type(raw.content_type.as_deref())?;

    // HTML / SSE frames often arrive as 200 with a non-JSON body.
    if looks_like_html(&raw.body) {
        bail!(
            "events_since received HTML (legacy web surface), not an EventPage; \
             server is missing the contract route — do not fall back to /newest or SSE. \
             ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }
    if looks_like_sse_frame(&raw.body) {
        bail!(
            "events_since received an SSE frame stream body; GET /events is a lossy live \
             tail and is not reliable replay. Cursor left unchanged. \
             ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }

    match raw.status {
        200..=299 => {}
        404 | 405 => {
            bail!(
                "events_since HTTP {}: route missing on server (capability audit: events_since \
                 not implemented). Cursor unchanged; do not poll newest/SSE as a substitute. \
                 body={}",
                raw.status,
                body_preview(&raw.body, 256)
            );
        }
        429 => {
            let preview = body_preview(&raw.body, 256);
            bail!(
                "events_since rate limited (429); retry later with the same after={after}; \
                 retry_after_secs={:?}; body={preview}",
                raw.retry_after_secs
            );
        }
        400..=499 => {
            bail!(
                "events_since client error HTTP {}: {}",
                raw.status,
                body_preview(&raw.body, 256)
            );
        }
        500..=599 => {
            bail!(
                "events_since server error HTTP {}: {}",
                raw.status,
                body_preview(&raw.body, 256)
            );
        }
        other => bail!("events_since unexpected HTTP status {other}"),
    }

    let page = parse_event_page_bytes(&raw.body)?;
    validate_event_page(&page, after, limit, expected_instance)?;

    Ok(EventsSinceResult {
        page,
        after,
        limit,
        http_status: raw.status,
    })
}

/// Parse JSON bytes into an [`EventPage`] (MCP result envelope or bare page).
pub fn parse_event_page_bytes(bytes: &[u8]) -> Result<EventPage> {
    let value: Value =
        serde_json::from_slice(bytes).context("events_since response is not JSON")?;
    parse_event_page_value(&value)
}

/// Accept either a bare `EventPage` object or a JSON-RPC / MCP wrapper:
/// `{ "result": { …page } }`, `{ "result": { "content": [{ "text": "{…page}" }] } }`.
///
/// Explicitly rejects known **legacy / lossy** shapes so they cannot be
/// mistaken for a cursored page: SSE hello, bare tagged `Event`s, `newest`
/// item arrays, MCP `isError` unknown-tool results, tools/list.
pub fn parse_event_page_value(value: &Value) -> Result<EventPage> {
    if let Some(err) = value.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| "");
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        if code == -32601
            || msg.contains("method not found")
            || msg.contains("unknown tool")
            || msg.contains("events_since")
        {
            bail!(
                "events_since unavailable on server (JSON-RPC {code}: {msg}); \
                 capability audit: tool/route missing. Cursor unchanged; \
                 do not fall back to newest or SSE. ({SERVER_EVENTS_SINCE_STATUS})"
            );
        }
        bail!("events_since JSON-RPC error: {err}");
    }

    // JSON-RPC / MCP result envelope first (so isError is not misread as a page).
    if let Some(result) = value.get("result") {
        return parse_mcp_or_page_result(result);
    }

    // Bare page.
    if is_event_page_shape(value) {
        return deserialize_page(value);
    }

    reject_legacy_shape(value)?;
    bail!("events_since response missing EventPage fields and JSON-RPC result")
}

fn parse_mcp_or_page_result(result: &Value) -> Result<EventPage> {
    // MCP tools/call error form used by reference nutnews mcp.rs:
    // { "content": [{"type":"text","text":"unknown tool 'events_since'"}], "isError": true }
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let text = mcp_content_text(result).unwrap_or_else(|| result.to_string());
        if text.contains("unknown tool") || text.contains("events_since") {
            bail!(
                "events_since tool missing on server ({text}); capability audit High gap. \
                 Cursor unchanged; refusing silent newest/SSE fallback. \
                 ({SERVER_EVENTS_SINCE_STATUS})"
            );
        }
        bail!("events_since tool returned isError: {text}");
    }

    if is_event_page_shape(result) {
        return deserialize_page(result);
    }

    // MCP success content envelope: result.content[].text holds JSON page.
    if let Some(text) = mcp_content_text(result) {
        if text.contains("unknown tool") {
            bail!(
                "events_since tool missing on server ({text}); capability audit High gap. \
                 Cursor unchanged; refusing silent newest/SSE fallback. \
                 ({SERVER_EVENTS_SINCE_STATUS})"
            );
        }
        let page_val: Value = serde_json::from_str(&text).with_context(|| {
            format!(
                "MCP content text is not a JSON EventPage (got preview {:?}); \
                 if this is a newest/front_page payload, it is not reliable replay",
                text.chars().take(80).collect::<String>()
            )
        })?;
        if is_event_page_shape(&page_val) {
            return deserialize_page(&page_val);
        }
        reject_legacy_shape(&page_val)?;
        bail!("MCP content JSON is not an EventPage");
    }

    reject_legacy_shape(result)?;
    // Fall through: try deserialize for precise missing-field errors.
    deserialize_page(result)
}

fn is_event_page_shape(value: &Value) -> bool {
    value.get("ledger_floor").is_some()
        && value.get("ledger_height").is_some()
        && value.get("gap").is_some()
        && value.get("events").is_some()
}

/// Refuse shapes that exist on today's server but are not cursored replay.
fn reject_legacy_shape(value: &Value) -> Result<()> {
    // SSE hello: {"type":"hello","ledger_height":N}
    if value.get("type").and_then(Value::as_str) == Some("hello") {
        bail!(
            "refusing SSE hello payload as EventPage; /events is a lossy live tail, \
             not events_since. ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }

    // Bare tagged Event (SSE data line / ledger row without seq wrapper).
    if let Some(t) = value.get("type").and_then(Value::as_str)
        && matches!(
            t,
            "item_submitted"
                | "comment_posted"
                | "vote_cast"
                | "item_classified"
                | "source_fetched"
                | "source_provided"
                | "handle_claimed"
                | "about_set"
                | "item_retitled"
                | "comment_edited"
                | "comment_flagged"
                | "comment_unflagged"
        )
    {
        bail!(
            "refusing bare Nuts Event (type={t}) without seq wrapper / EventPage envelope; \
             SSE bodies are not reliable replay. ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }

    // newest / front_page: JSON array of Item projections.
    if let Some(arr) = value.as_array() {
        let looks_like_items = arr.iter().take(3).all(|row| {
            row.get("title").is_some() && row.get("by").is_some() && row.get("id").is_some()
        });
        if looks_like_items {
            bail!(
                "refusing newest/front_page Item array as EventPage; projections omit \
                 ledger seq/gap and can silently lose mentions under concurrency. \
                 ({SERVER_EVENTS_SINCE_STATUS})"
            );
        }
        bail!("refusing bare JSON array as EventPage");
    }

    // tools/list style.
    if value.get("tools").is_some() {
        bail!("refusing tools/list payload as EventPage");
    }

    Ok(())
}

fn mcp_content_text(result: &Value) -> Option<String> {
    let content = result.get("content")?.as_array()?;
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = item.get("text").and_then(Value::as_str)
        {
            return Some(text.to_string());
        }
    }
    None
}

fn deserialize_page(value: &Value) -> Result<EventPage> {
    // Guard again in case a partial page sneaks through.
    if value.get("type").and_then(Value::as_str) == Some("hello") {
        bail!("refusing SSE hello payload as EventPage");
    }

    let instance = value
        .get("instance")
        .and_then(Value::as_str)
        .context("EventPage.instance missing or not a string")?
        .to_string();
    let ledger_floor = value
        .get("ledger_floor")
        .and_then(Value::as_u64)
        .context("EventPage.ledger_floor missing or not a u64")?;
    let ledger_height = value
        .get("ledger_height")
        .and_then(Value::as_u64)
        .context("EventPage.ledger_height missing or not a u64")?;
    let gap = value
        .get("gap")
        .and_then(Value::as_bool)
        .context("EventPage.gap missing or not a bool")?;
    let events_val = value
        .get("events")
        .context("EventPage.events missing")?
        .as_array()
        .context("EventPage.events not an array")?;

    let mut events = Vec::with_capacity(events_val.len());
    for (i, entry) in events_val.iter().enumerate() {
        let seq = entry
            .get("seq")
            .and_then(Value::as_u64)
            .with_context(|| format!("events[{i}].seq missing or not a u64"))?;
        let event = entry
            .get("event")
            .cloned()
            .with_context(|| format!("events[{i}].event missing"))?;
        if !event.is_object() {
            bail!("events[{i}].event must be a JSON object");
        }
        // Ensure the body is a recognizable contract or real Event shape.
        normalize_event_fields(&event)
            .with_context(|| format!("events[{i}].event field shape invalid"))?;
        events.push(LedgerEvent { seq, event });
    }

    Ok(EventPage {
        instance,
        ledger_floor,
        ledger_height,
        gap,
        events,
    })
}

fn reject_lossy_content_type(content_type: Option<&str>) -> Result<()> {
    let Some(ct) = content_type else {
        return Ok(());
    };
    let ct = ct.to_ascii_lowercase();
    if ct.contains("text/event-stream") {
        bail!(
            "events_since Content-Type is text/event-stream (SSE /events); \
             lossy live tail is not reliable replay. ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }
    if ct.contains("text/html") {
        bail!(
            "events_since Content-Type is text/html (legacy web page); \
             not an EventPage. ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }
    Ok(())
}

fn looks_like_html(body: &[u8]) -> bool {
    let start = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    let head = &body[start..body.len().min(start + 64)];
    let s = String::from_utf8_lossy(head).to_ascii_lowercase();
    s.starts_with("<!doctype") || s.starts_with("<html") || s.starts_with("<head")
}

fn looks_like_sse_frame(body: &[u8]) -> bool {
    let s = String::from_utf8_lossy(body);
    // Typical SSE: "data: {...}\n\n" or "event: ...\ndata:"
    let trimmed = s.trim_start();
    trimmed.starts_with("data:")
        || trimmed.starts_with("event:")
        || trimmed.starts_with("id:")
        || (trimmed.contains("\ndata:") && trimmed.lines().any(|l| l.starts_with("data:")))
}

/// Validate a parsed page against the request cursor/limit and expected instance.
///
/// Enforces contract §2 / §3 / §6 / §7 client-side:
/// - instance match
/// - `events.len() ≤ limit`
/// - unique strictly ascending positive `seq`
/// - every `seq > after` and within `[ledger_floor, ledger_height]` (when floor > 0)
/// - `gap=true` ⇒ no speculative fill-in events
/// - `gap=false` ⇒ contiguous sequences and no silent purge behind the cursor
pub fn validate_event_page(
    page: &EventPage,
    after: u64,
    limit: u32,
    expected_instance: &str,
) -> Result<()> {
    validate_limit(limit)?;
    let expected = normalize_instance(expected_instance)?;
    let got = normalize_instance(&page.instance)?;
    if got != expected {
        bail!("EventPage.instance {got:?} does not match expected {expected:?}");
    }

    if page.events.len() > limit as usize {
        bail!(
            "EventPage returned {} events but limit was {limit}",
            page.events.len()
        );
    }
    if page.events.len() > MAX_EVENTS_LIMIT as usize {
        bail!("EventPage exceeds absolute MAX_EVENTS_LIMIT ({MAX_EVENTS_LIMIT})");
    }

    if page.ledger_floor > 0 && page.ledger_height > 0 && page.ledger_height < page.ledger_floor {
        bail!(
            "ledger_height {} < ledger_floor {}",
            page.ledger_height,
            page.ledger_floor
        );
    }

    // Silent purge detection: server must set gap when after is behind floor.
    if after > 0
        && page.ledger_floor > 1
        && after < page.ledger_floor.saturating_sub(1)
        && !page.gap
    {
        bail!(
            "cursor after={after} is behind ledger_floor={} without gap=true \
             (refusing to pretend continuity)",
            page.ledger_floor
        );
    }

    if page.gap && !page.events.is_empty() {
        // Contract: gap pages are empty or omit speculative fill-in.
        bail!(
            "gap=true page must not carry {} speculative events",
            page.events.len()
        );
    }

    let mut seen = BTreeSet::new();
    let mut prev: Option<u64> = None;
    for (i, ev) in page.events.iter().enumerate() {
        if ev.seq == 0 {
            bail!("events[{i}].seq must be >= 1 (0 is reserved)");
        }
        if ev.seq <= after {
            bail!(
                "events[{i}].seq {} is not strictly greater than after={after}",
                ev.seq
            );
        }
        if page.ledger_height > 0 && ev.seq > page.ledger_height {
            bail!(
                "events[{i}].seq {} exceeds ledger_height {}",
                ev.seq,
                page.ledger_height
            );
        }
        if page.ledger_floor > 0 && ev.seq < page.ledger_floor {
            bail!(
                "events[{i}].seq {} is below ledger_floor {}",
                ev.seq,
                page.ledger_floor
            );
        }
        if !seen.insert(ev.seq) {
            bail!("duplicate seq {} inside EventPage", ev.seq);
        }
        if let Some(p) = prev {
            if ev.seq <= p {
                bail!("events not strictly ascending: seq {} follows {p}", ev.seq);
            }
            if !page.gap && ev.seq != p + 1 {
                bail!(
                    "non-contiguous seq {} after {p} with gap=false \
                     (hole would freeze the bridge cursor)",
                    ev.seq
                );
            }
        }
        prev = Some(ev.seq);
    }

    // Contiguity with the cursor: when history still covers the next sequence,
    // a non-gap page must start exactly there (after=0 → ledger_floor).
    if !page.gap {
        if let Some(first) = page.events.first() {
            let expected_first = if after == 0 {
                if page.ledger_floor > 0 {
                    page.ledger_floor
                } else {
                    first.seq
                }
            } else {
                after + 1
            };
            let history_covers_expected =
                page.ledger_floor == 0 || expected_first >= page.ledger_floor;
            if history_covers_expected && first.seq != expected_first {
                bail!(
                    "gap=false page must start at contiguous seq {expected_first}; got {}",
                    first.seq
                );
            }
        }
    }

    Ok(())
}

/// Pure helper: should the bridge enter reconcile instead of sequential apply?
pub fn page_requires_reconcile(page: &EventPage) -> bool {
    page.gap
}

/// Pure helper: next `after` argument after successfully processing a contiguous
/// non-gap page. Returns `None` when the page must not advance the cursor
/// (gap, or empty with no work). For empty non-gap pages the cursor stays put
/// (caller keeps `last_seq`).
pub fn suggested_cursor_after_process(last_seq: u64, page: &EventPage) -> CursorAdvance {
    if page.gap {
        return CursorAdvance::Reconcile {
            ledger_floor: page.ledger_floor,
            ledger_height: page.ledger_height,
        };
    }
    match page.max_seq() {
        Some(max) if max > last_seq => CursorAdvance::To { last_seq: max },
        _ => CursorAdvance::Unchanged { last_seq },
    }
}

/// Cursor advice derived from a validated page (does not itself persist).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorAdvance {
    Unchanged {
        last_seq: u64,
    },
    To {
        last_seq: u64,
    },
    Reconcile {
        ledger_floor: u64,
        ledger_height: u64,
    },
}

// ── Validation helpers ──────────────────────────────────────────────────────

pub fn validate_limit(limit: u32) -> Result<()> {
    if !(MIN_EVENTS_LIMIT..=MAX_EVENTS_LIMIT).contains(&limit) {
        bail!("events_since limit must be in {MIN_EVENTS_LIMIT}..={MAX_EVENTS_LIMIT}; got {limit}");
    }
    Ok(())
}

fn validate_timeout_ms(timeout_ms: u64) -> Result<()> {
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        bail!("timeout_ms must be in {MIN_TIMEOUT_MS}..={MAX_TIMEOUT_MS}; got {timeout_ms}");
    }
    Ok(())
}

fn validate_max_response_bytes(n: usize) -> Result<()> {
    if n == 0 || n > MAX_RESPONSE_BYTES_CEILING {
        bail!("max_response_bytes must be in 1..={MAX_RESPONSE_BYTES_CEILING}; got {n}");
    }
    Ok(())
}

fn normalize_instance(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("instance id must be non-empty");
    }
    if trimmed.len() > MAX_INSTANCE_BYTES {
        bail!("instance id exceeds MAX_INSTANCE_BYTES ({MAX_INSTANCE_BYTES})");
    }
    if trimmed.chars().any(|c| c.is_control()) {
        bail!("instance id contains control characters");
    }
    Ok(trimmed.to_string())
}

/// Origin = scheme + host + optional port. No path, query, fragment, or userinfo.
fn normalize_origin(raw: &str) -> Result<String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        bail!("origin must be non-empty");
    }
    let url = reqwest::Url::parse(raw).context("origin is not a valid URL")?;
    match url.scheme() {
        "https" | "http" => {}
        other => bail!("origin scheme must be https or http; got {other}"),
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("origin must not carry userinfo credentials");
    }
    if url.query().is_some() {
        bail!("origin must not carry a query string");
    }
    if url.fragment().is_some() {
        bail!("origin must not carry a fragment");
    }
    // Path must be empty or `/` only — MCP path is configured separately.
    let path = url.path();
    if path != "/" && !path.is_empty() {
        bail!("origin must not include a path (got {path:?}); set mcp_path separately");
    }
    let host = url.host_str().context("origin must include a host")?;
    if host.is_empty() {
        bail!("origin host is empty");
    }
    // Block obvious SSRF tricks: bare IP literals are allowed only as an
    // explicit pin (operator choice), but reject empty and whitespace hosts.
    if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("origin host contains illegal characters");
    }

    let mut origin = format!("{}://{}", url.scheme(), host);
    if let Some(port) = url.port() {
        origin.push(':');
        origin.push_str(&port.to_string());
    }
    Ok(origin)
}

fn normalize_mcp_path(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("mcp_path must be non-empty");
    }
    if !raw.starts_with('/') {
        bail!("mcp_path must be an absolute path starting with /");
    }
    if raw.contains("://") {
        bail!("mcp_path must not be an absolute URL");
    }
    if raw.contains("..") {
        bail!("mcp_path must not contain '..' segments");
    }
    if raw.contains('?') || raw.contains('#') {
        bail!("mcp_path must not contain query or fragment");
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("mcp_path contains illegal characters");
    }
    // Normalize trailing slash away except for root.
    let path = if raw.len() > 1 {
        raw.trim_end_matches('/').to_string()
    } else {
        raw.to_string()
    };

    // Capability audit: never pin the poller to lossy surfaces.
    if path == FORBIDDEN_SSE_PATH || path.starts_with(&format!("{FORBIDDEN_SSE_PATH}/")) {
        bail!(
            "mcp_path {path:?} is the SSE live tail; it is not events_since replay \
             and must not be used as the poll URL ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }
    if path == FORBIDDEN_NEWEST_PATH || path.starts_with(&format!("{FORBIDDEN_NEWEST_PATH}/")) {
        bail!(
            "mcp_path {path:?} is the newest projection; it cannot provide a durable \
             ledger cursor and must not be used as the poll URL ({SERVER_EVENTS_SINCE_STATUS})"
        );
    }
    if path == "/" {
        bail!("mcp_path must not be the site root HTML surface");
    }

    Ok(path)
}

fn assert_url_pinned_to_origin(request_url: &str, origin: &str) -> Result<()> {
    let url = reqwest::Url::parse(request_url).context("request_url parse")?;
    let origin_url = reqwest::Url::parse(origin).context("origin parse")?;
    if url.scheme() != origin_url.scheme() {
        bail!("request_url scheme diverges from pinned origin");
    }
    if url.host_str() != origin_url.host_str() {
        bail!("request_url host diverges from pinned origin");
    }
    if url.port() != origin_url.port() {
        bail!("request_url port diverges from pinned origin");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("request_url must not carry credentials");
    }
    Ok(())
}

fn assert_https_or_http_absolute(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).context("request URL parse")?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        bail!("events_since URL scheme must be http or https");
    }
    if parsed.host_str().is_none() {
        bail!("events_since URL must have a host");
    }
    Ok(())
}

fn parse_retry_after_secs(raw: &str) -> Option<u64> {
    // Prefer delta-seconds (HTTP-date ignored for simplicity in this primitive).
    raw.trim().parse::<u64>().ok()
}

fn body_preview(body: &[u8], max: usize) -> String {
    let end = body.len().min(max);
    let s = String::from_utf8_lossy(&body[..end]);
    if body.len() > max {
        format!("{s}…")
    } else {
        s.into_owned()
    }
}

fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ── Mock transport (tests + offline integration harnesses) ──────────────────

/// FIFO mock transport for offline tests. Records every prepared request.
///
/// Alias: [`MockEventsTransport`].
#[derive(Debug, Default)]
pub struct FifoMockTransport {
    responses: std::sync::Mutex<std::collections::VecDeque<Result<RawHttpResponse, String>>>,
    pub seen: std::sync::Mutex<Vec<PreparedEventsSince>>,
}

impl FifoMockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue_ok_page(&self, page: &EventPage) {
        let body = serde_json::to_vec(page).expect("serialize page");
        self.enqueue_raw(Ok(RawHttpResponse::json_ok(body)));
    }

    pub fn enqueue_jsonrpc_page(&self, page: &EventPage) {
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": "1",
            "result": page
        }))
        .expect("serialize");
        self.enqueue_raw(Ok(RawHttpResponse::json_ok(body)));
    }

    pub fn enqueue_mcp_text_page(&self, page: &EventPage) {
        let text = serde_json::to_string(page).expect("serialize");
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": "1",
            "result": {
                "content": [{ "type": "text", "text": text }],
                "isError": false
            }
        }))
        .expect("serialize");
        self.enqueue_raw(Ok(RawHttpResponse::json_ok(body)));
    }

    pub fn enqueue_raw(&self, response: Result<RawHttpResponse, String>) {
        self.responses
            .lock()
            .expect("fifo responses poisoned")
            .push_back(response);
    }

    pub fn enqueue_status_body(
        &self,
        status: u16,
        body: impl Into<Vec<u8>>,
        retry_after_secs: Option<u64>,
    ) {
        self.enqueue_raw(Ok(RawHttpResponse {
            status,
            body: body.into(),
            retry_after_secs,
            content_type: Some("application/json".into()),
        }));
    }

    pub fn enqueue_with_content_type(
        &self,
        status: u16,
        body: impl Into<Vec<u8>>,
        content_type: &str,
    ) {
        self.enqueue_raw(Ok(RawHttpResponse {
            status,
            body: body.into(),
            retry_after_secs: None,
            content_type: Some(content_type.to_ascii_lowercase()),
        }));
    }

    pub fn seen_requests(&self) -> Vec<PreparedEventsSince> {
        self.seen.lock().expect("fifo seen poisoned").clone()
    }
}

impl EventsTransport for FifoMockTransport {
    fn execute(&self, request: &PreparedEventsSince) -> Result<RawHttpResponse> {
        self.seen
            .lock()
            .expect("fifo seen poisoned")
            .push(request.clone());
        let next = self
            .responses
            .lock()
            .expect("fifo responses poisoned")
            .pop_front()
            .unwrap_or_else(|| Err("FifoMockTransport: no queued response".into()));
        match next {
            Ok(r) => Ok(r),
            Err(e) => bail!(e),
        }
    }
}

/// Preferred offline mock name.
pub type MockEventsTransport = FifoMockTransport;

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract-shaped page (docs/NUTS_EVENT_CONTRACT.md §13.1).
    fn sample_page() -> EventPage {
        EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 105,
            gap: false,
            events: vec![
                LedgerEvent {
                    seq: 101,
                    event: json!({
                        "kind": "comment",
                        "item_id": 9001,
                        "comment_id": 501,
                        "actor": "alice",
                        "text": "hey @steve — thoughts on simplicity?",
                        "reply_to": null
                    }),
                },
                LedgerEvent {
                    seq: 102,
                    event: json!({
                        "kind": "vote",
                        "item_id": 9001,
                        "actor": "bob",
                        "text": ""
                    }),
                },
            ],
        }
    }

    /// Real Nuts `Event` tagged shapes (research/nuts-news/source/src/events.rs).
    fn real_event_page() -> EventPage {
        EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 220,
            gap: false,
            events: vec![
                LedgerEvent {
                    seq: 200,
                    event: json!({
                        "type": "item_submitted",
                        "id": 36,
                        "by": "alice",
                        "title": "Simplicity",
                        "url": "https://example.com",
                        "text": null,
                        "ts": 1_700_000_000u64
                    }),
                },
                LedgerEvent {
                    seq: 201,
                    event: json!({
                        "type": "comment_posted",
                        "id": 501,
                        "item": 36,
                        "parent": null,
                        "by": "bob",
                        "text": "hey @steve — thoughts?",
                        "ts": 1_700_000_010u64
                    }),
                },
                LedgerEvent {
                    seq: 202,
                    event: json!({
                        "type": "vote_cast",
                        "by": "carol",
                        "target": 36,
                        "ts": 1_700_000_020u64
                    }),
                },
            ],
        }
    }

    fn client_with(mock: FifoMockTransport) -> NutsEventsClient<FifoMockTransport> {
        NutsEventsClient::new(NutsEventsConfig::default(), mock)
    }

    #[test]
    fn default_config_pins_production_origin() {
        let cfg = NutsEventsConfig::default();
        assert_eq!(cfg.origin, "https://news.nuts.services");
        assert_eq!(cfg.mcp_path, "/mcp");
        assert_eq!(cfg.request_url, "https://news.nuts.services/mcp");
        assert_eq!(cfg.expected_instance, DEFAULT_INSTANCE);
        assert_eq!(cfg.default_limit, DEFAULT_EVENTS_LIMIT);
    }

    #[test]
    fn origin_rejects_credentials_query_path_and_bad_scheme() {
        assert!(
            NutsEventsConfig::pin(
                "https://user:pass@news.nuts.services",
                "/mcp",
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
        assert!(
            NutsEventsConfig::pin(
                "https://news.nuts.services?x=1",
                "/mcp",
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
        assert!(
            NutsEventsConfig::pin(
                "https://news.nuts.services/other",
                "/mcp",
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
        assert!(
            NutsEventsConfig::pin(
                "ftp://news.nuts.services",
                "/mcp",
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
    }

    #[test]
    fn mcp_path_rejects_traversal_and_absolute_urls() {
        assert!(
            NutsEventsConfig::pin(
                DEFAULT_ORIGIN,
                "../mcp",
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
        assert!(
            NutsEventsConfig::pin(
                DEFAULT_ORIGIN,
                "https://evil.example/mcp",
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
        assert!(
            NutsEventsConfig::pin(
                DEFAULT_ORIGIN,
                "mcp",
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
    }

    #[test]
    fn limit_out_of_range_is_client_error() {
        assert!(validate_limit(0).is_err());
        assert!(validate_limit(501).is_err());
        assert!(validate_limit(1).is_ok());
        assert!(validate_limit(500).is_ok());

        let client = client_with(FifoMockTransport::new());
        assert!(client.prepare_events_since(0, 0).is_err());
        assert!(client.prepare_events_since(0, 501).is_err());
    }

    #[test]
    fn prepare_events_since_is_pinned_and_read_only() {
        let client = client_with(FifoMockTransport::new());
        let prep = client.prepare_events_since(100, 50).unwrap();
        assert_eq!(prep.url, "https://news.nuts.services/mcp");
        assert_eq!(prep.after, 100);
        assert_eq!(prep.limit, 50);
        assert_eq!(prep.body["method"], "tools/call");
        assert_eq!(prep.body["params"]["name"], "events_since");
        assert_eq!(prep.body["params"]["arguments"]["after"], 100);
        assert_eq!(prep.body["params"]["arguments"]["limit"], 50);
        // No auth material in the prepared body.
        let s = prep.body.to_string();
        assert!(!s.contains("Bearer"));
        assert!(!s.contains("token"));
        assert!(!s.contains("comment"));
        assert!(!s.contains("submit"));
    }

    #[test]
    fn happy_path_page_from_contract_example() {
        let mock = FifoMockTransport::new();
        mock.enqueue_ok_page(&sample_page());
        let client = client_with(mock);
        let result = client.events_since(100, 50).unwrap();
        assert!(!result.page.gap);
        assert_eq!(result.page.events.len(), 2);
        assert_eq!(result.page.events[0].seq, 101);
        assert_eq!(result.page.events[1].seq, 102);
        assert_eq!(result.after, 100);
        match suggested_cursor_after_process(100, &result.page) {
            CursorAdvance::To { last_seq } => assert_eq!(last_seq, 102),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn jsonrpc_and_mcp_envelopes_parse() {
        let page = sample_page();
        let mock = FifoMockTransport::new();
        mock.enqueue_jsonrpc_page(&page);
        mock.enqueue_mcp_text_page(&page);
        let client = client_with(mock);
        assert_eq!(client.events_since(100, 50).unwrap().page.events.len(), 2);
        assert_eq!(client.events_since(100, 50).unwrap().page.events.len(), 2);
    }

    #[test]
    fn caught_up_empty_page_is_valid() {
        let page = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 102,
            gap: false,
            events: vec![],
        };
        validate_event_page(&page, 102, 50, DEFAULT_INSTANCE).unwrap();
        match suggested_cursor_after_process(102, &page) {
            CursorAdvance::Unchanged { last_seq } => assert_eq!(last_seq, 102),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn gap_page_refuses_continuity_and_cursor_advance() {
        let page = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 5000,
            ledger_height: 5100,
            gap: true,
            events: vec![],
        };
        validate_event_page(&page, 50, 50, DEFAULT_INSTANCE).unwrap();
        assert!(page_requires_reconcile(&page));
        match suggested_cursor_after_process(50, &page) {
            CursorAdvance::Reconcile {
                ledger_floor,
                ledger_height,
            } => {
                assert_eq!(ledger_floor, 5000);
                assert_eq!(ledger_height, 5100);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn gap_true_with_events_is_rejected() {
        let mut page = sample_page();
        page.gap = true;
        page.ledger_floor = 5000;
        page.ledger_height = 5100;
        assert!(validate_event_page(&page, 50, 50, DEFAULT_INSTANCE).is_err());
    }

    #[test]
    fn silent_purge_without_gap_bit_is_rejected() {
        let page = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 5000,
            ledger_height: 5100,
            gap: false,
            events: vec![],
        };
        let err = validate_event_page(&page, 50, 50, DEFAULT_INSTANCE).unwrap_err();
        assert!(
            err.to_string().contains("gap=true"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn non_contiguous_and_duplicate_seq_rejected() {
        let page = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 200,
            gap: false,
            events: vec![
                LedgerEvent {
                    seq: 101,
                    event: json!({ "kind": "comment", "item_id": 1, "actor": "a", "text": "x" }),
                },
                LedgerEvent {
                    seq: 103,
                    event: json!({ "kind": "comment", "item_id": 1, "actor": "a", "text": "y" }),
                },
            ],
        };
        assert!(validate_event_page(&page, 100, 50, DEFAULT_INSTANCE).is_err());

        let dup = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 200,
            gap: false,
            events: vec![
                LedgerEvent {
                    seq: 101,
                    event: json!({ "kind": "x", "item_id": 1, "actor": "a", "text": "" }),
                },
                LedgerEvent {
                    seq: 101,
                    event: json!({ "kind": "x", "item_id": 1, "actor": "a", "text": "" }),
                },
            ],
        };
        assert!(validate_event_page(&dup, 100, 50, DEFAULT_INSTANCE).is_err());
    }

    #[test]
    fn seq_must_be_above_cursor_and_positive() {
        let page = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 50,
            gap: false,
            events: vec![LedgerEvent {
                seq: 100,
                event: json!({ "kind": "x", "item_id": 1, "actor": "a", "text": "" }),
            }],
        };
        assert!(validate_event_page(&page, 100, 50, DEFAULT_INSTANCE).is_err());

        let zero = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 0,
            ledger_height: 0,
            gap: false,
            events: vec![LedgerEvent {
                seq: 0,
                event: json!({ "kind": "x", "item_id": 1, "actor": "a", "text": "" }),
            }],
        };
        assert!(validate_event_page(&zero, 0, 50, DEFAULT_INSTANCE).is_err());
    }

    #[test]
    fn instance_mismatch_and_oversize_page_rejected() {
        let mut page = sample_page();
        page.instance = "evil.example".into();
        assert!(validate_event_page(&page, 100, 50, DEFAULT_INSTANCE).is_err());

        page = sample_page();
        assert!(validate_event_page(&page, 100, 1, DEFAULT_INSTANCE).is_err());
    }

    #[test]
    fn oversized_body_rejected_before_parse() {
        let raw = RawHttpResponse {
            status: 200,
            body: vec![b'x'; 64],
            retry_after_secs: None,
            content_type: Some("application/json".into()),
        };
        let err = interpret_response(&raw, 0, 50, DEFAULT_INSTANCE, 32).unwrap_err();
        assert!(err.to_string().contains("max_response_bytes"));
    }

    #[test]
    fn rate_limit_429_does_not_parse_as_page() {
        let mock = FifoMockTransport::new();
        mock.enqueue_status_body(429, br#"{"error":"slow down"}"#, Some(12));
        let client = client_with(mock);
        let err = client.events_since(100, 50).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("429"), "{msg}");
        assert!(msg.contains("after=100"), "{msg}");
    }

    #[test]
    fn http_5xx_and_4xx_are_errors() {
        let mock = FifoMockTransport::new();
        mock.enqueue_status_body(503, b"unavailable", None);
        mock.enqueue_status_body(400, b"bad limit", None);
        let client = client_with(mock);
        assert!(
            client
                .events_since(0, 10)
                .unwrap_err()
                .to_string()
                .contains("503")
        );
        assert!(
            client
                .events_since(0, 10)
                .unwrap_err()
                .to_string()
                .contains("400")
        );
    }

    #[test]
    fn jsonrpc_error_object_rejected() {
        let mock = FifoMockTransport::new();
        mock.enqueue_status_body(
            200,
            br#"{"jsonrpc":"2.0","id":"1","error":{"code":-32602,"message":"invalid limit"}}"#,
            None,
        );
        let client = client_with(mock);
        let err = client.events_since(0, 10).unwrap_err();
        assert!(err.to_string().contains("JSON-RPC error"), "{err}");
    }

    #[test]
    fn after_zero_must_start_at_floor() {
        let page = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 10,
            ledger_height: 12,
            gap: false,
            events: vec![
                LedgerEvent {
                    seq: 11,
                    event: json!({ "kind": "x", "item_id": 1, "actor": "a", "text": "" }),
                },
                LedgerEvent {
                    seq: 12,
                    event: json!({ "kind": "x", "item_id": 1, "actor": "a", "text": "" }),
                },
            ],
        };
        assert!(validate_event_page(&page, 0, 50, DEFAULT_INSTANCE).is_err());

        let good = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 10,
            ledger_height: 11,
            gap: false,
            events: vec![LedgerEvent {
                seq: 10,
                event: json!({ "kind": "x", "item_id": 1, "actor": "a", "text": "" }),
            }],
        };
        validate_event_page(&good, 0, 50, DEFAULT_INSTANCE).unwrap();
    }

    #[test]
    fn request_uses_only_pinned_url_even_across_calls() {
        let mock = FifoMockTransport::new();
        mock.enqueue_ok_page(&EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 1,
            gap: false,
            events: vec![],
        });
        let client = client_with(mock);
        client.events_since(0, 10).unwrap();
        let seen = client.transport().seen_requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].url, "https://news.nuts.services/mcp");
        // Prepared body never includes alternate hosts.
        assert!(!seen[0].body.to_string().contains("http"));
    }

    #[test]
    fn timeout_and_body_bounds_validated_on_config() {
        assert!(
            NutsEventsConfig::pin(
                DEFAULT_ORIGIN,
                DEFAULT_MCP_PATH,
                DEFAULT_INSTANCE,
                1, // too small
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
        assert!(
            NutsEventsConfig::pin(
                DEFAULT_ORIGIN,
                DEFAULT_MCP_PATH,
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                0,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
        assert!(
            NutsEventsConfig::pin(
                DEFAULT_ORIGIN,
                DEFAULT_MCP_PATH,
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                MAX_RESPONSE_BYTES_CEILING + 1,
                DEFAULT_EVENTS_LIMIT,
            )
            .is_err()
        );
    }

    #[test]
    fn custom_origin_with_port_pins_correctly() {
        let cfg = NutsEventsConfig::pin(
            "http://127.0.0.1:9876",
            "/mcp",
            "local-test",
            DEFAULT_TIMEOUT_MS,
            DEFAULT_MAX_RESPONSE_BYTES,
            25,
        )
        .unwrap();
        assert_eq!(cfg.request_url, "http://127.0.0.1:9876/mcp");
        assert_eq!(cfg.expected_instance, "local-test");
        assert_eq!(cfg.default_limit, 25);
    }

    #[test]
    fn module_has_no_write_surface() {
        // Compile-time documentation test: the public client API is events_since
        // only. This runtime check guards against accidental public write names
        // showing up in prepared payloads.
        let client = client_with(FifoMockTransport::new());
        let prep = client.prepare_events_since(0, 10).unwrap();
        let dump = format!("{prep:?}");
        for forbidden in ["comment", "submit", "allow_writes", "bearer"] {
            assert!(
                !dump.to_ascii_lowercase().contains(forbidden),
                "prepared request leaked write-related token {forbidden:?}"
            );
        }
    }

    #[test]
    fn transport_error_leaves_caller_cursor_unchanged_pattern() {
        let mock = FifoMockTransport::new();
        mock.enqueue_raw(Err("connection dropped".into()));
        let client = client_with(mock);
        let last_seq = 42u64;
        let err = client.events_since(last_seq, 50).unwrap_err();
        // Top-level context is "events_since transport"; source is in the chain.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("connection dropped"),
            "full chain missing source: {chain}"
        );
        // Caller pattern: on error, keep last_seq (we only assert the request
        // used the same after).
        let seen = client.transport().seen_requests();
        assert_eq!(seen[0].after, last_seq);
    }

    // ── Capability audit: server-blocked / legacy refusal ───────────────────

    #[test]
    fn capability_status_declares_server_blocked() {
        let s = capability_status();
        assert!(s.contains("server_blocked"), "{s}");
        assert!(s.contains("newest"), "{s}");
        assert!(s.contains("/events"), "{s}");
        assert!(s.contains("events_since"), "{s}");
    }

    #[test]
    fn mcp_path_rejects_sse_and_newest_surfaces() {
        for path in ["/events", "/events/stream", "/newest", "/newest/"] {
            let err = NutsEventsConfig::pin(
                DEFAULT_ORIGIN,
                path,
                DEFAULT_INSTANCE,
                DEFAULT_TIMEOUT_MS,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_EVENTS_LIMIT,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("SSE")
                    || err.contains("newest")
                    || err.contains("live tail")
                    || err.contains("projection")
                    || err.contains("root"),
                "path {path}: {err}"
            );
        }
    }

    #[test]
    fn missing_events_since_tool_is_error_not_empty_page() {
        // Reference mcp.rs: tools/call unknown → isError content text.
        let body = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "result": {
                "content": [{"type": "text", "text": "unknown tool 'events_since'"}],
                "isError": true
            }
        });
        let mock = FifoMockTransport::new();
        mock.enqueue_status_body(200, serde_json::to_vec(&body).unwrap(), None);
        let client = client_with(mock);
        let err = client.events_since(10, 50).unwrap_err().to_string();
        assert!(
            err.contains("missing") || err.contains("unavailable") || err.contains("unknown tool"),
            "{err}"
        );
        assert!(
            err.contains("newest") || err.contains("SSE") || err.contains("fallback"),
            "{err}"
        );
        // Must not look like a successful catch-up empty page.
        assert!(!err.contains("CursorAdvance"));
    }

    #[test]
    fn jsonrpc_method_not_found_is_server_blocked() {
        let body = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "error": {"code": -32601, "message": "method not found: tools/call"}
        });
        let err = parse_event_page_value(&body).unwrap_err().to_string();
        assert!(
            err.contains("unavailable") || err.contains("method not found"),
            "{err}"
        );
    }

    #[test]
    fn http_404_route_missing_is_clear() {
        let mock = FifoMockTransport::new();
        mock.enqueue_status_body(404, b"not found", None);
        let client = client_with(mock);
        let err = client.events_since(0, 10).unwrap_err().to_string();
        assert!(err.contains("404"), "{err}");
        assert!(
            err.contains("missing") || err.contains("not implemented"),
            "{err}"
        );
    }

    #[test]
    fn sse_content_type_and_hello_payload_refused() {
        let mock = FifoMockTransport::new();
        mock.enqueue_with_content_type(
            200,
            br#"data: {"type":"hello","ledger_height":223}"#,
            "text/event-stream",
        );
        let client = client_with(mock);
        let err = client.events_since(0, 10).unwrap_err().to_string();
        assert!(err.contains("SSE") || err.contains("event-stream"), "{err}");

        let hello = json!({"type": "hello", "ledger_height": 223});
        let err = parse_event_page_value(&hello).unwrap_err().to_string();
        assert!(err.contains("SSE") || err.contains("hello"), "{err}");
    }

    #[test]
    fn bare_comment_posted_event_is_not_a_page() {
        let bare = json!({
            "type": "comment_posted",
            "id": 1,
            "item": 2,
            "by": "alice",
            "text": "@steve hi",
            "ts": 1
        });
        let err = parse_event_page_value(&bare).unwrap_err().to_string();
        assert!(
            err.contains("bare") || err.contains("SSE") || err.contains("replay"),
            "{err}"
        );
    }

    #[test]
    fn newest_item_array_refused_as_page() {
        let newest = json!([
            {
                "id": 1,
                "title": "A",
                "url": null,
                "domain": null,
                "by": "alice",
                "points": 1,
                "comments": 0,
                "category": null,
                "ts": 1
            },
            {
                "id": 2,
                "title": "B",
                "url": "https://x.test",
                "domain": "x.test",
                "by": "bob",
                "points": 3,
                "comments": 1,
                "category": "ai",
                "ts": 2
            }
        ]);
        let err = parse_event_page_value(&newest).unwrap_err().to_string();
        assert!(
            err.contains("newest") || err.contains("projection") || err.contains("Item"),
            "{err}"
        );
        assert!(
            err.contains("lose")
                || err.contains("silent")
                || err.contains("cursor")
                || err.contains("SERVER")
                || err.contains("ready"),
            "{err}"
        );
    }

    #[test]
    fn html_legacy_body_refused() {
        let raw = RawHttpResponse {
            status: 200,
            body: b"<!DOCTYPE html><html><body>newest</body></html>".to_vec(),
            retry_after_secs: None,
            content_type: Some("text/html; charset=utf-8".into()),
        };
        let err = interpret_response(&raw, 0, 50, DEFAULT_INSTANCE, 4096)
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTML") || err.contains("html"), "{err}");
    }

    #[test]
    fn real_event_shapes_normalize_for_bridge() {
        let page = real_event_page();
        validate_event_page(&page, 199, 50, DEFAULT_INSTANCE).unwrap();

        let item = normalize_event_fields(&page.events[0].event).unwrap();
        assert_eq!(item.kind, "item");
        assert_eq!(item.item_id, Some(36));
        assert_eq!(item.actor, "alice");
        assert!(item.title.as_deref() == Some("Simplicity") || item.text.contains("Simplicity"));

        let comment = normalize_event_fields(&page.events[1].event).unwrap();
        assert_eq!(comment.kind, "comment");
        assert_eq!(comment.comment_id, Some(501));
        assert_eq!(comment.item_id, Some(36));
        assert_eq!(comment.actor, "bob");
        assert!(comment.text.contains("@steve"));
        assert!(!comment.is_ignorable_noise());

        let vote = normalize_event_fields(&page.events[2].event).unwrap();
        assert_eq!(vote.kind, "vote");
        assert!(vote.is_ignorable_noise());
    }

    #[test]
    fn real_event_page_poll_succeeds_when_server_implements_contract() {
        let mock = FifoMockTransport::new();
        mock.enqueue_ok_page(&real_event_page());
        let client = client_with(mock);
        let result = client.events_since(199, 50).unwrap();
        assert_eq!(result.page.events.len(), 3);
        assert_eq!(result.page.events[1].event["type"], "comment_posted");
        match suggested_cursor_after_process(199, &result.page) {
            CursorAdvance::To { last_seq } => assert_eq!(last_seq, 202),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_public_newest_or_sse_poll_helpers() {
        // This module must not grow lossy fallbacks. Public names are audited
        // by string presence in source... but we only assert the prepared
        // request always targets events_since on the MCP path.
        let client = client_with(FifoMockTransport::new());
        let prep = client.prepare_events_since(0, 10).unwrap();
        assert!(prep.url.ends_with("/mcp"));
        assert_eq!(prep.body["params"]["name"], "events_since");
        assert_ne!(prep.url, format!("{DEFAULT_ORIGIN}{FORBIDDEN_SSE_PATH}"));
        assert_ne!(prep.url, format!("{DEFAULT_ORIGIN}{FORBIDDEN_NEWEST_PATH}"));
    }

    #[test]
    fn event_missing_type_and_kind_rejected_in_page() {
        let page = EventPage {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: 1,
            gap: false,
            events: vec![LedgerEvent {
                seq: 1,
                event: json!({ "text": "no type field", "by": "x" }),
            }],
        };
        let body = serde_json::to_vec(&page).unwrap();
        let err = parse_event_page_bytes(&body).unwrap_err();
        // Outer context is "events[0].event field shape invalid"; type/kind is nested.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("type") || chain.contains("kind"),
            "full chain missing type/kind: {chain}"
        );
    }
}
