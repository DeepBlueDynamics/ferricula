"""Ferricula Cognitive MCP Server — thermodynamic memory for AI agents.

Spawns the ferricula binary as a subprocess and exposes cognitive
memory operations as MCP tools over stdio.

Supports two transport modes:
  - subprocess (default): spawns ferricula binary, communicates via stdin/stdout
  - HTTP: connects to ferricula HTTP service (--http-url http://localhost:8765)

Multi-instance: target different characters by name or port.
  --port 8780              shorthand for --http-url http://localhost:8780
  --name assis             label the instance (auto-discovered from /identity if omitted)
  target="assis"           on any tool call, routes to that character's instance

The `discover` tool scans ports and calls /identity to find running characters.

Vectors are NEVER exposed to the LLM. Text is embedded via shivvr
(gnosis-chunk on :8080), stored internally, and inverted back to
text on recall.

Three sensory channels with distinct decay profiles:
  - hearing:  external input (alpha=0.010)
  - seeing:   file/visual observation (alpha=0.010, keystoned)
  - thinking: working memory (alpha=0.015, decays faster)

Usage (registered in .mcp.json):
    python tools/ferricula-mcp.py [--data-dir ./data] [--http-url http://localhost:8765]
    python tools/ferricula-mcp.py --port 8780
    python tools/ferricula-mcp.py --port 8780 --name assis
"""

import json
import subprocess
import sys
import threading
import os
import re
import time
import urllib.parse
import urllib.request
import urllib.error
from pathlib import Path
from typing import Optional

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("ferricula")

PROJECT_DIR = Path(__file__).resolve().parent.parent
DEFAULT_DATA_DIR = PROJECT_DIR / "data"

# ── Surface Scoping ─────────────────────────────────────────────────────
# Controls which tools are registered. External LLM agents get "cognitive",
# internal archetypes/system agents get "system", operators get "all".

COGNITIVE_TOOLS = {
    "remember", "recall", "reflect", "observe", "inspect",
    "connect", "neighbors", "status", "health", "identity", "embody",
}
SYSTEM_TOOLS = {
    "dream", "keystone", "checkpoint", "offer_entropy",
    "inversion_check", "terms", "query", "disconnect", "clock",
}

_surface = os.environ.get("FERRICULA_SURFACE", "all")
for _i, _arg in enumerate(sys.argv):
    if _arg == "--surface" and _i + 1 < len(sys.argv):
        _surface = sys.argv[_i + 1]


def _register(name: str) -> bool:
    """Return True if this tool should be registered for the current surface."""
    if _surface == "all":
        return True
    if _surface == "cognitive":
        return name in COGNITIVE_TOOLS
    if _surface == "system":
        return name in SYSTEM_TOOLS
    return True


def _tool(name: str):
    """Conditional tool decorator. Registers with FastMCP only if surface allows."""
    if _register(name):
        return mcp.tool()
    return lambda f: f  # no-op: function exists but isn't exposed via MCP

# ── Channel Profiles ─────────────────────────────────────────────────────

CHANNELS = {
    "hearing":  {"alpha": 0.010, "description": "external input"},
    "seeing":   {"alpha": 0.010, "description": "file/visual observation"},
    "thinking": {"alpha": 0.015, "description": "working memory"},
    "body":     {"alpha": 0.020, "description": "interoception — agent self-monitoring"},
    "taste":    {"alpha": 0.018, "description": "evaluation — quality judgments from confer"},
    "smell":    {"alpha": 0.020, "description": "ambient — emerging patterns from SKG"},
}


# ── HttpClient ──────────────────────────────────────────────────────────

class HttpClient:
    """HTTP client for ferricula service on localhost."""

    def __init__(self, base_url: str):
        self.base_url = base_url.rstrip("/")

    def send(
        self,
        endpoint: str,
        method: str = "GET",
        body: Optional[str] = None,
        content_type: str = "application/json",
    ) -> str:
        """Send request to ferricula HTTP service, return response body."""
        url = f"{self.base_url}/{endpoint.lstrip('/')}"
        data = body.encode("utf-8") if body else None
        req = urllib.request.Request(
            url,
            data=data,
            headers={"Content-Type": content_type} if data else {},
            method=method,
        )
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                return resp.read().decode("utf-8")
        except urllib.error.HTTPError as e:
            return e.read().decode("utf-8") if e.fp else f'{{"error": "{e}"}}'
        except Exception as e:
            return f'{{"error": "{e}"}}'

    def get(self, endpoint: str) -> str:
        return self.send(endpoint, "GET")

    def post(self, endpoint: str, body: str = "{}") -> str:
        return self.send(endpoint, "POST", body)

    def post_raw(self, endpoint: str, body: str = "") -> str:
        return self.send(endpoint, "POST", body, content_type="text/plain")

    def available(self) -> bool:
        try:
            resp = self.get("status")
            return "error" not in resp.lower()[:50]
        except Exception:
            return False


# ── ShivvrClient ──────────────────────────────────────────────────────────

class ShivvrClient:
    """HTTP client for gnosis-chunk embedding service on localhost:8080."""

    def __init__(self, base_url: str = "http://localhost:8080"):
        self.base_url = base_url

    def _post(self, path: str, payload: dict) -> dict:
        url = f"{self.base_url}{path}"
        data = json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(
            url, data=data,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))

    def _get(self, path: str) -> dict:
        url = f"{self.base_url}{path}"
        req = urllib.request.Request(url)
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode("utf-8"))

    def embed(self, text: str) -> list[float]:
        """Embed text via shivvr, return dense vector."""
        result = self._post("/temp/ferricula/ingest", {"text": text})
        return result["chunks"][0]["embedding"]

    def invert(self, vector: list[float]) -> str:
        """Invert a vector back to approximate text via shivvr."""
        result = self._post("/invert", {"embedding": vector})
        return result.get("text", result.get("hypothesis", "[inversion failed]"))

    def health(self) -> dict:
        """Check shivvr health."""
        return self._get("/health")

    def available(self) -> bool:
        """Check if shivvr is reachable."""
        try:
            self.health()
            return True
        except Exception:
            return False


# ── SeedJournal ──────────────────────────────────────────────────────────

class SeedJournal:
    """Append-only JSONL journal for replaying memories into a fresh instance."""

    def __init__(self, path: Path):
        self.path = path
        self.path.parent.mkdir(parents=True, exist_ok=True)

    def append(self, op: str, **kwargs):
        """Serialize one seed line and append to the journal."""
        entry = {"op": op, "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **kwargs}
        with open(self.path, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")

    def read_all(self) -> list[dict]:
        """Read all seed entries."""
        if not self.path.exists():
            return []
        entries = []
        with open(self.path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        entries.append(json.loads(line))
                    except json.JSONDecodeError:
                        continue
        return entries

    def exists(self) -> bool:
        """True if journal file exists and has content."""
        return self.path.exists() and self.path.stat().st_size > 0


# ── IdAllocator ───────────────────────────────────────────────────────────

class IdAllocator:
    """Thread-safe ID allocator backed by ferricula's maxid command."""

    def __init__(self, repl: "ReplProcess"):
        self._lock = threading.Lock()
        resp = repl.send("maxid")
        try:
            self._current = int(resp.strip())
        except ValueError:
            self._current = 0

    def next(self) -> int:
        with self._lock:
            self._current += 1
            return self._current


# ── Find binary ──────────────────────────────────────────────────────────

def _find_binary():
    candidates = [
        PROJECT_DIR / "target" / "release" / "ferricula.exe",
        PROJECT_DIR / "target" / "release" / "ferricula",
        PROJECT_DIR / "target" / "debug" / "ferricula.exe",
        PROJECT_DIR / "target" / "debug" / "ferricula",
    ]
    for c in candidates:
        if c.exists():
            return str(c)
    return None


# ── ReplProcess ────────────────────────────────────────────────────────

class ReplProcess:
    """Manages a long-running ferricula subprocess."""

    MAX_CLOCK_EVENTS = 50

    def __init__(self, data_dir: str):
        self.data_dir = data_dir
        self.proc: Optional[subprocess.Popen] = None
        self.lock = threading.Lock()
        self._clock_events: list[str] = []
        self._start()

    def _start(self):
        binary = _find_binary()
        if not binary:
            raise RuntimeError(
                f"ferricula binary not found. Run `cargo build --release` in {PROJECT_DIR}"
            )
        env = os.environ.copy()
        env["NO_COLOR"] = "1"  # skip TUI splash in subprocess mode
        # Pass through clock config from MCP env
        for key in ("RADIO_URL", "CLOCK_TICK_SECS", "DREAM_THRESHOLD_BYTES"):
            val = os.environ.get(key)
            if val:
                env[key] = val
        self.proc = subprocess.Popen(
            [binary, self.data_dir],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=env,
        )
        # Consume the startup banner (read until first prompt)
        self._read_until_prompt()

    def _read_until_prompt(self) -> str:
        """Read stdout until we see '\\n> ' prompt. Return content before it.
        Lines starting with [clock] are buffered separately."""
        buf = []
        while True:
            ch = self.proc.stdout.read(1)
            if not ch:
                break
            buf.append(ch)
            text = "".join(buf)
            if text.endswith("\n> ") or (len(buf) == 2 and text == "> "):
                raw = text.rstrip("> \n").strip()
                # Separate clock events from command output
                output_lines = []
                for line in raw.split("\n"):
                    stripped = line.strip()
                    if stripped.startswith("[clock]"):
                        self._clock_events.append(stripped)
                        # Cap at MAX_CLOCK_EVENTS (FIFO)
                        if len(self._clock_events) > self.MAX_CLOCK_EVENTS:
                            self._clock_events = self._clock_events[-self.MAX_CLOCK_EVENTS:]
                    else:
                        output_lines.append(line)
                return "\n".join(output_lines).strip()
        return "".join(buf).strip()

    def drain_clock_events(self) -> list[str]:
        """Return and clear buffered [clock] events."""
        events = self._clock_events[:]
        self._clock_events.clear()
        return events

    def send(self, command: str) -> str:
        """Send a command and return the response."""
        with self.lock:
            if self.proc is None or self.proc.poll() is not None:
                self._start()
            self.proc.stdin.write(command + "\n")
            self.proc.stdin.flush()
            return self._read_until_prompt()

    def shutdown(self):
        with self.lock:
            if self.proc and self.proc.poll() is None:
                self.proc.stdin.write("exit\n")
                self.proc.stdin.flush()
                self.proc.wait(timeout=5)


# ── Instance Registry ────────────────────────────────────────────────────
# Maps character name (lowercase) → HttpClient. Allows targeting any
# running ferricula instance by name or port.

_registry: dict[str, HttpClient] = {}
_port_map: dict[int, str] = {}  # port → character name


def _register_instance(url: str, name: Optional[str] = None) -> Optional[str]:
    """Register a ferricula instance. Auto-discovers name from /identity if not given.
    Returns the discovered/given name, or None on failure."""
    client = HttpClient(url)
    if name is None:
        try:
            raw = client.get("identity")
            data = json.loads(raw)
            name = data.get("name", "").strip().lower()
        except Exception:
            pass
    if not name:
        # Fall back to port-based name
        parsed = urllib.parse.urlparse(url)
        name = f"port-{parsed.port or 8765}"
    name = name.lower()
    _registry[name] = client
    # Track port
    try:
        parsed = urllib.parse.urlparse(url)
        if parsed.port:
            _port_map[parsed.port] = name
    except Exception:
        pass
    return name


def _resolve_target(target: Optional[str] = None) -> Optional[HttpClient]:
    """Resolve a target name/port to an HttpClient, or return default."""
    if target:
        t = target.strip().lower()
        # Try name first
        if t in _registry:
            return _registry[t]
        # Try as port number
        try:
            port = int(t)
            if port in _port_map:
                return _registry[_port_map[port]]
            # Auto-register new port
            url = f"http://localhost:{port}"
            name = _register_instance(url)
            if name:
                return _registry[name]
        except ValueError:
            pass
        return None
    # No target specified — use default (first registered, or _http_client)
    if _registry:
        return next(iter(_registry.values()))
    if _http_client is not None:
        return _http_client
    return None


# ── Globals ──────────────────────────────────────────────────────────────

# Parse args
_data_dir = str(DEFAULT_DATA_DIR)
_http_url: Optional[str] = None
_cli_port: Optional[int] = None
_cli_name: Optional[str] = None
for i, arg in enumerate(sys.argv):
    if arg == "--data-dir" and i + 1 < len(sys.argv):
        _data_dir = sys.argv[i + 1]
    elif arg == "--http-url" and i + 1 < len(sys.argv):
        _http_url = sys.argv[i + 1]
    elif arg == "--port" and i + 1 < len(sys.argv):
        _cli_port = int(sys.argv[i + 1])
    elif arg == "--name" and i + 1 < len(sys.argv):
        _cli_name = sys.argv[i + 1]

# --port is shorthand for --http-url http://localhost:{port}
if _cli_port and not _http_url:
    _http_url = f"http://localhost:{_cli_port}"

# Also check env var
if not _http_url:
    _http_url = os.environ.get("FERRICULA_URL")

_repl: Optional[ReplProcess] = None
_http_client: Optional[HttpClient] = None
_shivvr: Optional[ShivvrClient] = None
_ids: Optional[IdAllocator] = None
_journal: Optional[SeedJournal] = None
_restored: bool = False
_replaying: bool = False  # suppress journaling during seed replay


def _use_http(target: Optional[str] = None) -> bool:
    """Return True if we should use HTTP transport."""
    if target and _resolve_target(target):
        return True
    return _http_url is not None


def _get_http(target: Optional[str] = None) -> HttpClient:
    """Get HttpClient for the given target, or the default."""
    if target:
        client = _resolve_target(target)
        if client:
            return client
    global _http_client
    if _http_client is None:
        _http_client = HttpClient(_http_url)
    return _http_client


def _get_repl() -> ReplProcess:
    global _repl
    if _repl is None:
        os.makedirs(_data_dir, exist_ok=True)
        _repl = ReplProcess(_data_dir)
    return _repl


def _get_shivvr() -> ShivvrClient:
    global _shivvr
    if _shivvr is None:
        shivvr_url = (
            os.environ.get("SHIVVR_URL")
            or "http://localhost:8080"
        )
        _shivvr = ShivvrClient(shivvr_url)
    return _shivvr


def _get_ids() -> IdAllocator:
    global _ids
    if _ids is None:
        _ids = IdAllocator(_get_repl())
    return _ids


def _get_journal() -> SeedJournal:
    global _journal
    if _journal is None:
        seed_path = os.environ.get("FERRICULA_SEEDS")
        if seed_path:
            _journal = SeedJournal(Path(seed_path))
        else:
            _journal = SeedJournal(Path(_data_dir) / "seeds.jsonl")
    return _journal


# Auto-register the CLI-provided instance
if _http_url:
    _initial_name = _register_instance(_http_url, _cli_name)
    if _initial_name:
        print(f"registered: {_initial_name} @ {_http_url}", file=sys.stderr)


def _restore_if_empty():
    """On first call, check if ferricula is empty and replay seeds if so."""
    global _restored
    if _restored:
        return
    _restored = True

    journal = _get_journal()
    if not journal.exists():
        return

    # Check current memory count
    try:
        if _use_http():
            raw = _get_http().get("status")
        else:
            raw = _get_repl().send("status")
        # Look for rows=0 or memories=0 in status output
        rows_match = re.search(r"rows=(\d+)", raw)
        if rows_match and int(rows_match.group(1)) > 0:
            return  # ferricula already has memories, skip
    except Exception:
        return  # can't determine state, don't replay

    seeds = journal.read_all()
    if not seeds:
        return

    global _replaying
    _replaying = True
    restored = 0
    for seed in seeds:
        op = seed.get("op")
        try:
            if op == "remember":
                ferricula_remember(
                    text=seed["text"],
                    channel=seed.get("channel", "hearing"),
                    emotion=seed.get("emotion"),
                    importance=seed.get("importance", 0.0),
                    keystone=seed.get("keystone", False),
                )
                restored += 1
            elif op == "reflect":
                ferricula_reflect(
                    thought=seed["text"],
                    importance=seed.get("importance", 0.0),
                )
                restored += 1
            elif op == "observe":
                ferricula_observe(
                    path=seed["path"],
                    summary=seed.get("summary"),
                )
                restored += 1
        except Exception:
            continue  # skip bad seeds, keep going

    _replaying = False

    if restored:
        print(f"restored {restored} memories from seed journal", file=sys.stderr)


def _invert_recall_results(raw: str) -> str:
    """Parse recall output, invert vectors for each hit, return enriched text."""
    shivvr = _get_shivvr()
    repl = _get_repl()

    # Extract IDs from recall output: "  id=N fidelity=..."
    id_pattern = re.compile(r"id=(\d+)")
    ids = [int(m.group(1)) for m in id_pattern.finditer(raw)]

    if not ids:
        return raw

    lines = []
    for mid in ids:
        # Touch for recall stats
        repl.send(f"touch {mid}")

        # Get row data to access vector
        row_resp = repl.send(f"get {mid}")
        inspect_resp = repl.send(f"inspect {mid}")

        # Try to invert the vector
        inverted = None
        try:
            row_data = json.loads(row_resp)
            text_tag = row_data.get("tags", {}).get("text", None)
            if text_tag:
                inverted = text_tag
        except (json.JSONDecodeError, KeyError):
            pass

        # Build output line
        line = f"  [{mid}]"
        if inverted:
            line += f" {inverted}"

        # Extract fidelity from inspect output
        fid_match = re.search(r"fidelity=([\d.]+)", inspect_resp)
        if fid_match:
            line += f"  (fidelity={fid_match.group(1)})"

        # Extract emotion
        emo_match = re.search(r"emotion=(\S+)", inspect_resp)
        if emo_match and emo_match.group(1) != "-":
            line += f"  emotion={emo_match.group(1)}"

        # Extract channel from tags
        try:
            row_data = json.loads(row_resp)
            channel = row_data.get("tags", {}).get("channel", None)
            if channel:
                line += f"  [{channel}]"
        except (json.JSONDecodeError, KeyError):
            pass

        lines.append(line)

    return f"recalled {len(ids)} memories:\n" + "\n".join(lines)


# ── Cognitive Tools ──────────────────────────────────────────────────────


@_tool("remember")
def ferricula_remember(
    text: str,
    channel: str = "hearing",
    emotion: Optional[dict] = None,
    importance: float = 0.0,
    keystone: bool = False,
    target: Optional[str] = None,
) -> str:
    """Remember something — embeds text automatically, never requires vectors.

    Three sensory channels control decay rate:
      - hearing: external input, standard decay
      - seeing: file/visual observation, standard decay
      - thinking: working memory, faster decay

    Args:
        text: The text to remember (will be embedded automatically).
        channel: Sensory channel — "hearing", "seeing", or "thinking".
        emotion: Optional {"primary": str, "secondary": str|null}.
        importance: Initial importance score (0.0 default).
        keystone: If true, memory is immune to decay.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    _restore_if_empty()

    if channel not in CHANNELS:
        return f"error: unknown channel '{channel}'. Use: {', '.join(CHANNELS)}"

    shivvr = _get_shivvr()
    if not shivvr.available():
        return f"error: shivvr not reachable at {shivvr.base_url}"

    profile = CHANNELS[channel]
    vector = shivvr.embed(text)

    if _use_http(target):
        http = _get_http(target)
        # HTTP mode: POST to /remember with full JSON
        mid = int(time.time() * 1000) % (2**31)
        row = {
            "id": mid,
            "tags": {"channel": channel, "text": text[:200]},
            "vector": [float(v) for v in vector],
            "decay_alpha": profile["alpha"],
        }
        if emotion:
            row["emotion"] = emotion
        if importance:
            row["importance"] = importance
        if keystone:
            row["keystone"] = True
        result = http.post("remember", json.dumps(row))
        if not _replaying:
            _get_journal().append("remember", text=text, channel=channel,
                                  emotion=emotion, importance=importance, keystone=keystone)
        return f"remembered id={mid} channel={channel} alpha={profile['alpha']} | {result}"

    # Subprocess mode
    mid = _get_ids().next()

    row = {
        "id": mid,
        "tags": {
            "channel": channel,
            "text": text[:200],
        },
        "vector": vector,
        "decay_alpha": profile["alpha"],
    }
    if emotion:
        row["emotion"] = emotion
    if importance:
        row["importance"] = importance
    if keystone:
        row["keystone"] = True

    result = _get_repl().send(f"remember {json.dumps(row)}")
    if not _replaying:
        _get_journal().append("remember", text=text, channel=channel,
                              emotion=emotion, importance=importance, keystone=keystone)
    return f"remembered id={mid} channel={channel} alpha={profile['alpha']} | {result}"


@_tool("recall")
def ferricula_recall(query: str, target: Optional[str] = None) -> str:
    """Search memories by text. Returns reconstructed text, never vectors.

    Accepts freeform text queries. Each recalled memory gets strengthened.

    Args:
        query: Search text or SQL query.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    _restore_if_empty()

    if _use_http(target):
        http = _get_http(target)
        shivvr = _get_shivvr()
        # Send query text — ferricula handles embedding internally via embed() SQL function.
        # If it's already SQL, pass through. Otherwise the planner wraps it in embed().
        return http.post("recall", json.dumps({"query": query}))

    # REPL path: send query directly — ferricula planner wraps freeform text
    # in embed() and ferricula resolves embeddings internally via shivvr.
    raw = _get_repl().send(f"recall {query}")
    return raw


@_tool("inspect")
def ferricula_inspect(id: int, target: Optional[str] = None) -> str:
    """Inspect a memory — shows reconstructed text, fidelity, emotion, graph.

    Never exposes raw vectors. Shows the text tag and thermodynamic state.

    Args:
        id: Memory ID to inspect.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    _restore_if_empty()

    if _use_http(target):
        return _get_http(target).get(f"inspect/{id}")

    repl = _get_repl()
    inspect_resp = repl.send(f"inspect {id}")
    row_resp = repl.send(f"get {id}")

    # Enrich with text from tag
    text_line = ""
    try:
        row_data = json.loads(row_resp)
        text_tag = row_data.get("tags", {}).get("text", "")
        channel = row_data.get("tags", {}).get("channel", "")
        if text_tag:
            text_line = f"\n  text: {text_tag}"
        if channel:
            text_line += f"\n  channel: {channel}"
    except (json.JSONDecodeError, KeyError):
        pass

    return inspect_resp + text_line


@_tool("observe")
def ferricula_observe(path: str, summary: Optional[str] = None, target: Optional[str] = None) -> str:
    """Observe a file — creates a keystone reference node in the knowledge graph.

    Uses the "seeing" channel. File observations are always keystoned
    (immune to decay) since they represent reference material.

    Args:
        path: File path to observe.
        summary: Optional description of the file. Uses filename if omitted.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    _restore_if_empty()

    shivvr = _get_shivvr()
    if not shivvr.available():
        return f"error: shivvr not reachable at {shivvr.base_url}"

    text = summary if summary else Path(path).name
    mid = _get_ids().next()
    vector = shivvr.embed(text)

    profile = CHANNELS["seeing"]
    row = {
        "id": mid,
        "tags": {
            "channel": "seeing",
            "type": "file",
            "path": path,
            "text": text[:200],
        },
        "vector": vector,
        "decay_alpha": profile["alpha"],
        "keystone": True,
    }

    result = _get_repl().send(f"remember {json.dumps(row)}")
    if not _replaying:
        _get_journal().append("observe", path=path, summary=summary)
    return f"observed id={mid} path={path} keystone=true | {result}"


@_tool("reflect")
def ferricula_reflect(thought: str, importance: float = 0.0, target: Optional[str] = None) -> str:
    """Record a thought — working memory with faster decay.

    Uses the "thinking" channel (alpha=0.015). Thoughts decay faster
    than external input unless keystoned or reinforced by recall.

    Args:
        thought: The thought or internal reflection to record.
        importance: Initial importance score (0.0 default).
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    _restore_if_empty()

    shivvr = _get_shivvr()
    if not shivvr.available():
        return f"error: shivvr not reachable at {shivvr.base_url}"

    mid = _get_ids().next()
    vector = shivvr.embed(thought)

    profile = CHANNELS["thinking"]
    row = {
        "id": mid,
        "tags": {
            "channel": "thinking",
            "text": thought[:200],
        },
        "vector": vector,
        "decay_alpha": profile["alpha"],
    }
    if importance:
        row["importance"] = importance

    result = _get_repl().send(f"remember {json.dumps(row)}")
    if not _replaying:
        _get_journal().append("reflect", text=thought, importance=importance)
    return f"reflected id={mid} channel=thinking alpha={profile['alpha']} | {result}"


@_tool("health")
def ferricula_health(target: Optional[str] = None) -> str:
    """Check health of ferricula and shivvr (embedding service).

    Args:
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    parts = []

    # Check ferricula
    try:
        if _use_http(target):
            status = _get_http(target).get("status")
            parts.append(f"ferricula: ok (http)\n  {status}")
        else:
            status = _get_repl().send("status")
            parts.append(f"ferricula: ok\n  {status}")
    except Exception as e:
        parts.append(f"ferricula: error ({e})")

    # Check shivvr
    try:
        shivvr = _get_shivvr()
        health = shivvr.health()
        parts.append(f"shivvr: ok\n  {json.dumps(health)}")
    except Exception as e:
        parts.append(f"shivvr: unreachable ({e})")

    return "\n".join(parts)


# ── Retained Tools (pass-through to REPL or HTTP) ────────────────────────


@_tool("dream")
def ferricula_dream(target: Optional[str] = None) -> str:
    """Run a dream cycle: decay, forgive, consolidate, neglect, review, prune.

    Dying memories with neighbors get a ghost echo: their vector is inverted
    to text via vec2text, re-embedded, and if fidelity >= 0.5 the echo is
    saved as labeled edges on surviving neighbors (requires shivvr).

    Args:
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).post("dream")
    return _get_repl().send("dream")


@_tool("status")
def ferricula_status(target: Optional[str] = None) -> str:
    """Get memory system status: counts of active/forgiven/archived memories,
    graph nodes/edges, prime tree terms.

    Args:
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).get("status")
    return _get_repl().send("status")


@_tool("keystone")
def ferricula_keystone(id: int, target: Optional[str] = None) -> str:
    """Toggle keystone status on a memory. Keystones are immune to decay.

    Args:
        id: Memory ID to toggle.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).post(f"keystone/{id}")
    return _get_repl().send(f"keystone {id}")


@_tool("connect")
def ferricula_connect(a: int, b: int, label: str = "related", kind: str = "semantic", target: Optional[str] = None) -> str:
    """Create a graph edge between two memories.

    Args:
        a: Source memory ID (cause, for causal edges).
        b: Target memory ID (effect, for causal edges).
        label: Edge label (e.g. "caused", "related", "contradicts").
        kind: Edge directionality: "semantic" (bidirectional) or "causal" (directed a->b only).
              Causal edges enforce the arrow of time — b cannot traverse back to a.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).post("connect", json.dumps({"a": a, "b": b, "label": label, "kind": kind}))
    return _get_repl().send(f"connect {a} {b} {label} {kind}")


@_tool("disconnect")
def ferricula_disconnect(a: int, b: int, target: Optional[str] = None) -> str:
    """Remove the graph edge between two memories.

    Args:
        a: First memory ID.
        b: Second memory ID.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).post("disconnect", json.dumps({"a": a, "b": b}))
    return _get_repl().send(f"disconnect {a} {b}")


@_tool("neighbors")
def ferricula_neighbors(id: int, target: Optional[str] = None) -> str:
    """Get all graph neighbors of a memory with edge labels and fidelity.

    Args:
        id: Memory ID.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).get(f"neighbors/{id}")
    return _get_repl().send(f"neighbors {id}")


@_tool("terms")
def ferricula_terms(target: Optional[str] = None) -> str:
    """List all terms in the prime tree with member counts.

    Args:
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).get("terms")
    return _get_repl().send("terms")


@_tool("query")
def ferricula_query(sql: str, target: Optional[str] = None) -> str:
    """Run a raw SQL query against the store. Does not update recall stats.

    Args:
        sql: SQL query string.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).post("query", json.dumps({"query": sql}))
    return _get_repl().send(f"query {sql}")


@_tool("checkpoint")
def ferricula_checkpoint(target: Optional[str] = None) -> str:
    """Flush current state to V2 snapshot and clear WAL.

    Args:
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).post("checkpoint")
    return _get_repl().send("checkpoint")


# ── Identity & Inversion Tools ──────────────────────────────────────────


@_tool("identity")
def ferricula_identity(target: Optional[str] = None) -> str:
    """Get the agent's identity: hexagram, horoscope, archetypes, emotions.

    Returns the full identity state including cast hexagram number and name,
    zodiac sign, primary/secondary emotions, and all five archetypes.

    Args:
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).get("identity")
    return "error: identity requires HTTP mode (--serve)"


@_tool("inversion_check")
def ferricula_inversion_check(id: int, target: Optional[str] = None) -> str:
    """Check semantic fidelity of a memory via vec2text inversion.

    Inverts the memory's vector back to approximate text via shivvr,
    then compares with the original text tag using Jaccard similarity.
    Returns: original text, inverted text, quality score (0.0..1.0).

    Requires shivvr (gnosis-chunk) to be running.

    Args:
        id: Memory ID to check.
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).get(f"inversion/{id}")
    return "error: inversion check requires HTTP mode (--serve)"


# ── Clock & Entropy Tools ─────────────────────────────────────────────────

RADIO_URL = os.environ.get("RADIO_URL", "http://localhost:9080")


@_tool("clock")
def ferricula_clock(target: Optional[str] = None) -> str:
    """Get clock telemetry: ticks, dreams, entropy stats, radio status.

    Also returns any buffered background [clock] events from the REPL.

    Args:
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if _use_http(target):
        return _get_http(target).get("clock")
    repl = _get_repl()
    status = repl.send("clock")
    events = repl.drain_clock_events()
    if events:
        status += "\nrecent events:\n  " + "\n  ".join(events)
    return status


@_tool("offer_entropy")
def ferricula_offer_entropy(source: str = "radio", target: Optional[str] = None) -> str:
    """Inject entropy into ferricula to trigger a dream cycle.

    Args:
        source: Either "radio" to fetch from gnosis-radio, or a hex string
                to inject directly (e.g. "deadbeef0123").
        target: Character name or port to route to (e.g. "assis" or "8780").
    """
    if source == "radio":
        # Fetch entropy from gnosis-radio
        try:
            url = f"{RADIO_URL}/api/entropy?bytes=64&format=json"
            req = urllib.request.Request(url)
            with urllib.request.urlopen(req, timeout=5) as resp:
                data = json.loads(resp.read().decode("utf-8"))
            hex_str = data.get("entropy_hex", "")
            if not hex_str:
                return "error: radio returned no entropy (pool empty?)"
        except Exception as e:
            return f"error: could not fetch entropy from radio ({e})"
    else:
        hex_str = source.strip()

    if _use_http(target):
        return _get_http(target).post_raw("offer", hex_str)
    result = _get_repl().send(f"offer {hex_str}")
    return result


# ── Embodiment ───────────────────────────────────────────────────────────


@mcp.tool()
def ferricula_embody(target: Optional[str] = None, memories: int = 12) -> str:
    """Embody a ferricula character — load identity and available mental state.

    Returns everything an LLM needs to inhabit this character: identity,
    current state, and any available memory/dream context.
    Use this at the start of a conversation to become the character.

    Args:
        target: Character name or port (e.g. "steve jobs" or "8773").
        memories: Number of keystone memories to include (default: 12).
    """
    if not _use_http(target):
        return "error: embody requires HTTP mode (a running ferricula instance)"

    http = _get_http(target)
    sections = []

    # ── Identity ──
    try:
        raw = http.get("identity")
        identity = json.loads(raw)
        name = identity.get("name", "unknown")
        hex_info = identity.get("hexagram", {})
        hex_name = hex_info.get("name", "")
        hex_num = hex_info.get("number", "")
        zodiac = identity.get("horoscope", {}).get("sign_name", "")
        primary_emo = identity.get("primary_emotion", "")
        secondary_emo = identity.get("secondary_emotion", "")

        id_lines = [f"# {name}"]
        if hex_name:
            id_lines.append(f"Hexagram #{hex_num} {hex_name} | {zodiac}")
        if primary_emo:
            emo = f"{primary_emo}/{secondary_emo}" if secondary_emo else primary_emo
            id_lines.append(f"Emotional baseline: {emo}")
        sections.append("\n".join(id_lines))
    except Exception as e:
        sections.append(f"identity: error ({e})")
        name = "unknown"

    # ── Status ──
    try:
        raw = http.get("status")
        data = json.loads(raw)
        status_text = data.get("result", raw)
        # Extract key numbers
        active_m = re.search(r"active=(\d+)", status_text)
        keystones_m = re.search(r"keystones=(\d+)", status_text)
        edges_m = re.search(r"(\d+) edges", status_text)
        terms_m = re.search(r"(\d+) terms", status_text)
        stats = []
        if active_m:
            stats.append(f"{active_m.group(1)} active memories")
        if keystones_m:
            stats.append(f"{keystones_m.group(1)} keystones")
        if edges_m:
            stats.append(f"{edges_m.group(1)} graph edges")
        if terms_m:
            stats.append(f"{terms_m.group(1)} terms")
        if stats:
            sections.append("## State\n" + ", ".join(stats))
    except Exception:
        pass

    # ── Core memories ──
    # /search is not available on ferricula HTTP instances; leave this section empty.
    try:
        core_memories = ""
        if core_memories:
            sections.append(core_memories)
    except Exception:
        pass

    # ── Latest dream ──
    # /dream/latest is not available on ferricula HTTP instances; leave this section empty.
    try:
        latest_dream = ""
        if latest_dream:
            sections.append(latest_dream)
    except Exception:
        pass

    return "\n\n".join(sections)


# ── Multi-Instance Tools ─────────────────────────────────────────────────

SCAN_PORTS = [
    8765, 8764, 8773, 8774, 8775, 8776, 8780,
    8781, 8782,
    8790, 8791, 8792, 8793, 8794, 8795, 8796, 8797,
]


@mcp.tool()
def ferricula_discover(ports: Optional[str] = None) -> str:
    """Scan ports for running ferricula instances, register them by name.

    Calls /identity on each port to discover who's there.

    Args:
        ports: Comma-separated ports to scan (default: SCAN_PORTS).
    """
    if ports:
        scan = [int(p.strip()) for p in ports.split(",") if p.strip().isdigit()]
    else:
        scan = SCAN_PORTS

    found = []
    for port in scan:
        url = f"http://localhost:{port}"
        client = HttpClient(url)
        try:
            raw = client.get("identity")
            data = json.loads(raw)
            name = data.get("name", "").strip()
            agent_id = data.get("agent_id", "")
            if name:
                _register_instance(url, name.lower())
                found.append(f"  {name} @ :{port} (agent={agent_id})")
            else:
                found.append(f"  unknown @ :{port} (no name in identity)")
        except Exception:
            continue  # port not responding, skip

    if not found:
        return "no ferricula instances found on scanned ports"
    return f"discovered {len(found)} instances:\n" + "\n".join(found)


@mcp.tool()
def ferricula_list_characters() -> str:
    """List all registered ferricula character instances.

    Shows characters that have been discovered or manually registered.
    Use the target parameter on any tool to talk to a specific character.
    """
    if not _registry:
        return "no characters registered. Run discover first, or use --port/--http-url."
    lines = []
    for name, client in _registry.items():
        lines.append(f"  {name} @ {client.base_url}")
    return f"{len(lines)} registered characters:\n" + "\n".join(lines)


if __name__ == "__main__":
    mcp.run()
