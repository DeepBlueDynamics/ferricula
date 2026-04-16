#!/bin/bash
# Start ferricula locally for testing.
# Mirrors exactly what the Cloud Run container does.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DATA_DIR="${FERRICULA_DATA:-$SCRIPT_DIR/data}"
PORT="${PORT:-8765}"

mkdir -p "$DATA_DIR"

echo "[run] ferricula on :$PORT  data=$DATA_DIR"
exec "$SCRIPT_DIR/ferricula" "$DATA_DIR" --serve "$PORT"
