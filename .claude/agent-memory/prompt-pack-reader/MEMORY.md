# Prompt Pack Reader - Persistent Memory

## Ferricula Architecture
- Location: `C:\Users\kord\Code\gnosis\ferricula\pilosa-memory\`
- Three agents: Scribe (interface/persistence), Steward (thermodynamic lifecycle), Weaver (graph/terms)
- Agent defs: `agents/scribe.md`, `agents/steward.md`, `agents/weaver.md`
- MCP server: `tools/ferricula-mcp.py` wraps ferricula binary as subprocess
- Depends on gnosis-chunk (chonk) on localhost:8080 for embedding/inversion
- Vectors never exposed to LLM -- text in, text out

## Key Patterns Identified
- **Jurisdiction model**: Each agent owns specific source files, no overlap
- **Pipeline interaction**: Scribe -> Steward -> Weaver (interface -> lifecycle -> structure)
- **Thermodynamic decay**: `fidelity *= exp(-alpha_eff)`, adaptive alpha [0.001, 0.02]
- **Use-based persistence**: Recall shrinks alpha (0.95x), neglect grows it (1.005x)
- **Consolidation = emergent importance**: depth reduces effective alpha
- **Sensory channels**: hearing/seeing/thinking with distinct decay profiles
- **WAL-first persistence**: postcard binary, V2 snapshots, atomic checkpoint

## Known Gaps (Identified in First Analysis)
- No automatic dream trigger (must be explicitly invoked)
- No anchor/keystone quota enforcement
- No death spiral detection
- No fidelity histogram in status
- No inter-agent offering protocol or per-agent memory scoping
- Consolidation doesn't compute centroid vectors
- Emotion stored but not connected to decay/scoring machinery
- Planner is stubby without AGENT_KEY

## Pack Analysis Methodology
1. Read all source files to understand data flow, not just agent docs
2. Map jurisdiction boundaries (which agent owns which files)
3. Trace trigger conditions (when does each agent activate?)
4. Identify the memory strategy (creation -> reinforcement -> decay -> death)
5. Find gaps (missing triggers, uncovered states, disconnected knobs)
6. Assess value vs. bare prompt (what does the architecture buy you?)
