# Steve Jobs recovery and v2 migration

Status recorded 2026-07-13.

## Live recovery instance

The original `steve-jobs` container and `steve-jobs-data` volume were left
untouched. A verified working copy is running as:

- container: `steve-jobs-recovery`
- volume: `steve-jobs-data-recovery-20260713`
- HTTP: `http://127.0.0.1:8873`
- MCP: `http://127.0.0.1:8874/mcp`
- image: `ferricula:0.10.3`

The source and copied volumes contained the same four files and had matching
SHA-256 hashes before the recovery instance was started: `agent.toml`,
`identity.json`, `snapshot_v4.bin`, and `wal.log`.

Verified live state after startup:

- identity: `ferricula-stevejobs` / Steve Jobs
- rows and memories: 3,362
- lifecycle: 1,409 active, 28 forgiven, 1,925 archived
- keystones: 556
- graph: 470 nodes, 588 edges
- prime tree: 5,749 terms, 10,967 nodes, 3,314 members

HTTP status, HTTP identity, MCP initialization, MCP tool listing, and a real
semantic recall were all tested successfully from the v2 development
container. The recall returned relevant product-design memories.

## V2-compatible runtime copy

A point-in-time copy of the recovery volume is at
`.runtime/steve-jobs/`. The entire `.runtime/` directory is gitignored because
it contains agent identity and memory data.

V2 can open the legacy v4 snapshot and replay its WAL directly:

```powershell
cargo run -q -p ferricula-server -- inspect .runtime\steve-jobs --json
```

This command is read-only: it loads and summarizes state but does not
checkpoint or mutate the data directory.

## What is and is not online

Ferricula memory and MCP are online. The separate Delos conversational broker
is not running.

The Steve-specific Delos broker in the original arena is the code that wraps
memory with a persona and LLM loop. Its actual routing is:

- ordinary turns: local Ollama, default `gemma4:e2b`
- critical Ferricula load: optional Anthropic Haiku escalation
- Ferricula `/tools`: suppresses tools by load tier

The newer `ferricula-arena` Steve configuration instead pins
`claude-sonnet-4-6`. Its autonomous loop uses radio entropy to decide when to
think/search, but does not implement the same provider routing.

Do not start the legacy Delos broker as a harmless sleeping process. Its idle
loop wakes every 90 seconds by default, recalls a memory, crawls externally,
ingests results, and periodically triggers dreams. That behavior needs an
explicit safe-idle mode before it becomes Steve's default runtime.

The five archetypes remain in legacy documentation and serialized identity
metadata. They are not five autonomous agents. V2 preserves Intuition,
Fortune, Craft, Ethics, and Advocate as bounded perspectives that whisper into
one `InternalAgent` judgment seam. They share Steve's context but own no
memory, credentials, tools, scheduler, or public identity.

## Immediate operator checks

```powershell
docker ps --filter name=steve-jobs-recovery
curl.exe http://127.0.0.1:8873/status
curl.exe http://127.0.0.1:8873/identity
docker logs --tail 100 steve-jobs-recovery
```

The recovery container uses `--restart unless-stopped`. Stop only the copy
with `docker stop steve-jobs-recovery`; do not start or alter the original
`steve-jobs` container during migration work.
