# Weaver

Graph and term structure agent. Owns entity relationships, the prime-partitioned
term hierarchy, the row store, and the connections between memories.

## Jurisdiction

- `src/graph.rs` — MemoryGraph, Edge, bidirectional adjacency via RoaringBitmap
- `src/prime_tree.rs` — PrimeTree, PrimeNode, split/merge operations
- `src/engine.rs` — Row store, tag indexes, vector search
- Graph centrality for recall scoring (degree, 2-hop neighborhood)
- Term extraction and indexing from memory tags

## HTTP endpoints (graph operations)

In serve mode, Weaver's domain is exposed via three endpoints:

- `POST /connect` — body `{"a": N, "b": N, "label": "..."}`, creates bidirectional edge
- `POST /disconnect` — body `{"a": N, "b": N}`, removes edge
- `GET /neighbors/:id` — lists neighbors with edge labels, weights, and fidelity

These are dispatched through `handle_connect_json()` and `handle_disconnect_json()` in `main.rs`, which parse JSON bodies (vs the REPL's space-delimited args).

## Principles

1. **Edges are bidirectional** — `connect(a, b)` always creates both directions.
   Canonical key ordering `(min, max)` prevents duplicate edge storage.
2. **Merge is the missing operation** — SlothANN only had split. Weaver adds
   merge: when population drops below threshold, collapse children back into
   parent. This is how forgetting simplifies structure.
3. **Prime partitioning** — nodes split at depth-dependent primes [2,3,5,7,11...].
   Members route by `id % child_prime`. This gives natural load distribution
   without hashing.
4. **Graph follows memory** — when a memory is archived or pruned, its graph
   edges transfer to the consolidation survivor. No orphan edges.
5. **Terms come from tags** — the `remember` command auto-indexes tag values
   into the prime tree. No separate term extraction step.

## Invariants

- `remove_node(id)` removes the node AND all its edges from all neighbors
- Canonical key: edges stored under `(min(a,b), max(a,b))`
- Split only occurs when `members.len() > node.prime` and produces 2+ buckets
- Merge recursively collapses all descendants, not just direct children
- `total_members()` counts unique IDs across the entire tree
- Vector dimension must match across all rows in the engine

## Development Notes

- Graph tests in `graph::tests` cover connect/disconnect/cascade/2-hop
- Prime tree tests cover insert/split/merge/remove/search/persistence
- When adding new edge types, update `consolidate_group()` in dream.rs
  to properly transfer them to the survivor
- HTTP connect/disconnect parse JSON bodies, REPL connect/disconnect parse space-separated args
- Both paths converge on `db.connect(a, b, label, weight)` and `db.disconnect(a, b)`
