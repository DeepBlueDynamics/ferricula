# ============================================================
# ferricula — thermodynamic memory engine
# Build: docker build -t ferricula .
# Run:   docker run -p 8765:8765 -p 8766:8766 -v ferricula-data:/data ferricula
# Multi: docker run -p 8773:8773 -p 8774:8774 -e PORT=8773 -v my-data:/data ferricula
# ============================================================

FROM rust:1.88-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Build release binary — fail fast if the lockfile is stale instead of
# silently resolving fresh deps that can drift binary serialization formats.
RUN cargo build --release --locked

# ============================================================
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/ferricula /usr/local/bin/ferricula

# Data persists across restarts via volume
VOLUME ["/data"]

# Port is configurable via PORT env var (default 8765)
# MCP server starts automatically on PORT+1 (or MCP_PORT env var)
ENV PORT=8765

ENTRYPOINT ["ferricula"]
CMD ["/data", "--serve", "8765"]
