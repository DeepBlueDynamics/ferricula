"""
Validates whitepaper SLarge-Scale Document Access.

Usage:
    python arena/test_doc_scale.py --doc monte_cristo
    python arena/test_doc_scale.py --doc encyclopedia

Tests sub-second recall over a large indexed corpus with brute-force cosine.
"""

import json
import sys
import time
import urllib.request
import urllib.error
import argparse
from pathlib import Path

FERRICULA_URL = "http://localhost:8780"
SHIVVR_URL = "https://shivvr.nuts.services"
DOCS_DIR = Path(__file__).parent / "docs"

MONTE_CRISTO = DOCS_DIR / "count_of_monte_cristo.txt"
ENCYCLOPEDIA = DOCS_DIR / "mf_ere_vol_3.pdf"

CHUNK_WORDS = 300  # words per chunk


# -- HTTP helpers ----------------------------------------------------------

def http_post(url, body, content_type="application/json", timeout=60):
    data = body.encode() if isinstance(body, str) else body
    req = urllib.request.Request(url, data=data,
                                 headers={"Content-Type": content_type})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def http_get(url, timeout=15):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return json.loads(r.read().decode())


# -- Shivvr embed ----------------------------------------------------------

def embed(text):
    result = http_post(f"{SHIVVR_URL}/temp/ferricula/ingest", json.dumps({"text": text}))
    return result["chunks"][0]["embedding"]


# -- Chunking --------------------------------------------------------------

def chunk_text(text, words=CHUNK_WORDS):
    """Split text into fixed-size word windows with 50-word overlap."""
    tokens = text.split()
    step = words - 50
    chunks = []
    i = 0
    while i < len(tokens):
        chunk = " ".join(tokens[i:i + words])
        if len(chunk.strip()) > 40:
            chunks.append(chunk)
        i += step
    return chunks


# -- Ferricula store -------------------------------------------------------

def remember(mid, text, vector, channel="seeing", alpha=0.010, keystone=False):
    row = {
        "id": mid,
        "tags": {"channel": channel, "text": text[:200]},
        "vector": vector,
        "decay_alpha": alpha,
    }
    if keystone:
        row["keystone"] = True
    http_post(f"{FERRICULA_URL}/remember", json.dumps(row))


def recall_timed(query):
    t0 = time.perf_counter()
    result = http_post(f"{FERRICULA_URL}/recall", json.dumps({"query": query}))
    ms = (time.perf_counter() - t0) * 1000
    return ms, result


# -- Ingest pipeline -------------------------------------------------------

def ingest_text_file(path, label):
    print(f"\n-- Ingesting {label} ({Path(path).stat().st_size / 1e6:.1f} MB) --")
    text = Path(path).read_text(encoding="utf-8", errors="replace")
    chunks = chunk_text(text)
    print(f"   {len(chunks)} chunks @ {CHUNK_WORDS} words each")

    t_start = time.perf_counter()
    base_id = int(time.time() * 1000) % (2**30)
    errors = 0

    for i, chunk in enumerate(chunks):
        mid = base_id + i
        try:
            vec = embed(chunk)
            remember(mid, chunk, vec)
        except Exception as e:
            errors += 1
            if errors <= 3:
                print(f"   [warn] chunk {i}: {e}")
            continue

        if (i + 1) % 50 == 0 or i == len(chunks) - 1:
            elapsed = time.perf_counter() - t_start
            rate = (i + 1) / elapsed
            eta = (len(chunks) - i - 1) / rate if rate > 0 else 0
            print(f"   {i+1}/{len(chunks)}  {rate:.1f} chunks/s  ETA {eta:.0f}s", end="\r")

    elapsed = time.perf_counter() - t_start
    print(f"\n   done: {len(chunks) - errors} stored, {errors} errors, {elapsed:.1f}s total")
    print(f"   avg ingest rate: {(len(chunks)-errors)/elapsed:.1f} chunks/s")
    return len(chunks) - errors


def ingest_pdf(path, label):
    """Extract text from PDF and ingest. Requires pypdf."""
    try:
        from pypdf import PdfReader
    except ImportError:
        print("pypdf not installed -- run: pip install pypdf")
        sys.exit(1)

    print(f"\n-- Ingesting {label} ({Path(path).stat().st_size / 1e6:.1f} MB PDF) --")
    reader = PdfReader(path)
    print(f"   {len(reader.pages)} pages, extracting text...")

    full_text = ""
    for page in reader.pages:
        full_text += page.extract_text() or ""

    print(f"   extracted {len(full_text)/1e6:.2f} MB of text")
    return ingest_text_file.__wrapped__(full_text, label) if hasattr(ingest_text_file, '__wrapped__') \
        else _ingest_raw(full_text, label)


def _ingest_raw(text, label):
    chunks = chunk_text(text)
    print(f"   {len(chunks)} chunks @ {CHUNK_WORDS} words each")
    t_start = time.perf_counter()
    base_id = int(time.time() * 1000) % (2**30)
    errors = 0
    for i, chunk in enumerate(chunks):
        mid = base_id + i
        try:
            vec = embed(chunk)
            remember(mid, chunk, vec)
        except Exception as e:
            errors += 1
            if errors <= 3:
                print(f"   [warn] chunk {i}: {e}")
            continue
        if (i + 1) % 50 == 0 or i == len(chunks) - 1:
            elapsed = time.perf_counter() - t_start
            rate = (i + 1) / elapsed
            eta = (len(chunks) - i - 1) / rate if rate > 0 else 0
            print(f"   {i+1}/{len(chunks)}  {rate:.1f} chunks/s  ETA {eta:.0f}s", end="\r")
    elapsed = time.perf_counter() - t_start
    print(f"\n   done: {len(chunks) - errors} stored, {errors} errors, {elapsed:.1f}s total")
    return len(chunks) - errors


# -- Recall benchmark ------------------------------------------------------

MONTE_CRISTO_QUERIES = [
    "Edmond Dantes imprisoned in Chateau d'If",
    "Abbe Faria hidden treasure on Monte Cristo",
    "Count of Monte Cristo revenge against enemies",
    "Mercedes and Fernand Mondego betrayal",
    "Haydee testimony against Fernand",
]

ENCYCLOPEDIA_QUERIES = [
    "Buddhist doctrine of impermanence and suffering",
    "Ethics of revenge and justice in religious tradition",
    "Christian theology atonement and redemption",
    "Hindu concepts of dharma and karma",
    "Death and afterlife across religious traditions",
]


def run_recall_benchmark(queries, label):
    print(f"\n-- Recall benchmark: {label} --")
    status = http_get(f"{FERRICULA_URL}/status")
    print(f"   {status['result'].strip().split(chr(10))[1].strip()}")  # memories line

    latencies = []
    errors = 0
    for q in queries:
        ms, result = recall_timed(q)
        latencies.append(ms)
        body = result.get("result", "") if isinstance(result, dict) else str(result)
        if body.startswith("error:"):
            errors += 1
            print(f"   {ms:6.1f}ms  [ERR]  \"{q[:60]}\"")
            print(f"          {body[:120]}")
        else:
            resonant = body.count("id=")
            # extract cosine candidate count from "resonance=N/M"
            import re
            m = re.search(r"resonance=(\d+)/(\d+)", body)
            total = int(m.group(2)) if m else "?"
            print(f"   {ms:6.1f}ms  [{resonant} resonant / {total} found]  \"{q[:60]}\"")

    avg = sum(latencies) / len(latencies)
    mx = max(latencies)
    ok = mx < 1000 and errors == 0
    print(f"\n   avg={avg:.1f}ms  max={mx:.1f}ms  errors={errors}  sub-second={'OK' if ok else 'FAIL'}")
    return latencies


# -- Main ------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--doc", choices=["monte_cristo", "encyclopedia", "both"],
                        default="monte_cristo")
    parser.add_argument("--skip-ingest", action="store_true",
                        help="Skip ingestion, just run recall benchmark")
    parser.add_argument("--chunks", type=int, default=CHUNK_WORDS,
                        help="Words per chunk (default 300)")
    args = parser.parse_args()

    # Verify services
    print("-- Service check --")
    try:
        s = http_get(f"{FERRICULA_URL}/status")
        print(f"   ferricula: ok ({FERRICULA_URL})")
    except Exception as e:
        print(f"   ferricula: UNREACHABLE -- {e}")
        sys.exit(1)
    try:
        http_post(f"{SHIVVR_URL}/temp/ferricula/ingest", json.dumps({"text": "test"}))
        print(f"   shivvr: ok ({SHIVVR_URL})")
    except Exception as e:
        print(f"   shivvr: UNREACHABLE -- {e}")
        sys.exit(1)

    if args.doc in ("monte_cristo", "both"):
        if not args.skip_ingest:
            ingest_text_file(MONTE_CRISTO, "Count of Monte Cristo")
            print("   cooling down shivvr (10s)...")
            time.sleep(10)
        run_recall_benchmark(MONTE_CRISTO_QUERIES, "Monte Cristo")

    if args.doc in ("encyclopedia", "both"):
        if not args.skip_ingest:
            ingest_pdf(ENCYCLOPEDIA, "Encyclopaedia of Religion and Ethics Vol.3")
            print("   cooling down shivvr (10s)...")
            time.sleep(10)
        run_recall_benchmark(ENCYCLOPEDIA_QUERIES, "Encyclopedia")


if __name__ == "__main__":
    main()
