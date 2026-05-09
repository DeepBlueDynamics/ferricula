#!/usr/bin/env python3
import sys, io
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
"""
steve_map.py — static analysis of steve.py (or any Flask agent file).

Usage:
    python steve_map.py [path/to/file.py]

Sections (in order):
    1.  Flask routes
    2.  Tool function defs / dispatch entries
    3.  SSE broadcast events
    4.  LLM calls / external API calls
    5.  Full function index
    6.  Tool call graph + orphans
    7.  Imports & stdlib usage           (NEW)
    8.  Environment variables            (NEW)
    9.  Module-level state (globals)     (NEW)
    10. Locks & events                   (NEW)
    11. Threads (long-lived + spawned)   (NEW)
    12. Background loops                 (NEW)
    13. Loop density per function        (NEW)
    14. UI → backend fetch() map         (NEW)
    15. UI event handlers                (NEW)
    16. JSON request/response shapes     (NEW)
    17. External HTTP client sites       (NEW)
    18. File I/O sites                   (NEW)
    19. Rust port hints (synthesized)    (NEW)
"""

import ast
import re
import sys
import collections
from pathlib import Path

TARGET = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent / "steve.py"
src = TARGET.read_text(encoding="utf-8")
tree = ast.parse(src, filename=str(TARGET))
lines = src.splitlines()

# ── helpers ───────────────────────────────────────────────────────────────────

def call_name(node):
    """Best-effort dotted name for a Call's func."""
    if isinstance(node.func, ast.Name):
        return node.func.id
    if isinstance(node.func, ast.Attribute):
        parts = []
        n = node.func
        while isinstance(n, ast.Attribute):
            parts.append(n.attr)
            n = n.value
        if isinstance(n, ast.Name):
            parts.append(n.id)
        return ".".join(reversed(parts))
    return None

def first_str_arg(call_node):
    if call_node.args:
        a = call_node.args[0]
        if isinstance(a, ast.Constant) and isinstance(a.value, str):
            return a.value
    return None

def argnames(args):
    names = [a.arg for a in args.args]
    if args.vararg:   names.append(f"*{args.vararg.arg}")
    if args.kwarg:    names.append(f"**{args.kwarg.arg}")
    return names

def line_text(lno):
    if isinstance(lno, int) and 1 <= lno <= len(lines):
        return lines[lno - 1].strip()
    return ""

# ── visitor ───────────────────────────────────────────────────────────────────

class Analyzer(ast.NodeVisitor):
    def __init__(self):
        self.functions      = []   # {name, line, args, decorators}
        self.routes         = []   # {path, methods, func, line}
        self.tool_funcs     = []   # (name, line)
        self.dispatch_entries = [] # (tool_name, line)
        self.broadcasts     = []   # (event, line)
        self.llm_calls      = []   # (label_hint, line)
        self.external_calls = []   # (call_str, line)
        self.all_calls      = []   # (caller_func, callee, line)

        # NEW collectors
        self.imports        = []   # (kind, module, name, line)  kind: "import" | "from"
        self.env_vars       = []   # (var_name, default, line)
        self.module_globals = []   # (name, lineno, has_lock_companion)
        self.locks          = []   # (var_name, kind, line)   kind: Lock|RLock|Event|Semaphore|Condition
        self.threads        = []   # (target_name, daemon, line, in_func)
        self.bg_loops       = []   # (cond, line, in_func) — `while not X.is_set():` style
        self.func_loop_counts = {} # func_name -> {"for":int,"while":int}
        self.json_in        = []   # (line, in_func)         — request.get_json sites
        self.json_out_keys  = []   # (line, in_func, keys[]) — jsonify({...}) literal keys
        self.http_clients   = []   # (line, in_func, target)
        self.file_io        = []   # (line, in_func, mode_hint)
        self._stack         = []   # function name stack
        self._in_module     = True # True while at module-level

    @property
    def _current(self):
        return self._stack[-1] if self._stack else None

    # ── functions / methods ──
    def visit_FunctionDef(self, node):
        decorators = []
        route_path, route_methods = None, ["GET"]
        for d in node.decorator_list:
            dname = (call_name(d) if isinstance(d, ast.Call)
                     else (d.id if isinstance(d, ast.Name) else repr(d)))
            decorators.append(dname)
            if isinstance(d, ast.Call) and call_name(d) in ("app.route", "route"):
                route_path = first_str_arg(d)
                for kw in d.keywords:
                    if kw.arg == "methods" and isinstance(kw.value, ast.List):
                        route_methods = [e.value for e in kw.value.elts
                                         if isinstance(e, ast.Constant)]

        self.functions.append({
            "name": node.name, "line": node.lineno,
            "args": argnames(node.args), "decorators": decorators,
        })
        if node.name.startswith("tool_"):
            self.tool_funcs.append((node.name, node.lineno))
        if route_path is not None:
            self.routes.append({"path": route_path, "methods": route_methods,
                                 "func": node.name, "line": node.lineno})

        self._stack.append(node.name)
        self.func_loop_counts.setdefault(node.name, {"for": 0, "while": 0})
        was_module = self._in_module
        self._in_module = False
        self.generic_visit(node)
        self._in_module = was_module
        self._stack.pop()

    visit_AsyncFunctionDef = visit_FunctionDef

    # ── imports ──
    def visit_Import(self, node):
        for alias in node.names:
            self.imports.append(("import", alias.name, alias.asname, node.lineno))
        self.generic_visit(node)

    def visit_ImportFrom(self, node):
        mod = node.module or ""
        for alias in node.names:
            self.imports.append(("from", mod, alias.name, node.lineno))
        self.generic_visit(node)

    # ── module-level assignments (globals) ──
    def visit_Assign(self, node):
        if self._in_module:
            for tgt in node.targets:
                if isinstance(tgt, ast.Name):
                    self.module_globals.append((tgt.id, node.lineno))
                elif isinstance(tgt, ast.Tuple):
                    for elt in tgt.elts:
                        if isinstance(elt, ast.Name):
                            self.module_globals.append((elt.id, node.lineno))
        self.generic_visit(node)

    def visit_AnnAssign(self, node):
        if self._in_module and isinstance(node.target, ast.Name):
            self.module_globals.append((node.target.id, node.lineno))
        self.generic_visit(node)

    # ── for / while loops ──
    def visit_For(self, node):
        if self._current:
            self.func_loop_counts.setdefault(self._current, {"for": 0, "while": 0})
            self.func_loop_counts[self._current]["for"] += 1
        self.generic_visit(node)

    visit_AsyncFor = visit_For

    def visit_While(self, node):
        if self._current:
            self.func_loop_counts.setdefault(self._current, {"for": 0, "while": 0})
            self.func_loop_counts[self._current]["while"] += 1
        # Background-loop pattern: `while not X.is_set():`
        cond = node.test
        if (isinstance(cond, ast.UnaryOp) and isinstance(cond.op, ast.Not)
                and isinstance(cond.operand, ast.Call)):
            name = call_name(cond.operand)
            if name and name.endswith(".is_set"):
                self.bg_loops.append((name, node.lineno, self._current or "<module>"))
        # Also catch `while True:`
        elif isinstance(cond, ast.Constant) and cond.value is True:
            self.bg_loops.append(("True", node.lineno, self._current or "<module>"))
        self.generic_visit(node)

    # ── calls (broadcasts, llm, external, threads, locks, http, file io, env, json) ──
    def visit_Call(self, node):
        name = call_name(node)
        line = getattr(node, "lineno", "?")

        if name:
            self.all_calls.append((self._current, name, line))

            # broadcast(event, ...)
            if name == "broadcast":
                self.broadcasts.append((first_str_arg(node) or "?", line))

            # call_llm / _call_llm
            if name in ("call_llm", "_call_llm"):
                self.llm_calls.append((first_str_arg(node) or "?", line))

            # External HTTP / AI SDK
            EXT = (
                "openai.", "anthropic.", "requests.", "httpx.",
                "urllib.request.", "client.chat", "client.messages",
                "client.images", "genai.", "google.", "cohere.",
                "AsyncOpenAI", "OpenAI", "Anthropic",
            )
            for pfx in EXT:
                if name == pfx.rstrip(".") or name.startswith(pfx):
                    self.external_calls.append((name, line))
                    break

            # Threads
            if name in ("threading.Thread", "Thread"):
                target_name = "?"
                daemon = False
                for kw in node.keywords:
                    if kw.arg == "target":
                        if isinstance(kw.value, ast.Name):
                            target_name = kw.value.id
                        elif isinstance(kw.value, ast.Attribute):
                            target_name = call_name(ast.Call(func=kw.value, args=[], keywords=[])) or "?"
                        elif isinstance(kw.value, ast.Lambda):
                            target_name = "<lambda>"
                    elif kw.arg == "daemon":
                        if isinstance(kw.value, ast.Constant):
                            daemon = bool(kw.value.value)
                self.threads.append((target_name, daemon, line, self._current or "<module>"))

            # Locks / Events / Semaphores / Conditions  — only at module level we capture creators,
            # but companion uses (.acquire/.release/.set/.wait) propagate via all_calls
            if name in (
                "threading.Lock", "Lock", "threading.RLock", "RLock",
                "threading.Event", "Event", "threading.Semaphore", "Semaphore",
                "threading.Condition", "Condition",
            ):
                # Look up via parent chain in module-level Assign? Caller will join.
                kind = name.split(".")[-1]
                self.locks.append(("<expr>", kind, line))

            # urllib.request.urlopen / urllib.request.Request → external HTTP client
            if name in ("urllib.request.urlopen", "urlopen", "urllib.request.Request", "Request"):
                target = "?"
                if node.args:
                    a = node.args[0]
                    if isinstance(a, ast.Constant) and isinstance(a.value, str):
                        target = a.value
                    elif isinstance(a, ast.JoinedStr):  # f-string
                        target = "<f-string>"
                self.http_clients.append((line, self._current or "<module>", target))

            # File I/O
            if name == "open":
                mode = "r"
                if len(node.args) >= 2 and isinstance(node.args[1], ast.Constant):
                    mode = str(node.args[1].value)
                else:
                    for kw in node.keywords:
                        if kw.arg == "mode" and isinstance(kw.value, ast.Constant):
                            mode = str(kw.value.value)
                self.file_io.append((line, self._current or "<module>", mode))
            if name in ("os.makedirs", "os.mkdir", "Path.mkdir", "shutil.copy", "shutil.copyfile",
                        "shutil.copytree", "shutil.rmtree", "os.remove", "os.unlink", "os.rename"):
                self.file_io.append((line, self._current or "<module>", name))

            # os.environ.get(...)
            if name == "os.environ.get":
                var = "?"
                default = None
                if node.args and isinstance(node.args[0], ast.Constant):
                    var = str(node.args[0].value)
                if len(node.args) >= 2 and isinstance(node.args[1], ast.Constant):
                    default = node.args[1].value
                self.env_vars.append((var, default, line))

            # request.get_json(...)
            if name in ("request.get_json", "data.get_json"):
                self.json_in.append((line, self._current or "<module>"))

            # jsonify({...})
            if name == "jsonify":
                keys: list[str] = []
                if node.args and isinstance(node.args[0], ast.Dict):
                    for k in node.args[0].keys:
                        if isinstance(k, ast.Constant) and isinstance(k.value, str):
                            keys.append(k.value)
                # also kwargs form: jsonify(a=1, b=2)
                for kw in node.keywords:
                    if kw.arg:
                        keys.append(kw.arg)
                self.json_out_keys.append((line, self._current or "<module>", keys))

        self.generic_visit(node)

    # Catch:  if name == "some_tool":  inside dispatch_tool
    def visit_Compare(self, node):
        if (isinstance(node.left, ast.Name) and node.left.id == "name"
                and len(node.ops) == 1 and isinstance(node.ops[0], ast.Eq)
                and len(node.comparators) == 1):
            comp = node.comparators[0]
            if isinstance(comp, ast.Constant) and isinstance(comp.value, str):
                self.dispatch_entries.append((comp.value, getattr(node, "lineno", "?")))
        self.generic_visit(node)


az = Analyzer()
az.visit(tree)

# Resolve <expr> lock var names by scanning module-level assignments where RHS is a lock call
lock_lines = {l[2] for l in az.locks}
named_locks = []  # (name, kind, line)
for g_name, g_line in az.module_globals:
    if g_line in lock_lines:
        # find which lock kind matches this line
        for _vn, kind, ln in az.locks:
            if ln == g_line:
                named_locks.append((g_name, kind, ln))
                break

# Dedup dispatch entries, keep first occurrence per tool name
seen_d, dispatch = set(), []
for n, l in az.dispatch_entries:
    if n not in seen_d:
        seen_d.add(n); dispatch.append((n, l))

# ── UI / embedded HTML+JS regex pass ──────────────────────────────────────────
# We pull from the raw `src` string because that JS lives inside Python f-strings
# and triple-quoted blocks rendered by render_template_string. AST won't help there.

# fetch('/api/...') or fetch(`/api/${...}`) etc.
ui_fetches: dict[str, list[int]] = collections.defaultdict(list)
fetch_re = re.compile(r"""fetch\(\s*['"`]([^'"`)]+)['"`]""")
for i, ln in enumerate(lines, start=1):
    for m in fetch_re.finditer(ln):
        ui_fetches[m.group(1)].append(i)

# DOM event handlers: $('id').addEventListener('event', fn) or onclick="fn()"
# Forms covered:
#   $('id').addEventListener('click', send);
#   document.getElementById('id').addEventListener('change', ...)
#   <button onclick="poke()">  / <a onclick="...">
ui_listeners: list[tuple[str, str, str, int]] = []  # (id, event, handler_excerpt, line)
addev_re = re.compile(
    r"""(?:\$\(['"`]([\w-]+)['"`]\)|getElementById\(['"`]([\w-]+)['"`]\))"""
    r"""\s*\.addEventListener\(\s*['"`](\w+)['"`]\s*,\s*([^)]{1,60})\)"""
)
onclick_re = re.compile(r"""<[^>]*\bid=['"]([\w-]+)['"][^>]*\bonclick=['"]([^'"]{1,80})['"]""", re.IGNORECASE)
for i, ln in enumerate(lines, start=1):
    for m in addev_re.finditer(ln):
        elem_id = m.group(1) or m.group(2) or "?"
        ui_listeners.append((elem_id, m.group(3), m.group(4).strip(), i))
    for m in onclick_re.finditer(ln):
        ui_listeners.append((m.group(1), "click", m.group(2).strip(), i))

# Also gather: `id="xxx"` element inventory (for cross-ref)
id_re = re.compile(r"""\bid=['"]([\w-]+)['"]""")
ui_ids: dict[str, list[int]] = collections.defaultdict(list)
for i, ln in enumerate(lines, start=1):
    for m in id_re.finditer(ln):
        ui_ids[m.group(1)].append(i)

# ── formatting ────────────────────────────────────────────────────────────────

W = 88
tool_set = {n for n, _ in az.tool_funcs}

def section(title):
    print(f"\n{'-' * W}\n  {title}\n{'-' * W}")

def ruled():
    print('=' * W)

ruled()
print(f"  STEVE MAP — {TARGET.name}   {len(lines)} lines · {len(az.functions)} functions")
ruled()

# ── 1. Flask routes ───────────────────────────────────────────────────────────
section(f"FLASK ROUTES  ({len(az.routes)})")
for r in sorted(az.routes, key=lambda x: x["line"]):
    methods = ",".join(r["methods"])
    print(f"  {r['path']:<42} [{methods:<12}] {r['func']}  (:{r['line']})")

# ── 2. Tool function definitions ──────────────────────────────────────────────
section(f"TOOL FUNCTIONS  ({len(az.tool_funcs)})")
for name, line in sorted(az.tool_funcs, key=lambda x: x[1]):
    dispatched = name.removeprefix("tool_") in seen_d
    marker = "" if dispatched else "  <- NOT in dispatch"
    print(f"  :{line:<6}  {name}{marker}")

# ── 3. Dispatch entries ───────────────────────────────────────────────────────
section(f"TOOL DISPATCH ENTRIES  ({len(dispatch)})")
for name, line in sorted(dispatch, key=lambda x: x[1]):
    has_impl = f"tool_{name}" in tool_set
    marker = "ok" if has_impl else "!!  NO tool_ IMPL"
    print(f"  :{line:<6}  \"{name}\"  {marker}")

# ── 4. Broadcast / SSE events ─────────────────────────────────────────────────
section(f"BROADCAST (SSE) EVENTS  ({len(az.broadcasts)})")
by_evt: dict = collections.defaultdict(list)
for evt, line in az.broadcasts:
    by_evt[evt].append(line)
for evt in sorted(by_evt):
    lns = "  ".join(f":{l}" for l in sorted(by_evt[evt]))
    print(f"  {evt:<38}  {lns}")

# ── 5. LLM calls ──────────────────────────────────────────────────────────────
section(f"LLM CALLS  ({len(az.llm_calls)})")
for hint, line in az.llm_calls:
    src_line = line_text(line)[:72]
    print(f"  :{line:<6}  call_llm({hint!r:<30})  {src_line}")

# ── 6. External API calls ─────────────────────────────────────────────────────
section(f"EXTERNAL API CALLS  ({len(az.external_calls)})")
ext_by: dict = collections.defaultdict(list)
for name, line in az.external_calls:
    ext_by[name].append(line)
for name in sorted(ext_by):
    lns = "  ".join(f":{l}" for l in sorted(ext_by[name]))
    print(f"  {name:<48}  {lns}")

# ── 7. Full function index ────────────────────────────────────────────────────
section(f"ALL FUNCTIONS  ({len(az.functions)})")
for f in sorted(az.functions, key=lambda x: x["line"]):
    args  = ", ".join(f["args"])
    decos = ("  @" + " @".join(f["decorators"])) if f["decorators"] else ""
    print(f"  :{f['line']:<6}  def {f['name']}({args}){decos}")

# ── 8. Call graph — tool_* callers ────────────────────────────────────────────
section("CALL GRAPH — who calls each tool_*")
callers_of: dict = collections.defaultdict(list)
for caller, callee, line in az.all_calls:
    if callee in tool_set and caller != callee:
        callers_of[callee].append((caller or "<module>", line))
for tool in sorted(callers_of):
    cs = "  ".join(f"{c}:{l}" for c, l in sorted(callers_of[tool], key=lambda x: x[1]))
    print(f"  {tool:<44}  <- {cs}")

# ── 9. Orphaned tool_* (defined but never called) ────────────────────────────
section("ORPHANED TOOL FUNCTIONS  (defined but never called anywhere)")
called_anywhere = {callee for _, callee, _ in az.all_calls}
orphans = [(n, l) for n, l in az.tool_funcs if n not in called_anywhere]
if orphans:
    for name, line in sorted(orphans, key=lambda x: x[1]):
        print(f"  :{line:<6}  {name}")
else:
    print("  (none)")

# ── 10. Imports & stdlib usage ────────────────────────────────────────────────
section(f"IMPORTS  ({len(az.imports)})")
imp_groups: dict[str, list[tuple]] = collections.defaultdict(list)
for kind, mod, name, line in az.imports:
    imp_groups[mod].append((kind, name, line))
for mod in sorted(imp_groups):
    items = imp_groups[mod]
    examples = ", ".join(sorted({n for _, n, _ in items if n}))[:60]
    head_line = min(l for _, _, l in items)
    print(f"  :{head_line:<6}  {mod:<32} {examples}")

# ── 11. Environment variables ─────────────────────────────────────────────────
section(f"ENV VARS  ({len(set(v for v, _, _ in az.env_vars))} distinct, {len(az.env_vars)} sites)")
env_by: dict = collections.defaultdict(list)
env_default: dict = {}
for var, default, line in az.env_vars:
    env_by[var].append(line)
    if default is not None and var not in env_default:
        env_default[var] = default
for var in sorted(env_by):
    lns = "  ".join(f":{l}" for l in sorted(env_by[var]))
    dflt = env_default.get(var)
    if dflt is None:
        dflt_s = ""
    else:
        dflt_s = f"  default={dflt!r}"
    print(f"  {var:<32}  {lns}{dflt_s}")

# ── 12. Module-level state ────────────────────────────────────────────────────
section(f"MODULE-LEVEL STATE  ({len(az.module_globals)} assignments)")
seen_g = set()
for name, line in az.module_globals:
    if name in seen_g:
        continue
    seen_g.add(name)
    src_line = line_text(line)[:72]
    print(f"  :{line:<6}  {name:<28}  {src_line}")

# ── 13. Locks & events ────────────────────────────────────────────────────────
section(f"LOCKS & EVENTS  ({len(named_locks)} named, {len(az.locks)} total constructors)")
for name, kind, line in named_locks:
    print(f"  :{line:<6}  {kind:<12}  {name}")
unnamed = [(k, l) for _, k, l in az.locks if not any(ln == l for _, _, ln in named_locks)]
if unnamed:
    print("  --- unnamed (inline) ---")
    for kind, line in unnamed:
        print(f"  :{line:<6}  {kind:<12}  <inline>")

# ── 14. Threads ───────────────────────────────────────────────────────────────
section(f"THREADS  ({len(az.threads)} Thread() sites)")
thr_by: dict = collections.defaultdict(list)
for tgt, daemon, line, in_func in az.threads:
    thr_by[tgt].append((line, in_func, daemon))
for tgt in sorted(thr_by):
    sites = thr_by[tgt]
    daemon_flag = "daemon" if any(d for _, _, d in sites) else "non-daemon"
    locs = "  ".join(f"{f}:{l}" for l, f, _ in sorted(sites))
    print(f"  {tgt:<32}  [{daemon_flag:<10}]  {locs}")

# ── 15. Background loops (long-lived) ─────────────────────────────────────────
section(f"BACKGROUND LOOPS  ({len(az.bg_loops)})")
for cond, line, in_func in az.bg_loops:
    src_line = line_text(line)[:72]
    print(f"  :{line:<6}  [{in_func}]  {src_line}")

# ── 16. Loop density per function ─────────────────────────────────────────────
section("LOOP DENSITY (functions with >0 loops)")
loop_rows = sorted(
    ((fn, c["for"], c["while"]) for fn, c in az.func_loop_counts.items()
     if c["for"] or c["while"]),
    key=lambda x: -(x[1] + x[2])
)
print(f"  {'function':<44} {'for':>4} {'while':>6}")
for fn, fc, wc in loop_rows[:30]:
    print(f"  {fn:<44} {fc:>4} {wc:>6}")
if len(loop_rows) > 30:
    print(f"  ... and {len(loop_rows) - 30} more")

# ── 17. UI → backend fetch map ────────────────────────────────────────────────
section(f"UI → BACKEND fetch()  ({len(ui_fetches)} distinct endpoints)")
# Mark which ones map to a known Flask route
route_paths = {r["path"] for r in az.routes}
def _path_matches_route(p: str) -> bool:
    if p in route_paths:
        return True
    # crude match for /api/image/<token> style
    for rp in route_paths:
        if "<" in rp:
            pat = re.escape(rp).replace(r"<[^>]+>", r"[^/]+").replace(r"\<", "<").replace(r"\>", ">")
            pat = re.sub(r"<[^>]+>", "[^/]+", rp)
            if re.fullmatch(pat.replace("/", r"\/"), p):
                return True
    return False
for url in sorted(ui_fetches):
    lns = "  ".join(f":{l}" for l in ui_fetches[url][:6])
    more = "" if len(ui_fetches[url]) <= 6 else f" (+{len(ui_fetches[url])-6} more)"
    matched = "ok" if _path_matches_route(url) else "?? "
    print(f"  [{matched}]  {url:<40}  {lns}{more}")

# ── 18. UI event handlers ─────────────────────────────────────────────────────
section(f"UI EVENT HANDLERS  ({len(ui_listeners)})")
for elem, ev, fn, line in sorted(ui_listeners, key=lambda x: x[3]):
    fn_short = fn[:46]
    print(f"  :{line:<6}  #{elem:<20}  on {ev:<8} → {fn_short}")

# ── 19. JSON request/response shapes ──────────────────────────────────────────
section(f"REQUEST get_json() SITES  ({len(az.json_in)})")
for line, in_func in az.json_in[:30]:
    src_line = line_text(line)[:60]
    print(f"  :{line:<6}  [{in_func or '?'}]  {src_line}")
if len(az.json_in) > 30:
    print(f"  ... and {len(az.json_in) - 30} more")

section(f"RESPONSE jsonify() KEY SHAPES  ({len(az.json_out_keys)} sites)")
shape_groups: dict = collections.defaultdict(list)
for line, in_func, keys in az.json_out_keys:
    if not keys:
        continue
    sig = ",".join(sorted(keys))
    shape_groups[sig].append((line, in_func))
for sig in sorted(shape_groups, key=lambda s: -len(shape_groups[s]))[:40]:
    sites = shape_groups[sig]
    locs = "  ".join(f"{f}:{l}" for l, f in sites[:3])
    more = "" if len(sites) <= 3 else f"  (+{len(sites)-3})"
    print(f"  {{{sig}}}  ×{len(sites):<3}  {locs}{more}")

# ── 20. External HTTP client sites ────────────────────────────────────────────
section(f"EXTERNAL HTTP CLIENTS  ({len(az.http_clients)} urlopen/Request sites)")
http_by_func: dict = collections.defaultdict(list)
for line, in_func, target in az.http_clients:
    http_by_func[in_func].append((line, target))
for fn in sorted(http_by_func, key=lambda f: -len(http_by_func[f]))[:30]:
    sites = http_by_func[fn]
    print(f"  [{fn}]  ×{len(sites)}")
    for line, target in sites[:6]:
        print(f"    :{line:<6}  {target[:64]}")
    if len(sites) > 6:
        print(f"    ... and {len(sites) - 6} more")

# ── 21. File I/O ──────────────────────────────────────────────────────────────
section(f"FILE I/O SITES  ({len(az.file_io)})")
io_by_kind: dict = collections.defaultdict(list)
for line, in_func, mode in az.file_io:
    io_by_kind[mode].append((line, in_func))
for mode in sorted(io_by_kind):
    sites = io_by_kind[mode]
    print(f"  mode/op {mode!r:<24}  ×{len(sites)}")
    for line, fn in sites[:6]:
        src_line = line_text(line)[:60]
        print(f"    :{line:<6}  [{fn or '?'}]  {src_line}")
    if len(sites) > 6:
        print(f"    ... and {len(sites) - 6} more")

# ── 22. Rust port hints (synthesized) ─────────────────────────────────────────
section("RUST PORT HINTS  (synthesized — read this as a checklist)")

# Estimated counts
n_routes      = len(az.routes)
n_threads     = len(az.threads)
n_bg          = len(az.bg_loops)
n_locks       = len(named_locks)
n_globals     = len({n for n, _ in az.module_globals})
n_env         = len(set(v for v, _, _ in az.env_vars))
n_external    = len(set(c for c, _ in az.external_calls))
n_broadcasts  = len(set(e for e, _ in az.broadcasts))
n_fetches     = len(ui_fetches)
n_handlers    = len(ui_listeners)
n_json_in     = len(az.json_in)
n_json_out    = len(az.json_out_keys)

hints = [
    ("Web framework",
     f"{n_routes} Flask routes → axum/actix-web router. SSE endpoint on /api/stream."),
    ("Concurrency",
     f"{n_threads} Thread() spawns + {n_bg} background loops + {n_locks} named locks. "
     f"Map to tokio tasks + Arc<Mutex<...>> / Arc<RwLock<...>>. Each `threading.Event` becomes "
     f"`tokio::sync::Notify` or `CancellationToken`."),
    ("Mutable globals",
     f"{n_globals} module-level names. Either lift into a single AppState struct held in axum "
     f"State<Arc<AppState>>, or use OnceLock/Lazy for truly immutable config. Avoid scattered "
     f"`static mut`."),
    ("External clients",
     f"{n_external} distinct external API surfaces (anthropic/openai/gemini/ollama/ferricula/"
     f"comfy/grub/serp/hyperia/elevenlabs). Use `reqwest::Client` (one shared instance, cloneable)."),
    ("Configuration",
     f"{n_env} env vars → one Config struct, parsed once at startup with sensible defaults. "
     f"`figment` or hand-rolled `std::env::var(...)`."),
    ("SSE event surface",
     f"{n_broadcasts} distinct event names broadcast. Define `enum SseEvent {{ ... }}` with "
     f"`#[derive(Serialize)]`; render via axum's `Sse<Stream<Item = SseEvent>>`."),
    ("Tool dispatch",
     f"{len(dispatch)} tools dispatched by string match. Replace with `enum ToolCall {{ ... }}` "
     f"(serde-tagged) and a `match` in `handle_tool(call: ToolCall) -> ToolResult`."),
    ("LLM provider abstraction",
     "Multiple providers (claude, openai, gemini, ollama). Define `trait LlmProvider {{ async fn "
     "complete(&self, sys: &str, msgs: Vec<Msg>, tools: Option<&[Tool]>) -> Result<LlmResult>; }}` "
     "and pick at runtime via the same routing logic in call_llm()."),
    ("UI surface",
     f"{n_fetches} distinct fetch endpoints + {n_handlers} DOM event handlers. "
     "The HTML/JS is currently inline in steve.py — keep it as a single static asset "
     "served from a `RustEmbed` directory, OR migrate to a separate frontend (Leptos/Sycamore) "
     "and have axum serve only JSON."),
    ("JSON shapes",
     f"{n_json_in} get_json() request sites + {n_json_out} jsonify() responses. Define typed "
     "structs with serde derives — no untyped serde_json::Value flowing through handlers."),
    ("File I/O",
     f"{len(az.file_io)} I/O sites. Use tokio::fs for async hot paths; std::fs is fine for "
     "startup/checkpoint paths. Watch for the embedded `dreams/`, `draws/`, `logs/`, `audio/` dirs."),
    ("Background loops",
     f"{n_bg} long-lived loops (think_loop, advocate_loop, reminder_loop, etc.). Each becomes a "
     "spawned tokio task with a `CancellationToken`. Shutdown wires through `select!`."),
    ("HTML in code",
     "The full UI is embedded in steve.py via render_template_string. "
     "For Rust: ship the HTML/CSS/JS as static files (`include_str!` or `RustEmbed`) and let "
     "the agent code stay focused on the API."),
    ("Postcard / ferricula compat",
     "ferricula already speaks postcard. Use `postcard` crate if you want the Rust agent to "
     "read snapshots directly without going through HTTP. Otherwise keep talking via HTTP at "
     "FERRICULA_URL."),
    ("Test surface",
     f"{len(az.functions)} functions. Look for pure/computational ones first (model selection, "
     "budget pressure, cost calc, prompt assembly) — those port and test cleanly. Threaded loops "
     "and HTTP handlers are integration territory."),
]
for title, body in hints:
    # word-wrap the body
    print(f"  • {title}")
    words = body.split()
    line = "    "
    for w in words:
        if len(line) + len(w) + 1 > W - 2:
            print(line.rstrip())
            line = "    " + w + " "
        else:
            line += w + " "
    if line.strip():
        print(line.rstrip())

# Migration order suggestion
section("RUST PORT — suggested module order")
order = [
    ("config.rs",       "env vars + defaults (one-shot, no async)"),
    ("model.rs",        "AppState + model rank + budget pressure (pure)"),
    ("ferricula.rs",    "HTTP client wrapping FERRICULA_URL (mirror clients.py)"),
    ("llm.rs",          "LlmProvider trait + claude/openai/gemini/ollama impls + call_llm router"),
    ("tools.rs",        "ToolCall enum + dispatch + each tool fn"),
    ("sse.rs",          "SseEvent enum + broadcast channel + /api/stream handler"),
    ("routes.rs",       "axum routes — small handlers calling into above"),
    ("think_loop.rs",   "background task: think cycle"),
    ("advocate_loop.rs","background task: advocate (local-only)"),
    ("dream.rs",        "dream visualization (talks to ferricula + image providers)"),
    ("ui/",             "static HTML/CSS/JS bundle (RustEmbed) — last"),
    ("main.rs",         "wire State, spawn tasks, axum::serve"),
]
for fname, body in order:
    print(f"  {fname:<22}  {body}")

print(f"\n{'═' * W}\n")
