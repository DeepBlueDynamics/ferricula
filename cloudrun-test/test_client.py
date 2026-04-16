#!/usr/bin/env python3
"""Smoke test for ferricula + shivvr.

Run ferricula first:
  chmod +x ferricula run.sh
  ./run.sh

Then in another terminal:
  pip install httpx
  python test_client.py
"""

import asyncio
import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from arena.clients import FerriculaClient, ShivvrClient

FERRICULA_URL = os.environ.get("FERRICULA_URL", "http://localhost:8765")
SHIVVR_URL    = os.environ.get("SHIVVR_URL", "https://shivvr.nuts.services")


async def main():
    ferricula = FerriculaClient(FERRICULA_URL)
    shivvr    = ShivvrClient(SHIVVR_URL)

    print(f"ferricula: {FERRICULA_URL}")
    print(f"shivvr:    {SHIVVR_URL}")
    print()

    # Health checks
    print("── health ──────────────────────────────")
    ok = await ferricula.available()
    print(f"  ferricula reachable: {ok}")
    if not ok:
        print("  ERROR: ferricula not running. Start it with ./run.sh")
        return

    ok = await shivvr.available()
    print(f"  shivvr reachable:    {ok}")
    if not ok:
        print("  ERROR: shivvr not reachable")
        return

    # Status
    print()
    print("── status ──────────────────────────────")
    status = await ferricula.status()
    print(f"  memories={status.memories}  active={status.active}  keystones={status.keystones}")
    print(f"  graph    nodes={status.graph_nodes}  edges={status.graph_edges}")

    # Remember
    print()
    print("── remember ────────────────────────────")
    test_text = "The best products are the ones that make you feel something."
    vec = await shivvr.embed(test_text)
    print(f"  embedded: {len(vec)}d vector")
    mid = await ferricula.remember(test_text, vec,
                                   channel="thinking",
                                   importance=0.8)
    print(f"  stored → id={mid}")

    # Recall
    print()
    print("── recall ──────────────────────────────")
    hits = await ferricula.recall_text("great products design", shivvr, k=3)
    print(f"  query: 'great products design'  hits={len(hits)}")
    for h in hits:
        print(f"  id={h.id}  fidelity={h.fidelity:.3f}  recalls={h.recalls}")

    # Inspect the memory we just stored
    print()
    print("── inspect ─────────────────────────────")
    info = await ferricula.inspect(mid)
    print(f"  id={info.id}  state={info.state}  fidelity={info.fidelity:.3f}")
    print(f"  alpha={info.decay_alpha}  importance={info.importance}")

    print()
    print("── all good ✓ ──────────────────────────")


if __name__ == "__main__":
    asyncio.run(main())
