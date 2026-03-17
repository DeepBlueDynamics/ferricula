# ============================================================
# ferricula — thermodynamic memory engine
# Build: docker build -t ferricula .
# Run:   docker run -p 8765:8765 -v ferricula-data:/data ferricula
# Multi: docker run -p 8764:8764 -e PORT=8764 -v my-data:/data ferricula
# ============================================================

FROM rust:1.88-bookworm AS builder

WORKDIR /build
COPY Cargo.toml ./
COPY src ./src

# Build release binary
RUN cargo build --release --locked 2>/dev/null || cargo build --release

# ============================================================
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/ferricula /usr/local/bin/ferricula

# Data persists across restarts via volume
VOLUME ["/data"]

# Port is configurable via PORT env var (default 8765)
ENV PORT=8765

# Default: run in serve mode with /data as storage
CMD ferricula /data --serve $PORT
