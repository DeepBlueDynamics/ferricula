"""ferricula-code: Index a codebase into ferricula as keystones.

Walks a directory, summarizes each significant file, and stores
each as a keystone memory in ferricula. Enables semantic code
search across all indexed projects via ferricula_recall.

The codebase becomes part of the agent's permanent memory.
You don't search code — you remember it.

Environment:
    FERRICULA_URL: ferricula HTTP instance (default http://localhost:8765)
    CHONK_URL: embedding service (default http://nemesis:8080)
"""

import os
import sys
import json
import time
import urllib.request
import urllib.error
from pathlib import Path
from typing import Optional

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("ferricula-code")

FERRICULA_URL = os.environ.get("FERRICULA_URL", "http://localhost:8765")
CHONK_URL = os.environ.get("CHONK_URL", "http://nemesis:8080")

# File extensions worth indexing
CODE_EXTENSIONS = {
    ".py", ".rs", ".js", ".ts", ".tsx", ".jsx", ".go", ".java", ".c", ".cpp",
    ".h", ".hpp", ".rb", ".php", ".swift", ".kt", ".scala", ".sh", ".bash",
    ".toml", ".yaml", ".yml", ".json", ".sql", ".html", ".css", ".scss",
    ".vue", ".svelte", ".lua", ".zig", ".nim", ".ex", ".exs", ".erl",
    ".md", ".txt", ".dockerfile",
}

# Directories to skip
SKIP_DIRS = {
    ".git", ".hg", "node_modules", "__pycache__", ".venv", "venv",
    "target", "build", "dist", ".next", ".nuxt", "vendor", ".cache",
    ".tox", "eggs", ".mypy_cache", ".pytest_cache", ".claude",
}

MAX_LINES = 80
MAX_FILES = 500


# ── HTTP helpers ────────────────────────────────────────────────────────

def _post(url: str, body: dict, timeout: int = 15) -> dict:
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def _get(url: str, timeout: int = 15) -> dict:
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def _embed(text: str) -> list[float]:
    """Embed text via chonk, return dense vector."""
    result = _post(f"{CHONK_URL}/memory/_mcp/ingest", {"text": text}, timeout=30)
    if "embedding" in result:
        return result["embedding"]
    return result["chunks"][0]["embedding"]


def _remember(text: str, keystone: bool = True) -> dict:
    """Embed text via chonk and store as a keystone memory in ferricula."""
    vector = _embed(text[:500])  # cap text for embedding
    mid = int(time.time() * 1000) % (2**31)
    row = {
        "id": mid,
        "tags": {
            "channel": "seeing",
            "type": "code",
            "text": text[:200],
        },
        "vector": [float(v) for v in vector],
        "decay_alpha": 0.010,
        "importance": 0.8,
    }
    if keystone:
        row["keystone"] = True
    data = json.dumps(row).encode()
    req = urllib.request.Request(
        f"{FERRICULA_URL}/remember",
        data=data,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode())


# ── File summarization ──────────────────────────────────────────────────

def summarize_file(path: Path, project: str) -> Optional[str]:
    """Create a summary of a file for indexing."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except Exception:
        return None

    lines = text.splitlines()
    if not lines:
        return None

    rel_path = str(path)
    ext = path.suffix.lower()

    imports = []
    functions = []
    classes = []
    key_lines = []

    for i, line in enumerate(lines[:MAX_LINES]):
        stripped = line.strip()

        # Python
        if ext == ".py":
            if stripped.startswith("import ") or stripped.startswith("from "):
                imports.append(stripped)
            elif stripped.startswith("def "):
                functions.append(stripped.split("(")[0].replace("def ", ""))
            elif stripped.startswith("class "):
                classes.append(stripped.split("(")[0].split(":")[0].replace("class ", ""))
            elif stripped.startswith('"""') or stripped.startswith("'''"):
                key_lines.append(stripped)

        # Rust
        elif ext == ".rs":
            if stripped.startswith("use "):
                imports.append(stripped)
            elif stripped.startswith("pub fn ") or stripped.startswith("fn "):
                sig = stripped.split("{")[0].strip()
                functions.append(sig)
            elif stripped.startswith(("pub struct ", "struct ", "pub enum ", "enum ")):
                classes.append(stripped.split("{")[0].strip())
            elif stripped.startswith("impl "):
                classes.append(stripped.split("{")[0].strip())

        # JavaScript/TypeScript
        elif ext in (".js", ".ts", ".tsx", ".jsx"):
            if stripped.startswith("import "):
                imports.append(stripped)
            elif "function " in stripped or stripped.startswith("export "):
                functions.append(stripped.split("{")[0].strip()[:100])
            elif stripped.startswith("class "):
                classes.append(stripped.split("{")[0].strip())

        # Go
        elif ext == ".go":
            if stripped.startswith("import"):
                imports.append(stripped)
            elif stripped.startswith("func "):
                functions.append(stripped.split("{")[0].strip())
            elif stripped.startswith("type ") and "struct" in stripped:
                classes.append(stripped.split("{")[0].strip())

        # First meaningful comments
        if i < 5 and (stripped.startswith("//") or stripped.startswith("#") or stripped.startswith("/*")):
            key_lines.append(stripped)

    # Build summary
    parts = [f"[project:{project}] {rel_path}"]
    parts.append(f"  {len(lines)} lines, {ext}")

    if key_lines:
        parts.append(f"  {key_lines[0][:120]}")
    if imports:
        parts.append(f"  imports: {', '.join(imports[:8])}")
    if classes:
        parts.append(f"  types: {', '.join(classes[:10])}")
    if functions:
        parts.append(f"  functions: {', '.join(functions[:15])}")

    content_lines = [
        l.strip() for l in lines[:30]
        if l.strip() and not l.strip().startswith(("import ", "from ", "use ", "#!", "//!", "/*"))
    ][:5]
    if content_lines:
        parts.append(f"  context: {' | '.join(content_lines)}")

    return "\n".join(parts)


def walk_project(directory: str) -> list[Path]:
    """Walk a project directory and return indexable files."""
    root = Path(directory)
    files = []
    for path in root.rglob("*"):
        if any(skip in path.parts for skip in SKIP_DIRS):
            continue
        if path.is_file() and path.suffix.lower() in CODE_EXTENSIONS:
            files.append(path)
        if len(files) >= MAX_FILES:
            break
    return sorted(files)


# ── MCP Tools ───────────────────────────────────────────────────────────

@mcp.tool()
def index_project(directory: str, project: str = "") -> str:
    """Index a codebase into ferricula as keystone memories.

    Walks the directory, summarizes each significant file, and stores
    each as a permanent keystone in ferricula. The codebase becomes
    part of the agent's permanent memory — searchable via recall.

    Args:
        directory: Path to the project directory to index.
        project: Project name for tagging (defaults to directory name).
    """
    root = Path(directory)
    if not root.is_dir():
        return json.dumps({"error": f"directory not found: {directory}"})

    if not project:
        project = root.name

    files = walk_project(directory)
    indexed = 0
    errors = 0
    error_msgs = []

    for path in files:
        summary = summarize_file(path, project)
        if not summary:
            continue
        try:
            _remember(summary, keystone=True)
            indexed += 1
        except Exception as e:
            errors += 1
            if len(error_msgs) < 3:
                error_msgs.append(str(e)[:80])

    result = {
        "project": project,
        "files_found": len(files),
        "indexed": indexed,
        "errors": errors,
        "search_hint": f'Use ferricula_recall("project:{project} <query>") to search',
    }
    if error_msgs:
        result["sample_errors"] = error_msgs
    return json.dumps(result)


@mcp.tool()
def search_code(query: str, project: str = "") -> str:
    """Search indexed code via ferricula memory.

    Searches across all codebases stored in ferricula. Results are
    ranked by semantic similarity — describe what you need, not
    exact filenames.

    Args:
        query: What to search for (semantic — describe what you need).
        project: Optional project filter (prefix match on [project:name]).
    """
    search_query = f"project:{project} {query}" if project else query
    body = json.dumps({"query": search_query}).encode()
    req = urllib.request.Request(
        f"{FERRICULA_URL}/search",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            data = json.loads(resp.read().decode())
        results = data.get("results", [])
        lines = []
        for hit in results[:10]:
            text = hit.get("text", "")
            score = hit.get("bm25", 0)
            lines.append(f"  [{score:.1f}] {text}")
        if lines:
            return f"found {len(results)} matches:\n" + "\n".join(lines)
        return "no matches"
    except Exception as e:
        return f"error: {e}"


@mcp.tool()
def index_file(file_path: str, project: str = "") -> str:
    """Index a single file into ferricula as a keystone memory.

    Args:
        file_path: Path to the file to index.
        project: Project name for tagging (defaults to parent directory).
    """
    path = Path(file_path)
    if not path.is_file():
        return json.dumps({"error": f"file not found: {file_path}"})

    if not project:
        project = path.parent.name

    summary = summarize_file(path, project)
    if not summary:
        return json.dumps({"error": "could not summarize file (empty or unreadable)"})

    try:
        result = _remember(summary, keystone=True)
        return json.dumps({"file": str(path), "project": project, "result": result})
    except Exception as e:
        return json.dumps({"error": str(e)})


if __name__ == "__main__":
    mcp.run()
