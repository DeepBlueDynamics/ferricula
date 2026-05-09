"""HTTP clients for ferricula and shivvr (gnosis-chunk / shivvr v0.2).

Reuses patterns from tools/ferricula-mcp.py — stdlib-only HTTP,
regex-based response parsing, no third-party dependencies.
"""

import json
import re
import urllib.request
import urllib.error
from dataclasses import dataclass, field
from typing import Optional


# ── ShivvrClient ─────────────────────────────────────────────────────────

class ShivvrClient:
    """HTTP client for shivvr v0.2 embedding service.

    Uses the session-based API: POST /temp/:session/ingest
    Returns {"chunks": [{"text": ..., "embedding": [...768 floats...]}], ...}
    """

    def __init__(self, base_url: str = "https://shivvr.nuts.services",
                 session: str = "ferricula"):
        self.base_url = base_url.rstrip("/")
        self.session = session

    def embed(self, text: str) -> list[float]:
        """Embed text via shivvr, return first chunk's vector."""
        data = json.dumps({"text": text}).encode("utf-8")
        req = urllib.request.Request(
            f"{self.base_url}/temp/{self.session}/ingest",
            data=data,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            result = json.loads(resp.read().decode("utf-8"))
        return result["chunks"][0]["embedding"]

    def chunk_and_embed(self, text: str, timeout: int = 600) -> list[dict]:
        """Send text to shivvr's chunker. Returns list of {text, embedding}.

        Shivvr splits the text into semantic chunks and embeds each one.
        Each returned dict has 'text' (str) and 'embedding' (list[float]).
        HTTP runs in a daemon thread so Ctrl-C kills the process on Windows.
        """
        import threading
        result_box = [None, None]  # [result, error]

        def _do_request():
            try:
                data = json.dumps({"text": text}).encode("utf-8")
                req = urllib.request.Request(
                    f"{self.base_url}/temp/{self.session}/ingest",
                    data=data,
                    headers={"Content-Type": "application/json"},
                )
                with urllib.request.urlopen(req, timeout=timeout) as resp:
                    result_box[0] = json.loads(resp.read().decode("utf-8"))
            except Exception as e:
                result_box[1] = e

        t = threading.Thread(target=_do_request, daemon=True)
        t.start()
        # join in short bursts — KeyboardInterrupt fires between joins
        remaining = timeout
        while t.is_alive() and remaining > 0:
            t.join(timeout=0.5)
            remaining -= 0.5

        if result_box[1] is not None:
            raise result_box[1]
        if result_box[0] is None:
            raise TimeoutError(f"shivvr did not respond in {timeout}s")

        chunks = result_box[0].get("chunks", [])
        return [{"text": c["text"], "embedding": c["embedding"]} for c in chunks]

    def available(self) -> bool:
        try:
            req = urllib.request.Request(f"{self.base_url}/health")
            with urllib.request.urlopen(req, timeout=10) as resp:
                resp.read()
            return True
        except Exception:
            return False


# ── Response Parsing ─────────────────────────────────────────────────────

@dataclass
class DreamReport:
    ticks: int = 0
    decayed: int = 0
    forgiven: int = 0
    archived: int = 0
    consolidated: int = 0
    pruned: int = 0
    ghost_echoes: int = 0
    keystones_reviewed: int = 0
    edges_created: int = 0
    keystones_promoted: int = 0
    active_archetypes: list[str] = field(default_factory=list)


@dataclass
class RecallHit:
    id: int = 0
    fidelity: float = 1.0
    recalls: int = 0


@dataclass
class InspectResult:
    id: int = 0
    state: str = "Active"
    fidelity: float = 1.0
    decay_alpha: float = 0.01
    effective_alpha: float = 0.01
    keystone: bool = False
    recalls: int = 0
    consolidation_depth: int = 0
    importance: float = 0.0
    emotion: str = "-"
    degree: int = 0


@dataclass
class StatusResult:
    rows: int = 0
    memories: int = 0
    active: int = 0
    forgiven: int = 0
    archived: int = 0
    keystones: int = 0
    graph_nodes: int = 0
    graph_edges: int = 0


def _unwrap(resp_text: str) -> str:
    """Unwrap {"result": "..."} response, return inner string."""
    try:
        data = json.loads(resp_text)
        if "result" in data:
            return data["result"]
        if "error" in data:
            return f"error: {data['error']}"
        return resp_text
    except (json.JSONDecodeError, TypeError):
        return resp_text


def parse_dream_report(text: str) -> DreamReport:
    """Parse dream report from ferricula response text."""
    inner = _unwrap(text)
    report = DreamReport()
    for key in ("ticks", "decayed", "forgiven", "archived",
                "consolidated", "pruned", "ghost_echoes", "keystones_reviewed",
                "edges_created", "keystones_promoted"):
        m = re.search(rf"{key}=(\d+)", inner)
        if m:
            setattr(report, key, int(m.group(1)))
    # Parse active_archetypes=[Intuition,Ethics,...]
    m = re.search(r"active_archetypes=\[([^\]]*)\]", inner)
    if m and m.group(1):
        report.active_archetypes = [s.strip() for s in m.group(1).split(",") if s.strip()]
    return report


def parse_recall(text: str) -> list[RecallHit]:
    """Parse recall results into list of RecallHit."""
    inner = _unwrap(text)
    hits = []
    for m in re.finditer(r"id=(\d+)\s+fidelity=([\d.]+)\s+recalls=(\d+)", inner):
        hits.append(RecallHit(
            id=int(m.group(1)),
            fidelity=float(m.group(2)),
            recalls=int(m.group(3)),
        ))
    # Fallback: id-only lines (no fidelity yet)
    if not hits:
        for m in re.finditer(r"id=(\d+)", inner):
            hits.append(RecallHit(id=int(m.group(1))))
    return hits


def parse_inspect(text: str) -> InspectResult:
    """Parse inspect response into InspectResult."""
    inner = _unwrap(text)
    result = InspectResult()
    m = re.search(r"id=(\d+)", inner)
    if m:
        result.id = int(m.group(1))
    m = re.search(r"state=(\w+)", inner)
    if m:
        result.state = m.group(1)
    m = re.search(r"fidelity=([\d.]+)", inner)
    if m:
        result.fidelity = float(m.group(1))
    m = re.search(r"decay_alpha=([\d.]+)", inner)
    if m:
        result.decay_alpha = float(m.group(1))
    m = re.search(r"effective=([\d.]+)", inner)
    if m:
        result.effective_alpha = float(m.group(1))
    m = re.search(r"keystone=(true|false)", inner, re.IGNORECASE)
    if m:
        result.keystone = m.group(1).lower() == "true"
    m = re.search(r"recalls=(\d+)", inner)
    if m:
        result.recalls = int(m.group(1))
    m = re.search(r"consolidation_depth=(\d+)", inner)
    if m:
        result.consolidation_depth = int(m.group(1))
    m = re.search(r"importance=([\d.]+)", inner)
    if m:
        result.importance = float(m.group(1))
    m = re.search(r"emotion=(\S+)", inner)
    if m:
        result.emotion = m.group(1)
    m = re.search(r"degree=(\d+)", inner)
    if m:
        result.degree = int(m.group(1))
    return result


def parse_status(text: str) -> StatusResult:
    """Parse status response into StatusResult."""
    inner = _unwrap(text)
    result = StatusResult()
    m = re.search(r"rows=(\d+)", inner)
    if m:
        result.rows = int(m.group(1))
    m = re.search(r"memories=(\d+)", inner)
    if m:
        result.memories = int(m.group(1))
    m = re.search(r"active=(\d+)", inner)
    if m:
        result.active = int(m.group(1))
    m = re.search(r"forgiven=(\d+)", inner)
    if m:
        result.forgiven = int(m.group(1))
    m = re.search(r"archived=(\d+)", inner)
    if m:
        result.archived = int(m.group(1))
    m = re.search(r"keystones=(\d+)", inner)
    if m:
        result.keystones = int(m.group(1))
    m = re.search(r"(\d+) nodes", inner)
    if m:
        result.graph_nodes = int(m.group(1))
    m = re.search(r"(\d+) edges", inner)
    if m:
        result.graph_edges = int(m.group(1))
    return result


def parse_remember_id(text: str) -> Optional[int]:
    """Extract memory ID from remember response."""
    inner = _unwrap(text)
    m = re.search(r"id=(\d+)", inner)
    return int(m.group(1)) if m else None


# ── FerriculaClient ──────────────────────────────────────────────────────

class FerriculaClient:
    """HTTP client for a ferricula instance."""

    def __init__(self, base_url: str, name: str = "agent"):
        self.base_url = base_url.rstrip("/")
        self.name = name
        self._next_id = 1

    def _post(self, endpoint: str, body: str = "{}") -> str:
        url = f"{self.base_url}/{endpoint.lstrip('/')}"
        data = body.encode("utf-8")
        req = urllib.request.Request(
            url, data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                return resp.read().decode("utf-8")
        except urllib.error.HTTPError as e:
            return e.read().decode("utf-8") if e.fp else f'{{"error": "{e}"}}'
        except Exception as e:
            return f'{{"error": "{e}"}}'

    def _get(self, endpoint: str) -> str:
        url = f"{self.base_url}/{endpoint.lstrip('/')}"
        req = urllib.request.Request(url)
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                return resp.read().decode("utf-8")
        except urllib.error.HTTPError as e:
            return e.read().decode("utf-8") if e.fp else f'{{"error": "{e}"}}'
        except Exception as e:
            return f'{{"error": "{e}"}}'

    def available(self) -> bool:
        try:
            resp = self._get("status")
            return "error" not in resp.lower()[:50]
        except Exception:
            return False

    def alloc_id(self) -> int:
        mid = self._next_id
        self._next_id += 1
        return mid

    def remember(self, text: str, vector: list[float], *,
                 channel: str = "hearing", decay_alpha: float = 0.01,
                 emotion: Optional[dict] = None, importance: float = 0.0,
                 keystone: bool = False,
                 character: Optional[str] = None) -> int:
        """Ingest a memory, return assigned ID."""
        mid = self.alloc_id()
        tags = {"channel": channel, "text": text[:200]}
        if character:
            tags["character"] = character
        row = {
            "id": mid,
            "tags": tags,
            "vector": [float(v) for v in vector],
            "decay_alpha": decay_alpha,
        }
        if emotion:
            row["emotion"] = emotion
        if importance:
            row["importance"] = importance
        if keystone:
            row["keystone"] = True
        self._post("remember", json.dumps(row))
        return mid

    def recall_vector(self, vector: list[float], k: int = 10,
                      character: Optional[str] = None) -> list[RecallHit]:
        """Recall by vector similarity, optionally filtered by character tag."""
        vec_str = "[" + ",".join(str(v) for v in vector) + "]"
        if character:
            sql = (f"SELECT id FROM docs WHERE tag('character') = '{character}' "
                   f"AND vector_topk_cosine('{vec_str}', {k})")
        else:
            sql = f"SELECT id FROM docs WHERE vector_topk_cosine('{vec_str}', {k})"
        resp = self._post("recall", json.dumps({"query": sql}))
        return parse_recall(resp)

    def recall_text(self, text: str, shivvr: ShivvrClient, k: int = 5,
                    character: Optional[str] = None) -> list[RecallHit]:
        """Embed text via shivvr, then recall by vector similarity."""
        vector = shivvr.embed(text)
        return self.recall_vector(vector, k, character=character)

    def dream(self) -> DreamReport:
        resp = self._post("dream")
        return parse_dream_report(resp)

    def inspect(self, mid: int) -> InspectResult:
        resp = self._get(f"inspect/{mid}")
        return parse_inspect(resp)

    def status(self) -> StatusResult:
        resp = self._get("status")
        return parse_status(resp)

    def keystone(self, mid: int) -> str:
        return _unwrap(self._post(f"keystone/{mid}"))

    def connect(self, a: int, b: int, label: str = "related") -> str:
        return _unwrap(self._post("connect", json.dumps({"a": a, "b": b, "label": label})))

    def offer(self, entropy_hex: str) -> DreamReport:
        """POST hex entropy to /offer, get dream report with archetype activation."""
        resp = self._post("offer", entropy_hex)  # raw hex body, NOT JSON
        return parse_dream_report(resp)

    def identity(self) -> dict:
        """GET /identity, return full identity JSON."""
        resp = self._get("identity")
        try:
            return json.loads(resp)
        except json.JSONDecodeError:
            return {}

    def neighbors(self, mid: int) -> str:
        """GET /neighbors/{id}, return neighbor text."""
        return _unwrap(self._get(f"neighbors/{mid}"))

    def checkpoint(self) -> str:
        return _unwrap(self._post("checkpoint"))

    def get_row(self, mid: int) -> dict:
        """Get raw row JSON (tags, vector_dim — no raw vector)."""
        resp = self._get(f"get/{mid}")
        try:
            return json.loads(resp)
        except json.JSONDecodeError:
            return {"error": resp}
