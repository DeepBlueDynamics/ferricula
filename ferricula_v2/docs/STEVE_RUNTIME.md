# Steve runtime — deployment and operations

Runbook for the containerized Steve runtime (`ferricula-server serve`).
Written 2026-07-13 against `ferricula-server 2.0.0-alpha.0`. Companion to
[STEVE_RECOVERY.md](STEVE_RECOVERY.md), which records the recovery instance
and volume provenance.

Deployment files:

| File | Purpose |
|---|---|
| `Dockerfile` | Multi-stage build; thin runtime image, non-root, no `ml`/ONNX stack |
| `compose.yaml` | Production service definition, hardening, volumes, secrets |
| `config/steve.example.toml` | Template for `config/steve.toml` (mounted read-only) |
| `.dockerignore` | Allowlist build context — `.runtime/` and secrets can never enter an image |
| `docs/STEVE_RUNTIME.md` | This runbook |

No Rust source, Cargo manifests, recovery data, or original-Ferricula files
were changed for this deployment work.

---

## 1. What runs

One process: `ferricula-server serve --config /app/config/steve.toml`
(`crates/ferricula-server/src/main.rs`). On startup it:

1. loads and validates the TOML config (`config.rs`);
2. opens `memory_dir` read-only — parses `identity.json`, loads
   `snapshot_v4.bin`, replays `wal.log` in memory, and keeps a summary
   (`Inspection`); it does **not** checkpoint or write to the memory
   directory;
3. restores durable runtime state (mode, task queue) from
   `state_dir/runtime-state.json`, restores model usage from
   `model-usage.json`, or initializes fresh state from `initial_mode`;
4. starts a task worker, a wake scheduler, and the axum HTTP API on the
   configured bind address (default port **8875**).

The image builds only `ferricula-server` and its dependency subtree
(`ferricula-core`, `ferricula-cognition`). The `ml` feature stack
(ONNX/ort, ~1.1 GB of models) is not in that subtree and is not present in
the image.

## 2. First deployment

```bash
cd ferricula_v2

# 1. Compose mounts the safe example directly. For a custom provider table,
#    copy it and change the compose bind mount to config/steve.toml.

# 2. Operator token as a Docker secret file (never commit; keep out of git)
mkdir -p secrets
openssl rand -hex 32 > secrets/ferricula_operator_token
chmod 600 secrets/ferricula_operator_token

# 3. Verify the recovery volume exists (compose treats it as external)
docker volume inspect steve-jobs-data-recovery-20260713 >/dev/null

# 4. Validate, build, start
docker compose config -q
docker compose build
docker compose up -d
docker compose ps        # wait for "healthy"
```

`secrets/` and `config/steve.toml` are gitignored.

To talk to the API as the operator:

```bash
TOKEN=$(cat secrets/ferricula_operator_token)
curl -s http://127.0.0.1:8875/health                                   # no auth
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8875/status
```

## 3. Volumes and data-preservation policy

| Volume | Mount | Mode | Policy |
|---|---|---|---|
| `steve-jobs-data` (original) | — | — | **Never referenced** by this stack. Untouched ultimate fallback. |
| `steve-jobs-data-recovery-20260713` | `/data/steve-memory` | **read-only** | Migration source. `external: true`, so compose can never create/alter/remove it; the `:ro` mount makes writes impossible even if code changes. |
| `steve-v2-runtime` | `/data/steve-runtime` | read-write | The only writable persistence: `runtime-state.json` (mode, task queue, wake bookkeeping). Compose-managed. |

Keep the memory mount read-only for the whole migration phase. When v2
gains write paths into the memory plane (checkpointing, `remember`), do
**not** flip this mount to read-write. Promote instead:

```bash
docker compose down
docker volume create steve-v2-memory
docker run --rm \
  -v steve-jobs-data-recovery-20260713:/src:ro \
  -v steve-v2-memory:/dst \
  debian:bookworm-slim sh -c 'cp -a /src/. /dst/'
# then point the steve-memory volume entry in compose.yaml at
# steve-v2-memory and drop the :ro flag deliberately, in one reviewed change
```

That keeps three generations at all times: original, verified recovery
copy, and the live v2 working set.

## 4. Activity modes: paused vs asleep vs wake

Modes (`runtime.rs::ActivityMode`): `paused`, `asleep`, `peripheral`,
`engaged`, `deliberative`. Operator aliases accepted by the API: `off` →
paused, `sleep` → asleep, `wake`/`awake` → engaged.

- **Paused** — hard stop. The worker claims no tasks (queued work is held,
  not lost) and the scheduler enqueues no wakes. Steve does nothing until
  an operator changes mode. Use for migrations, backups, incidents.
- **Asleep** — normal resting state and the shipped default
  (`initial_mode = "asleep"`). Steve is not continuously engaged, but the
  wake scheduler still fires and the worker still executes queued tasks.
  This is exactly how the 3–6 daily wakes happen: Steve sleeps, a
  `scheduled_wake` task fires, he observes, and returns to rest.
- **Engaged (wake)** — operator-invoked active state via
  `POST /control/wake`. Semantically "Steve is up and interacting."
- **Peripheral / Deliberative** — reserved intermediate ("half-listening")
  and heightened ("thinking hard") states. ⚠ **Rust dependency:** the
  current worker/scheduler only special-cases `paused`; the other four
  modes behave identically today. Treat peripheral/deliberative as
  API-stable placeholders until behavior lands.

Mode is durable: every change is persisted atomically
(tmp-file + rename) to `runtime-state.json`, and a restart resumes the
persisted mode. `initial_mode` applies only on first boot with a fresh
state volume.

```bash
AUTH="Authorization: Bearer $TOKEN"
curl -s -X POST -H "$AUTH" http://127.0.0.1:8875/control/pause     # hard stop
curl -s -X POST -H "$AUTH" http://127.0.0.1:8875/control/sleep     # resume resting
curl -s -X POST -H "$AUTH" http://127.0.0.1:8875/control/wake      # engage
curl -s -X POST -H "$AUTH" -H 'content-type: application/json' \
  -d '{"mode":"deliberative"}' http://127.0.0.1:8875/control/mode  # any mode
```

## 5. Scheduled wakes (3–6 per day)

Configured in `[schedule]`. The scheduler ticks every 60 s and enqueues a
`scheduled_wake` task when all of these hold:

- `schedule.enabled` is true (toggle live via
  `POST /control/schedule {"enabled": bool}` — persisted),
- mode is not `paused`,
- `now − last_scheduled_wake ≥ 86400 / wakes_per_day` seconds.

`wakes_per_day = 4` (every ~6 h) is the shipped default; 3–6 is the
intended operating band, and validation rejects 0 and >24. Wakes are
spaced relative to the previous wake, not anchored to clock times.
`jitter_minutes` adds a stable per-interval offset derived from Steve's ID
and previous wake, so restarts cannot reroll or bunch the schedule. After a
long pause the next wake fires within a minute of resuming.

What a wake does today: the deployed read-only Nuts News connector reads the
newest `budgets.max_threads_per_wake` threads (public read, no credential) and
records the observation on the task. If an operator disables the connector,
the wake completes as an explicit no-op. No model calls and no writes occur.

There is not yet time-of-day anchoring (for example, "wake around 09:00").

## 6. Operator authentication

- Every endpoint except `GET /health` requires
  `Authorization: Bearer <token>`; failures get `401`.
- The expected token is read from the environment variable **named** by
  `operator_token_env` (default `FERRICULA_OPERATOR_TOKEN`) and compared
  in constant time.
- Config validation refuses token-looking values in `*_env` fields and
  refuses `require_operator_auth = false` whenever the bind address is
  non-loopback — which it always is inside the container. Auth cannot be
  accidentally disabled in this deployment.
- The token value lives only in `secrets/ferricula_operator_token` on the
  host and `/run/secrets/ferricula_operator_token` in the container. The
  entrypoint shim resolves `FERRICULA_OPERATOR_TOKEN_FILE` into the
  process environment; the value appears in no image layer, compose file,
  or config file.

Rotation: write a new value to the secret file, then
`docker compose up -d --force-recreate steve`. There is one operator
token; per-operator identities and audit trails are future work.

## 7. Network exposure: port 8875, localhost only

Inside the container the server binds `0.0.0.0:8875` (required for the
port mapping to reach it). Compose publishes it as **`127.0.0.1:8875`** on
the host only — nothing is reachable from other machines. Verify with
`ss -tlnp | grep 8875`; the listener must show `127.0.0.1:8875`.

For remote operator access, use an SSH tunnel
(`ssh -L 8875:127.0.0.1:8875 host`) or put an authenticated TLS reverse
proxy in front. Do not widen the publish address; the app speaks plain
HTTP and has a single shared operator token.

Outbound traffic when fully enabled: `news.nuts.services` (MCP over
HTTPS) and, once a model route exists, the model provider API. With the
defaults, the runtime makes no outbound calls at all.

## 8. Health checks

`GET /health` is unauthenticated by design and returns
`{"ok": true, "agent_id": …, "mode": …}`. Both the Dockerfile
`HEALTHCHECK` and the compose healthcheck probe it every 30 s from inside
the container (via the image's `curl`); `docker compose ps` shows
`healthy`/`unhealthy`, and `restart: unless-stopped` restarts a crashed
process. Note the probe proves the API and runtime state are up — it does
not exercise memory recall. For a deeper check, hit `/status` with the
operator token and compare `memory_records` against the expected count
(3,362 as of the recovery snapshot).

## 9. Model provider configuration

References are by environment-variable **name** only; values arrive as
Docker secrets through the entrypoint shim (`*_FILE` pattern):

| Variable | Secret file | Used for |
|---|---|---|
| `FERRICULA_OPERATOR_TOKEN` | `secrets/ferricula_operator_token` | Operator API auth (required) |
| `NUTNEWS_STEVE_TOKEN` | `secrets/nutnews_steve_token` | Nuts News **writes** only (optional) |
| `ANTHROPIC_API_KEY` | `secrets/anthropic_api_key` | Built-in Haiku/Sonnet routes when enabled |
| `OPENAI_API_KEY` | `secrets/openai_api_key` | Optional OpenAI-compatible profile |

The safe container config points `local_ollama` at
`http://host.docker.internal:11434/v1`. `ollama_model` selects the model. A
host Ollama signed into Cloud can therefore expose `glm-5.2:cloud` through the
same OpenAI-compatible route. Because that name ends in `:cloud`, the loader
removes `private_context` automatically; list `local_ollama` in
`private_context_profiles` only if you deliberately authorize that cloud
service to receive recovered memory excerpts.

The runtime includes a real provider router and HTTP transport. Mechanical
work is always no-model. Other task classes use ordered profile chains with
context/capability filtering, provider fallback, per-route ceilings, and the
hard `budgets.max_model_usd_per_day` cap. Usage is persisted in
`model-usage.json`, so restart does not reset the daily budget.

Recovered memories are private context. The built-in local Ollama profile has
the `private_context` capability; cloud profiles deliberately do not. A cloud
profile cannot receive Steve's memory-backed prompt unless the operator grants
that capability explicitly in a complete `[models]` configuration.

Direct mentions are deliberated into `engage`, `observe`, `ignore`, `defer`,
or `establish_boundary`. A mention never requires a response. Only a valid
self-authored `engage` decision may include proposed public prose, and proposal
is still separate from Nuts News publication.

## 10. Nuts News policy: writes off by default

The deployed default is read-only: `nutnews.enabled = true`,
`allow_writes = false`.

| Level | Config | Credential | Effect |
|---|---|---|---|
| 0 | `enabled=false` | none | Wakes are no-ops; no outbound traffic |
| 1 read-only (default) | `enabled=true`, `allow_writes=false` | none needed | Scheduled wakes read newest threads (public MCP reads); write attempts fail with "writes are disabled by policy" |
| 2 writes | `enabled=true`, `allow_writes=true` | `NUTNEWS_STEVE_TOKEN` secret mounted | Comments/submissions possible, bearer-authenticated as Steve |

The write gate is enforced in `nutnews.rs` before any request is built,
and the token is only read from the environment at write time — so level
1 genuinely cannot write even if a token is present. Keep level 2 off
until the per-day comment/submission budgets are enforced in code (§12)
and the identity/disclosure conventions in
`research/nuts-news/ARCHITECTURE.md` are in place. Never reuse the
operator's personal Nuts News credential as Steve's token.

## 11. Backup, restore, rollback

**What to back up:** the runtime state volume (cheap, one JSON file
today) and — once promoted to read-write — the v2 memory volume. The
recovery volume is itself a verified backup of the original; hash-check
it rather than re-copying it.

```bash
# Backup runtime state (pause first for a quiescent queue — optional)
mkdir -p backups
docker run --rm -v steve-v2-runtime:/src:ro -v "$PWD/backups":/dst \
  debian:bookworm-slim tar czf /dst/steve-runtime-$(date +%Y%m%d-%H%M%S).tgz -C /src .

# Integrity check of the read-only memory volume (compare across runs)
docker run --rm -v steve-jobs-data-recovery-20260713:/m:ro \
  debian:bookworm-slim sh -c 'cd /m && sha256sum agent.toml identity.json snapshot_v4.bin wal.log'
```

**Restore runtime state:**

```bash
docker compose down
docker run --rm -v steve-v2-runtime:/dst -v "$PWD/backups":/src \
  debian:bookworm-slim sh -c 'rm -rf /dst/* && tar xzf /src/<backup>.tgz -C /dst'
docker compose up -d
```

**Rollback, in escalating order:**

1. *Bad code/image:* `docker compose down`, pin the previous image tag in
   `compose.yaml` (tags follow the workspace version), `up -d`. Memory and
   state volumes are unaffected.
2. *Bad runtime state:* restore the state-volume backup as above — or,
   since today it holds only mode + task queue, delete
   `runtime-state.json` for a factory-fresh queue in `initial_mode`.
3. *Corrupted v2 memory copy (post-promotion):* recreate `steve-v2-memory`
   from the untouched recovery volume (§3 promotion procedure).
4. *Catastrophic:* the original `steve-jobs-data` volume has never been
   mounted by anything in this stack and remains the final fallback.

Take a state backup (and, post-promotion, a memory backup) before every
image upgrade.

## 12. Dependencies on Rust endpoints — read before trusting a knob

Marked ⚠ throughout; collected here. These are places where the
configuration or documentation intentionally runs ahead of the code:

| Area | Status in code today |
|---|---|
| `schedule.jitter_minutes` | **Applied** as stable bounded per-interval jitter |
| Time-of-day wake anchoring | Not implemented; spacing is relative to the last wake |
| `budgets.max_model_usd_per_day` | **Enforced** across fallback and persisted across restart |
| `max_autonomous_comments_per_day`, `max_autonomous_submissions_per_day` | Declared; writes remain disabled and these must be enforced before publication ships |
| `budgets.max_threads_per_wake` | **Enforced** (caps the Nuts News `newest` read) |
| Model provider keys | Active through validated env-var references; local/no-model operation needs none |
| `peripheral` / `deliberative` modes | Accepted and persisted, behavior identical to `asleep` (only `paused` gates the worker/scheduler) |
| `POST /tasks/mention` | Performs private-memory recall and sovereign deliberation; never publishes automatically |
| `research`/`reflect` tasks | Retrieve memory and council context; full inference remains incomplete |
| `POST /memory/recall` | **Implemented** as authenticated provider-free lexical candidate recall; hybrid embeddings and reinforcement remain future work |
| `POST /tasks/read-hacker-news` | **Implemented** against the fixed HN API; arbitrary URL crawling remains unavailable |
| Graceful-shutdown checkpoint | None needed yet (memory is read-only; state writes are atomic tmp+rename, so SIGKILL-safe) |
| Wake execution content | A wake currently = one bounded feed read (or a no-op); reflection/dreaming on wake is future cognition wiring |

When any of these land in Rust, revisit the config template and this
document in the same change.

## 13. Acceptance checks

Run after every deploy or upgrade. All commands from the repo root on the
Docker host; `TOKEN=$(cat secrets/ferricula_operator_token)`,
`AUTH="Authorization: Bearer $TOKEN"`.

1. **Compose syntax** — `docker compose config -q` exits 0.
2. **Image builds** — `docker compose build` succeeds (proves `.runtime/`
   is outside the build context: the context should be a few MB).
3. **Memory opens read-only** —
   `docker compose run --rm steve inspect /data/steve-memory --json`
   reports `ferricula-stevejobs` / Steve Jobs, 3,362 memories, 556
   keystones (values per STEVE_RECOVERY.md).
4. **Service healthy** — `docker compose up -d`, then `docker compose ps`
   shows `healthy`; `curl -fsS http://127.0.0.1:8875/health` returns
   `"ok": true` with `"mode": "asleep"` on first boot.
5. **Auth enforced** — `curl -s -o /dev/null -w '%{http_code}'
   http://127.0.0.1:8875/status` is `401`; with `-H "$AUTH"` it is `200`
   and reports `nutnews_enabled: true`, `nutnews_writes_enabled: false`,
   `memory_records: 3362`.
6. **Mode transitions** — pause, verify `/health` shows `paused`, wake,
   verify `engaged`, sleep, verify `asleep`.
7. **Wake path is safe** — `curl -s -X POST -H "$AUTH"
   http://127.0.0.1:8875/tasks/read-feed`, then `GET /tasks`: the task
   completes with `"observed": true`, a bounded feed result, and
   `"model_calls": 0`; no write credential is mounted.
8. **State survives restart** — set mode `paused`,
   `docker compose restart steve`, confirm mode is still `paused`; then
   return it to `asleep`.
9. **Loopback only** — `ss -tlnp | grep 8875` shows `127.0.0.1:8875`
   (not `0.0.0.0` or `[::]`); from another machine the port is closed.
10. **Memory volume untouched** — the sha256 check from §11 matches the
    hashes recorded before startup; and `docker inspect steve-jobs-v2`
    shows the `/data/steve-memory` mount with `"RW": false`.
11. **Original volume never referenced** — `docker inspect steve-jobs-v2 |
    grep -c 'steve-jobs-data"'` is 0; only the recovery-named volume and
    `steve-v2-runtime` appear.
12. **Scheduler fires** — within ~1 minute of first boot, `GET /tasks`
    shows one completed `scheduled_wake` (spacing gate passes immediately
    on a fresh state volume), and `/status` reports a
    `last_scheduled_wake` timestamp.
13. **Memory recall** — authenticated `POST /memory/recall` with
    `{"query":"product design simplicity","limit":3}` returns Steve memory
    IDs and relevant keystone text without making a model call.
14. **Sovereignty** — enqueue a test mention containing a demand for an
    answer. With no eligible private-context model, it must return
    `self_authored:false`, `disposition:"defer"`, and no public response. With
    a configured model, engage/observe/ignore/defer/establish_boundary are all
    accepted outcomes; only engage may contain proposed prose.
