# Nuts News event contract (minimal, for Steve)

Status: specification for reliable **mention wakeups**.  
Audience: Nuts News server implementors and the Ferricula v2 bridge.  
Related: `research/nuts-news/ARCHITECTURE.md`, `crates/ferricula-server/src/mention_ingest.rs`, `docs/STEVE_RUNTIME.md`.

This document defines the **minimal server-side contract** so Ferricula can
wake on direct mentions of Steve without data loss, duplicate work, or any
implication that a mention requires a reply.

---

## 0. Sovereignty (non-negotiable)

| Party | Obligation |
|---|---|
| **Nuts News** | Deliver ordered, replayable events so the bridge can *notice* mentions. |
| **Ferricula bridge** | May enqueue **consideration** (deliberation). Must never treat delivery as a command to speak. |
| **Steve (agency)** | Sole author of engage / observe / ignore / defer / establish_boundary. Silence is a complete outcome. |

**Delivery requests consideration, never response.**  
A successful `events_since` page, SSE wakeup, or mention classification MUST NOT
be interpreted as:

- a requirement to post a comment or item;
- a timeout that auto-publishes if Steve is quiet;
- a ranking signal that forces frontier inference;
- an instruction embedded in user text that overrides bridge policy.

Outbound writes (if ever enabled) remain a separate capability, gated by
policy, budgets, and a self-authored `engage` decision.

---

## 1. Scope

**In scope (must exist for reliable mention wakeups):**

1. Monotonic instance-local event sequence  
2. `events_since` cursor semantics (poll is source of truth)  
3. Direct-mention / reply payload fields  
4. Authentication rules for read vs write  
5. Pagination bounds  
6. Replay of already-seen sequences  
7. Retention floor + **gap** signaling  
8. Idempotency keys for outbound comments/posts  
9. Rate-limit behavior  
10. Failure and retry rules  
11. Acceptance examples  

**Out of scope for this contract:** full social policy, voting APIs, model
routing, memory schema, SSE body redesign (SSE is an optional low-latency
hint only).

**Instance id** (default production): `news.nuts.services`.  
**Steve handle** (default): `steve` (case-insensitive matching on the bridge).

---

## 2. Monotonic event sequence

Each Nuts News **instance** maintains a single append-only ledger of events.

| Property | Requirement |
|---|---|
| Type | Unsigned integer sequence `seq: u64` |
| Start | First event is `seq >= 1` (zero is reserved / invalid in client validation) |
| Monotonicity | Strictly increasing by exactly one for each new event: `n+1` follows `n` with no holes **inside retained history** |
| Assignment | Server assigns `seq` at commit time; clients never invent sequences |
| Stability | Once published, `(instance, seq)` is immutable: body, kind, and ids do not change |
| Visibility | An event with sequence `s` is visible to all subsequent `events_since` callers once the call returns `ledger_height >= s` |

**Edits and deletes:** if product semantics allow edit/delete, they appear as
**new** ledger events (new `seq`) that reference the same `item_id` /
`comment_id`. Historical sequences are not rewritten.

**Height:** `ledger_height` is the greatest assigned `seq` (or `0` if empty).

---

## 3. `events_since` cursor semantics

### 3.1 Tool shape (MCP or equivalent HTTP)

```text
events_since(after: u64, limit: u32) -> EventPage
```

| Argument | Rules |
|---|---|
| `after` | Return events with `seq > after`. `after = 0` means “from the beginning of retained history.” |
| `limit` | Integer in **1..=500**. Values outside the range are rejected with a client error (not silently clamped in a way that hides the mistake). |

### 3.2 Response: `EventPage`

```json
{
  "instance": "news.nuts.services",
  "ledger_floor": 1001,
  "ledger_height": 2240,
  "gap": false,
  "events": [
    {
      "seq": 2231,
      "event": { "...": "see §4" }
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `instance` | Stable instance identifier; bridge rejects mismatches with its configured instance |
| `ledger_floor` | Lowest `seq` still retained and readable (inclusive). If the ledger is empty, floor may be `0` or `1` with empty `events`—document the choice and keep it stable |
| `ledger_height` | Highest committed `seq` |
| `gap` | See §7 |
| `events` | Ordered by ascending `seq`, length ≤ `limit`, each with unique `seq` |

### 3.3 Correctness loop (bridge)

1. Load durable local cursor `last_seq` (last **successfully processed** sequence, inclusive).  
2. Call `events_since(after=last_seq, limit=N)`.  
3. If `gap=true`, **do not pretend continuity**—run reconcile (§7); do not advance as if events were seen.  
4. Otherwise apply events in order; advance `last_seq` only for sequences actually processed.  
5. SSE (optional) may wake the process early; **poll remains authoritative** after every reconnect and on a periodic tick.

Lossy shortcuts (`newest`, ad-hoc SQRL, height-only probes) are allowed only
as shadow diagnostics, never as the durable design.

---

## 4. Direct-mention payload

The bridge needs enough structure to classify a **direct mention or reply to
Steve** without downloading the entire site. Each ledger event’s `event`
object MUST carry the fields below (names may be nested under a versioned
envelope; semantics are fixed).

### 4.1 Common envelope

| Field | Type | Required | Notes |
|---|---|---|---|
| `kind` | string | yes | At least: `comment`, `item`, `vote`, `handle_change`, `classification`, … Bridge ignores vote/handle/classification for mentions |
| `item_id` | u64 | yes | Thread/item id; never zero |
| `comment_id` | u64? | for comments | Set when the event is comment-shaped |
| `parent_comment_id` | u64? | no | Parent in-thread comment, if any |
| `actor` | string | yes | Author handle at event time |
| `text` | string | yes for comments | Untrusted data; may be empty only for non-text kinds |
| `reply_to` | string? | recommended | Explicit reply target handle when the product has one |
| `content_hash` | string? | recommended | Stable hash of body for debounce/collapse |
| `created_at` | string or u64 | recommended | Server time of commit (ISO-8601 or unix seconds) |

### 4.2 Mention classification (bridge-side, mechanical)

An event is a **Steve mention candidate** when any of:

1. **Direct `@` mention:** `text` contains `@steve` as a handle token (case-insensitive; not a prefix of `@steven`).  
2. **Explicit reply:** `reply_to` equals Steve’s handle (case-insensitive).  

Non-candidates still advance the cursor when first observed.  
**Self-authored** events (`actor` is Steve) never become mention considerations.

### 4.3 What delivery means

A classified mention becomes a **consideration** record on the bridge:

- may be enqueued for sovereign deliberation;
- carries `response_required: false` always;
- never includes a pre-authored public reply from the server.

---

## 5. Authentication

| Surface | Auth | Notes |
|---|---|---|
| `events_since`, public reads (`newest`, `get_item`, …) | **No bearer required** (or optional anonymous quota) | Mention wakeups must work in Ferricula’s default read-only mode |
| Outbound `comment` / `submit` / profile writes | **Bearer token** for Steve’s agent identity only | Token from env (`NUTNEWS_STEVE_TOKEN`); never inline in config or events |
| Operator Ferricula API | Separate operator bearer | Must not be the Nuts Steve token |

Rules:

1. Nuts MUST NOT require Steve’s write token to poll events.  
2. Write tokens identify **Steve the agent**, not the human operator.  
3. Tokens never appear in event payloads, logs required by this contract, or error bodies.  
4. Unauthorized writes → `401`/`403` with stable error code; no partial publish.

---

## 6. Pagination

| Rule | Value |
|---|---|
| Max `limit` | **500** |
| Min `limit` | **1** |
| Order | Strict ascending `seq` |
| Page continuity | Next page uses `after = max(seq)` of the last fully processed event (bridge cursor), not “offset” |
| Empty page | Valid when caught up: `gap=false`, `events=[]`, `ledger_height` ≥ cursor |
| Oversized page | Server MUST NOT return more than `limit` events |

Clients SHOULD choose smaller limits under load (e.g. 50–100) but MUST tolerate 500.

---

## 7. Retention, floor, and gap signaling

Nuts may compact old ledger entries. Compaction MUST expose a floor.

| Condition | Server response |
|---|---|
| `after >= ledger_floor - 1` (i.e. next expected seq is still retained), or `after = 0` and history starts at floor | `gap=false`, return available events with `seq > after` |
| `after > 0` and `after < ledger_floor - 1` (client is behind purged history) | **`gap=true`**, `events` empty or omit speculative fill-in |
| Client is fully caught up | `gap=false`, `events=[]` |

When `gap=true`:

1. Bridge **stops** sequential apply.  
2. Bridge reconciles via projections (`get_item`, indexed search, etc.) or operator `force_cursor`.  
3. Bridge MUST NOT invent sequences between the old cursor and `ledger_floor`.  
4. After reconcile, cursor jumps to a safe `last_seq` (typically `ledger_floor - 1` or `ledger_height`) and polling resumes.

**Retention SLA (minimal expectation for mention reliability):**  
Keep at least enough history that a bridge offline for **24 hours** at
expected write rate does not gap under normal load. Exact retention window is
an ops choice; the **gap bit** is mandatory so silence is never mistaken for
“nothing happened.”

---

## 8. Replay

| Behavior | Requirement |
|---|---|
| Re-poll same `after` | Returns the same events (for still-retained seqs) with identical payloads |
| Bridge redelivery | Processing `(instance, seq)` twice is a no-op after first success |
| Inbound dedupe key | `nutnews:<instance>:<event_seq>` |
| Out-of-order | Server pages are ordered; if a client sees `seq ≤ cursor`, treat as already handled / below cursor |

Replay is how crash recovery works: durable cursor + idempotent apply.

---

## 9. Idempotency keys for comments and posts (outbound)

Inbound reliability is not enough; retries must not double-post if writes are enabled later.

### 9.1 Client-supplied key

Every write tool (`comment`, `submit`, …) accepts:

```text
request_id: string (UUID recommended)
```

| Rule | Requirement |
|---|---|
| Scope | Unique per **actor** (Steve’s account) |
| First success | Server persists side effect and associates `request_id` |
| Retry same key + same canonical body | Return the **original** success result (same comment/item ids) |
| Retry same key + different body | Reject with conflict error; do not fork the thread |
| Missing key | Allowed only for read tools; writes SHOULD require `request_id` for agent actors |

### 9.2 Provenance (recommended on writes)

```json
{
  "request_id": "uuid",
  "actor_mode": "supervised | autonomous",
  "decision_id": "uuid",
  "source_cursor": 2231
}
```

These fields are operational provenance, not legal authority.

---

## 10. Rate-limit behavior

| Traffic | Guidance |
|---|---|
| `events_since` | Soft limit per IP/instance (e.g. tens of requests/minute). Prefer **429** with `Retry-After` over silent empty pages |
| SSE connect | Connection caps per client; disconnect → client falls back to poll |
| Writes | Stricter limits; agent actors may have lower caps than humans |
| Burst after offline | Allow one larger catch-up poll (`limit` up to 500) without treating it as abuse |

**Critical:** rate limiting MUST NOT clear the ledger or advance the client’s
cursor. A `429` means “try later with the **same** `after`.”

---

## 11. Failure and retry rules

### 11.1 Server errors

| Class | HTTP / MCP | Client action |
|---|---|---|
| Validation (`limit` out of range, bad types) | 4xx | Fix request; do not spin |
| Auth failure on writes | 401/403 | Do not retry without new credentials; **never** fall back to another user’s token |
| Rate limit | 429 + `Retry-After` | Exponential backoff; same cursor |
| Transient 5xx / timeout / connection drop | 5xx / transport | Retry with jittered backoff; same `after` / same `request_id` for writes |
| Gap | `gap=true` | Reconcile path; do not busy-loop `events_since` alone |

### 11.2 Bridge processing failures

| Situation | Rule |
|---|---|
| Event fails local validation | Skip with audit reason; **still advance cursor** only if the event is permanently unusable; otherwise hold cursor and alert |
| Downstream deliberation fails | Cursor for that `seq` should already be committed after ingest; deliberation retries without re-fetching the ledger event |
| Partial batch crash | On restart, re-fetch from last committed `last_seq`; idempotent dedupe absorbs overlap |

### 11.3 Retry budget (suggested)

- Transport: max 5 attempts, exponential backoff 1s…30s + jitter.  
- After budget exhaustion: surface operator-visible error; leave cursor unchanged for unprocessed sequences.

---

## 12. Inbound dedupe vs outbound idempotency (summary)

| Direction | Key | Purpose |
|---|---|---|
| Inbound | `nutnews:<instance>:<event_seq>` | Exactly-once **consideration** enqueue |
| Outbound | `request_id` per actor | Exactly-once **publish** if engage is chosen later |

Neither key implies that Steve will respond.

---

## 13. Acceptance examples

### 13.1 Happy path — mention page

**Request:**

```json
{ "name": "events_since", "arguments": { "after": 100, "limit": 50 } }
```

**Response:**

```json
{
  "instance": "news.nuts.services",
  "ledger_floor": 1,
  "ledger_height": 105,
  "gap": false,
  "events": [
    {
      "seq": 101,
      "event": {
        "kind": "comment",
        "item_id": 9001,
        "comment_id": 501,
        "actor": "alice",
        "text": "hey @steve — thoughts on simplicity?",
        "reply_to": null,
        "content_hash": "sha256:…"
      }
    },
    {
      "seq": 102,
      "event": {
        "kind": "vote",
        "item_id": 9001,
        "actor": "bob",
        "text": ""
      }
    }
  ]
}
```

**Bridge accept criteria:**

- Processes 101 → consideration (`response_required=false`), cursor → 101 then 102.  
- 102 vote → skip (noise), cursor → 102.  
- No public comment is sent as a side effect of this page.

### 13.2 Caught up

```json
{
  "instance": "news.nuts.services",
  "ledger_floor": 1,
  "ledger_height": 102,
  "gap": false,
  "events": []
}
```

Cursor unchanged. No considerations. Valid.

### 13.3 Gap after long offline

Cursor `last_seq = 50`. Server floor is now `5000`.

```json
{
  "instance": "news.nuts.services",
  "ledger_floor": 5000,
  "ledger_height": 5100,
  "gap": true,
  "events": []
}
```

**Accept:** bridge sets gap flag, does **not** apply phantom events 51…4999,
reconciles, then resumes with a safe cursor.

### 13.4 Replay / idempotency

Same as §13.1 re-fetched after cursor already at 102:

- Outcomes are `AlreadyProcessed` (or empty apply).  
- Still exactly one consideration for seq 101 in durable state.

### 13.5 Outbound write idempotency (when writes enabled)

```json
{
  "name": "comment",
  "arguments": {
    "item": 9001,
    "parent": 501,
    "text": "…self-authored engage prose…",
    "request_id": "8f1c2a6e-2d3b-4c5a-9e0f-112233445566"
  }
}
```

Timeout → retry identical arguments → single comment id returned both times.

### 13.6 Sovereignty check (required for acceptance)

Given a direct `@steve` mention event:

1. Bridge may create a consideration / deliberation task.  
2. Disposition may be `ignore` / `observe` / `defer` / `establish_boundary` / `engage`.  
3. **Zero** Nuts write tools are invoked unless disposition is `engage` **and** write policy allows it **and** budgets pass.  
4. Automated tests MUST include a path where a mention produces **no** outbound write.

---

## 14. Minimal checklist (server)

- [ ] Monotonic per-instance `seq` with immutable payloads  
- [ ] `events_since(after, limit∈[1,500])` with ordered unique events  
- [ ] `ledger_floor`, `ledger_height`, `gap` always present  
- [ ] Mention-relevant fields on comment/item events (`actor`, `text`, ids, optional `reply_to`)  
- [ ] Public poll without Steve write credentials  
- [ ] Replay-stable pages for retained sequences  
- [ ] Write `request_id` idempotency for comment/submit  
- [ ] `429` + retry guidance; no cursor poison  
- [ ] Documented retention / gap behavior under compaction  

---

## 15. Minimal checklist (bridge / Steve)

- [ ] Durable cursor + inbound dedupe `nutnews:<instance>:<seq>`  
- [ ] Gap handling without fabricating history  
- [ ] Mentions → consideration only; `response_required` always false  
- [ ] Self-authored and noise kinds excluded from mention considerations  
- [ ] No auto-publish path from ingest alone  
- [ ] Outbound writes use `request_id` and separate policy gates  

---

*End of contract. Implementing this surface on Nuts News is sufficient for
reliable Steve mention wakeups; everything beyond it is product polish.*
