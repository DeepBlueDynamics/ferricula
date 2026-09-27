# Model-backed task harness

The harness separates generative work from narrow judgments. A small LLM produces a candidate; a decision backend checks it against caller-supplied external evidence. An uncertain or malformed decision can escalate once. The run then ends with accept, reject or abstain.

The implementation lives in `ferricula-server::harness`. `GenerationBackend` and `DecisionBackend` are independent traits: a classifier can implement the decision trait without offering a generation endpoint. `RoutedBackend` uses existing local or API model profiles and strict JSON decisions. It is a generic small-judge cascade, not a reproduction of JEV or a trained JEV checkpoint.

## Run a task

Configure the existing model routing schema with separate routes:

| Route | Role |
|---|---|
| `summarize` | Small generative LLM |
| `scan` | Small decision model or JSON-capable LLM |
| `deliberate` | Optional stronger decision model |

Profiles specify provider, model identifier, base URL, environment-variable name for credentials, context limit, timeout, capabilities and configured token prices. Local compatible HTTP services and configured API providers use the same interface. Pin actual model versions for comparisons. Prices in a profile are operator configuration, not a live pricing feed.

Supply a task JSON object on standard input:

```json
{
  "task": "Report whether the test run passed.",
  "evidence": [
    {"id": "test-1", "text": "Test runner exited 0: 8 passed, 0 failed."}
  ]
}
```

Run from the v2 workspace:

```bash
cargo run --locked -p ferricula-server --example task_harness -- models.toml usage.json 0.25 < task.json
```

This uses a daily budget of $0.25 with the supplied usage journal. All workers sharing a budget must use that journal and the same pricing configuration. The example refuses concurrent use of one journal. A crash may leave a lock file; verify the old process exited before removing that lock. The journal records model/token/cost metadata, not task text. The candidate and verdict go to stdout and are not added to memory.

The example treats evidence as private context. Profiles must explicitly advertise the existing `private_context` capability before receiving it. API keys are resolved from environment-variable names at request time. Ordinary default routes may select the same local model for every stage; configure distinct profiles to measure a small-versus-large cascade.

## Read-time curation in chat

Set `curator_enabled = true` in the runtime TOML to add a read-time briefing before the chat answer. It defaults to false. The `summarize` route supplies the generative curator; the existing chat route supplies the answer. Both use the same usage ledger and budget checks.

This first adapter reads actual `text` tags from the identity-verified recovered store. It excludes released records, does not invent source text from titles or metadata, and treats legacy facts as unclassified facts rather than successful task trajectories. It does not yet hydrate external Lume sections. Briefings are local variables only; they are not attached to stored chat turns, memory rows or training data. A no-model or truncated curator response fails the request rather than masquerading as successful curation. An empty eligible selection needs no model call.

## Judgment and limits

The routed decision schema is:

```json
{"verdict":"accept","confidence":0.9,"evidence_ids":["test-1"]}
```

Unknown fields, invalid JSON, invalid confidence, absent acceptance evidence, or invented evidence references result in abstention or the single escalation. A no-model echo is an error, never a successful judgment. Empty, duplicate-ID and oversized task inputs are rejected before inference. Candidate output size is bounded.

The confidence from a JSON-generating model is self-reported and uncalibrated. The threshold is only an escalation heuristic. This module does not authorize lifecycle changes or train gates. Evidence references do not prove entailment, and acceptance is a model judgment, not a verified task result. Tool execution and ground-truth checking belong to the caller.

Every completed stage reports its model, provider-reported token counts and cost estimated from configured rates. Missing usage is returned as null, never claimed as zero cost. The spending journal reserves missing counts from request bounds and marks the entry `usage_estimated`; a paid profile with such an entry is blocked for the rest of that UTC day, including after journal reload. These reservations are not measured token counts, and old journal entries are not retroactively reclassified. Provider failures can have unknown charges; the router cannot recover provider invoices. Budget checks use configured estimates and are not a billing guarantee.

The library is synchronous; asynchronous server integrations must call it outside state locks, for example through `spawn_blocking`. Callers should share and persist the router usage ledger. The example demonstrates that journal handling, including saving usage if a later stage fails.

## Validation

```bash
cargo test --locked -p ferricula-server --test harness
cargo check --locked -p ferricula-server --example task_harness
```

The integration tests use scripted transports. They establish routing, bounds and accounting behavior; they do not measure real-model accuracy, calibration, latency or public benchmark performance.
