# Ferricula Operations

## Registry

Docker Hub: `deepbluedynamics/ferricula`

## Release Checklist

Before building and pushing, always:

1. **Bump the version** in `Cargo.toml` (semver: patch for fixes, minor for new features/breaking changes)
2. **Update site version** in `ferricula-ops/site/index.html` and `docs.html` — search for `v0.X.Y`
3. **Commit everything** to `DeepBlueDynamics/ferricula` and push
4. **Build + push Docker** (see below)
5. **Deploy site** from `ferricula-ops/site/` (see Site Deployment)

## Build & Push

```bash
# Build + push tagged with current git SHA
./scripts/docker-push.sh

# Build + push SHA and also tag latest
./scripts/docker-push.sh latest
```

Requires `docker login` before pushing.

## GitHub Push

```bash
cd /path/to/ferricula
git add Cargo.toml Cargo.lock OPERATIONS.md scripts/ src/
git commit -m "release: vX.Y.Z — <summary>"
git push origin main
```

## Site Deployment

Site lives in `ferricula-ops/site/`. Deploys to Google Cloud Run at ferricula.com.

```bash
cd /path/to/ferricula-ops/site
gcloud auth login   # one-time if not already authenticated
./deploy.sh
```

Project: `gnosis-459403`, Region: `us-central1`, Service: `ferricula-site`

## Run (self-hosted)

```bash
# Default port 8765 (REST) + 8766 (MCP)
docker run -p 8765:8765 -p 8766:8766 -v ferricula-data:/data deepbluedynamics/ferricula

# Custom port — use PORT env var (not trailing args)
docker run -p 8773:8773 -p 8874:8774 -e PORT=8773 -v my-data:/data deepbluedynamics/ferricula

# Connect Claude Code to the MCP endpoint:
#   claude mcp add ferricula --sse http://localhost:8766/mcp
```

## Environment Variables

| Variable | Default | Purpose |
|---|---|---|
| `PORT` | `8765` | HTTP listen port |
| `AGENT_KEY` | — | Anthropic API key (query planner) |
| `SHIVVR_URL` | — | Embedding service (shivvr) |
| `SHIVVR_AUTH_TOKEN` | — | Bearer token for shivvr HTTPS |
| `RADIO_URL` | — | Entropy source (gnosis-radio) |
| `CLOCK_TICK_SECS` | `60` | Dream cycle interval |
| `EDGE_ANCHOR_COUNT` | `10` | Top-fidelity memories scanned every dream |
| `EDGE_EXPLORER_COUNT` | `30` | Additional memories sampled via radio entropy each dream |
| `EDGE_MAX_PER_DREAM` | `12` | Cap on new semantic edges per dream cycle |

## Data

All state lives in `/data` inside the container. Mount a named volume to persist across restarts:

```bash
docker volume create ferricula-data
docker run -p 8765:8765 -v ferricula-data:/data deepbluedynamics/ferricula
```

## Build from Source

```bash
cargo build --release
./target/release/ferricula /data --serve 8765
```

Requires Rust 1.88+. See README.md for full checkout guide.

## Health Check

```
GET http://localhost:8765/health
```

Returns `200 OK` when the engine is up.
