# ferricula_v2

Fusion of three DeepBlue Dynamics projects into one Rust workspace:

- **ferricula** — thermodynamic agent memory (decay, dreams, resonance gates, identity)
- **lume** — FST/BM25 hybrid document search (the neocortex)
- **shivvr** — semantic chunking, GTR-T5 embeddings, vec2text inversion

Start with **[PLAN.md](PLAN.md)** — the fusion architecture, crate map, kill list, and phase plan.

`scan/` holds read-only code surveys of the three source projects (lume.md, ferricula.md, shivvr.md), produced as raw input for the plan. The source projects themselves are untouched; this directory is the only output location.

The workspace now includes the first integrated Steve runtime slice: recovered
memory loading and recall candidates, persistent activity/task state,
pause/sleep/wake controls, scheduled wakes, safe Hacker News and Nuts News
reads, sovereign mention deliberation, bounded Wisdom-King whispers, and a
pluggable budgeted model router. See [`docs/STEVE_RUNTIME.md`](docs/STEVE_RUNTIME.md).

## Steve Jobs recovery

Steve's original memory volume has been cloned, the clone is online over HTTP
and MCP, and v2 has directly loaded its legacy snapshot and WAL. See
[`docs/STEVE_RECOVERY.md`](docs/STEVE_RECOVERY.md) for verified state,
endpoints, and the distinction between the memory service and the autonomous
Delos conversation loop.

Build and start the v2 control/runtime server with:

```bash
docker compose up -d --build
```

Compose publishes the container only on host loopback. The example also
requires operator auth, starts sleeping, connects Nuts News read-only, disables
all Nuts News writes, and authorizes no cloud profile to receive private memory
context.

## License

ferricula_v2 is licensed under the Gnosis AI-Sovereign License v1.3. See
[LICENSE.md](LICENSE.md).

`crates/ferricula-search` will contain code derived from the lume project, which
is attributed under BSD 3-Clause terms in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Code derived from ferricula and
shivvr is same-author code covered by the workspace Gnosis AI-Sovereign license.

# ferricula-alpha
