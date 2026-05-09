"""Steve — ferricula autonomous agent UI.

Chat with Steve on the left. Watch him think and browse on the right.
Toggle thinking mode to let him run autonomously between responses.

Usage:
    cd arena
    pip install flask
    AGENT_KEY=sk-ant-... python steve.py

    FERRICULA_URL defaults to http://localhost:8773 (steve-jobs container)
    SERPAPI_API_KEY enables web search
    GNOSIS_CRAWL_URL enables full page crawling (defaults to grubcrawler.dev)
    THINK_INTERVAL sets seconds between autonomous cycles (default 45)
"""

import json
import os
import queue
import re
import random
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime

from flask import Flask, Response, jsonify, render_template_string, request

# Load .env from the arena directory if present — so voice/API keys work without sourcing
try:
    from dotenv import load_dotenv
    load_dotenv(os.path.join(os.path.dirname(__file__), ".env"))
except ImportError:
    pass

# Windows console defaults to cp1252 — force UTF-8 so emoji and arrows print cleanly
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
if hasattr(sys.stderr, "reconfigure"):
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

sys.path.insert(0, os.path.dirname(__file__))
from clients import FerriculaClient

# ── Config ────────────────────────────────────────────────────────────────────

FERRICULA_URL   = os.environ.get("FERRICULA_URL",   "http://localhost:8773")
HYPERIA_URL     = os.environ.get("HYPERIA_URL",     "http://localhost:9800")
SHIVVR_URL      = os.environ.get("SHIVVR_URL",      "https://shivvr.nuts.services")
ANTHROPIC_KEY   = os.environ.get("AGENT_KEY") or os.environ.get("ANTHROPIC_API_KEY")
OPENAI_KEY      = os.environ.get("OPENAI_API_KEY")
SERPAPI_KEY     = os.environ.get("SERPAPI_API_KEY") or os.environ.get("SERPAPI_KEY")
CRAWL_URL       = os.environ.get("GNOSIS_CRAWL_URL", "https://grubcrawler.dev")
CRAWL_KEY       = os.environ.get("GRUB_API_KEY", "")
OLLAMA_URL         = os.environ.get("OLLAMA_URL", "http://localhost:11434")
OLLAMA_MODEL       = os.environ.get("OLLAMA_MODEL", "gemma4:e2b")
OLLAMA_MODEL_LARGE = os.environ.get("OLLAMA_MODEL_LARGE", "gemma4:31b-cloud")
OPENAI_MODEL_FAST  = os.environ.get("OPENAI_FAST_MODEL", "gpt-4o-mini")
OPENAI_MODEL       = os.environ.get("OPENAI_MODEL", "gpt-4o")
CLAUDE_MODEL       = os.environ.get("CLAUDE_MODEL", "claude-sonnet-4-6")
CLAUDE_MODEL_OPUS  = os.environ.get("CLAUDE_OPUS_MODEL", "claude-opus-4-7")
GEMINI_KEY         = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
GEMINI_MODEL       = os.environ.get("GEMINI_MODEL",       "gemini-3-flash-preview")     # Gemini 3 Flash
GEMINI_MODEL_PRO   = os.environ.get("GEMINI_MODEL_PRO",   "gemini-3.1-pro-preview")      # Gemini 3.1 Pro
GEMINI_MODEL_LITE  = os.environ.get("GEMINI_MODEL_LITE",  "gemini-3.1-flash-lite")       # Gemini 3.1 Flash-Lite (stable)
GEMINI_IMAGE_MODEL = os.environ.get("GEMINI_IMAGE_MODEL", "nano-banana-pro-preview")     # Nano Banana Pro
RADIO_URL       = os.environ.get("RADIO_URL", "http://nemesis:9090")
ELEVEN_KEY      = os.environ.get("ELEVENLABS_API_KEY", "")
ELEVEN_VOICE    = os.environ.get("ELEVENLABS_VOICE_ID", "ApsbCjXt5HguctE80a0i")
AGENTMAIL_KEY   = os.environ.get("AGENTMAIL_API_KEY") or os.environ.get("AGENTMAIL_KEY", "")
AGENTMAIL_EMAIL = os.environ.get("AGENTMAIL_EMAIL", "steve@nuts.services")
AGENTMAIL_URL   = "https://api.agentmail.to"
THINK_INTERVAL  = int(os.environ.get("THINK_INTERVAL", "45"))
ADVOCATE_INTERVAL = int(os.environ.get("ADVOCATE_INTERVAL", "180"))  # seconds between advocate cycles
HOURLY_BUDGET   = float(os.environ.get("HOURLY_BUDGET", "2.0"))   # $/hr — 0 = unlimited
DREAMS_DIR      = os.path.join(os.path.dirname(__file__), "dreams")
UPLOADS_DIR     = os.path.join(os.path.dirname(__file__), "uploads")
DRAWS_DIR       = os.path.join(os.path.dirname(__file__), "draws")
AUDIO_DIR       = os.path.join(os.path.dirname(__file__), "audio")
COMFYUI_URL     = os.environ.get("COMFYUI_URL", "http://localhost:8000")

# UPDATE PRICES HERE when models change rates (per million tokens: input, output, cache_read)
_MODEL_COSTS: dict[str, tuple[float, float, float]] = {
    "claude-opus-4":    (15.00, 75.00, 1.50),
    "claude-sonnet-4":  ( 3.00, 15.00, 0.30),
    "claude-haiku-4":   ( 0.80,  4.00, 0.08),
    "gpt-4o-mini":      ( 0.15,  0.60, 0.0),
    "gpt-4.1-mini":     ( 0.40,  1.60, 0.0),
    "gpt-4.1-nano":     ( 0.10,  0.40, 0.0),
    "gpt-4o":           ( 2.50, 10.00, 0.0),
    "gpt-4.1":          ( 2.00,  8.00, 0.0),
    "gpt-5":            ( 2.50, 10.00, 0.0),
    # Gemini 3 family — substring match catches preview suffixes etc.
    # Real API IDs (per /v1beta/models): gemini-3.1-pro-preview, gemini-3-flash-preview,
    # gemini-3.1-flash-lite, nano-banana-pro-preview, gemini-3.1-flash-image-preview
    "gemini-3.1-pro":   ( 1.25,  5.00, 0.0),  # 3.1 Pro
    "gemini-3-pro":     ( 1.25,  5.00, 0.0),
    "gemini-3.1-flash-lite": ( 0.04, 0.15, 0.0),
    "gemini-3.1-flash": ( 0.075, 0.30, 0.0),
    "gemini-3-flash":   ( 0.075, 0.30, 0.0),
    "gemini-3":         ( 0.075, 0.30, 0.0),  # catch-all for 3.x flash variants
    "nano-banana":      ( 0.0,   0.0,  0.0),  # image cost tracked elsewhere via _IMAGE_COSTS
    "gemini-2.5-flash": ( 0.075, 0.30, 0.0),  # legacy
    "gemini":           ( 0.075, 0.30, 0.0),
    "gemma":            ( 0.0,   0.0,  0.0),   # all Ollama/local — always free
    "gemma4":           ( 0.0,   0.0,  0.0),
}

def _cost_for(model_id: str, in_tok: int, out_tok: int, cache_r: int = 0) -> float:
    model_lower = model_id.lower()
    for key, (inp, outp, cache) in _MODEL_COSTS.items():
        if key in model_lower:
            return (in_tok * inp + out_tok * outp + cache_r * cache) / 1_000_000
    return 0.0

# Model cost rank: 0=free/local, 1=cheap cloud, 2=mid cloud, 3=expensive
_MODEL_COST_RANK: dict[str, int] = {
    "gemma":       0,
    "gemma_large": 0,
    "gemini_lite": 1,   # 3.1 Flash-Lite — cheapest cloud
    "openai_fast": 1,
    "gemini":      1,   # 3 Flash — frontier perf at flash price
    "openai":      2,
    "claude":      2,
    "gemini_pro":  3,   # 3.1 Pro — frontier tier
    "claude_opus": 3,
}

def _log_cost_entry(cost: float) -> None:
    now = time.time()
    with _hourly_log_lock:
        _hourly_log.append((now, cost))
        cutoff = now - 3600
        while _hourly_log and _hourly_log[0][0] < cutoff:
            _hourly_log.pop(0)

def _hourly_cost() -> float:
    now = time.time()
    cutoff = now - 3600
    with _hourly_log_lock:
        return sum(c for t, c in _hourly_log if t >= cutoff)

def _budget_pressure() -> float:
    if HOURLY_BUDGET <= 0:
        return 0.0
    return min(1.0, _hourly_cost() / HOURLY_BUDGET)

def _budget_nudge(preferred: str) -> str:
    """Downgrade model if hourly budget pressure is high."""
    pressure = _budget_pressure()
    max_rank = 3
    for threshold, cap in [(0.50, 2), (0.75, 1), (0.90, 0)]:
        if pressure >= threshold:
            max_rank = cap
    rank = _MODEL_COST_RANK.get(preferred, 2)
    if rank <= max_rank:
        return preferred
    candidates = [m for m, r in _MODEL_COST_RANK.items() if r <= max_rank]
    if not candidates:
        return "gemma"
    best_rank = max(_MODEL_COST_RANK[m] for m in candidates)
    top = [m for m in candidates if _MODEL_COST_RANK[m] == best_rank]
    nudged = random.choice(top)
    if nudged != preferred:
        pct = int(pressure * 100)
        broadcast("think_text", {"ts": _ts(), "text": f"💸 {pct}% budget pressure → {_MODEL_LABELS.get(nudged, nudged)}"})
    return nudged

ferricula = FerriculaClient(FERRICULA_URL, "steve")

# ── Emotion state machine ─────────────────────────────────────────────────────

EMOTIONS = ["joy", "trust", "fear", "surprise", "sadness", "boredom", "anger", "interest"]

BLENDS = {
    frozenset(["joy",      "trust"]):    "love",
    frozenset(["joy",      "interest"]): "optimism",
    frozenset(["joy",      "surprise"]): "delight",
    frozenset(["trust",    "fear"]):     "submission",
    frozenset(["trust",    "anger"]):    "dominance",
    frozenset(["fear",     "surprise"]): "awe",
    frozenset(["sadness",  "interest"]): "melancholy",
    frozenset(["sadness",  "surprise"]): "disapproval",
    frozenset(["boredom",  "sadness"]):  "withdrawal",
    frozenset(["anger",    "interest"]): "agitation",
    frozenset(["anger",    "surprise"]): "outrage",
    frozenset(["interest", "surprise"]): "alertness",
}

# When Steve enters one of these states he surfs on his own, may ignore chat
SURF_STATES = {"interest", "boredom", "surprise", "optimism", "agitation", "alertness", "delight"}

# ── Model selection per emotion ───────────────────────────────────────────────
# Steve Jobs persona: Revolution (hex 49) · Pisces · base anger
#
# Gemma  (local, fast)   = raw reactive states — no cloud needed
# Claude (cloud, deep)   = synthesis, building, vision, grief — depth matters
# Gemini (broad, quick)  = curiosity/knowledge states — wide coverage, fast
# OpenAI (practical)      = judgment/output states — gpt-4o efficient and direct
# OpenAI fast             = hot reactive states — gpt-4o-mini, lowest latency
# Claude Opus             = deepest vision/grief states — worth the cost
# gemma                   = pure local, only during extended idle
#
# Fallback chain: preferred → other cloud models → gemma
EMOTION_MODEL: dict[str, str] = {
    # ── Base 8 ──────────────────────────────────────────────────────────────
    "joy":          "openai",       # getting things done, shipping
    "trust":        "gemini",       # encyclopedic trust, broad synthesis
    "fear":         "openai_fast",  # threat response — lowest latency
    "surprise":     "openai_fast",  # immediate reaction — fast
    "sadness":      "claude_opus",  # deep grief — full depth
    "boredom":      "openai_fast",  # restless surface scan
    "anger":        "openai_fast",  # raw heat — reactive
    "interest":     "gemini",       # wide curiosity, broad knowledge
    # ── Primary dyads ───────────────────────────────────────────────────────
    "love":         "claude_opus",  # deep creative synthesis
    "optimism":     "claude_opus",  # visionary building — Steve's best state
    "delight":      "openai",       # quick practical joy
    "submission":   "gemini",       # broad analysis of constraint
    "dominance":    "claude_opus",  # anger into command — Steve's power state
    "awe":          "claude_opus",  # transcendence
    "melancholy":   "claude",       # reflective depth
    "disapproval":  "openai",       # sharp practical judgment
    "withdrawal":   "claude",       # inward, quiet
    "agitation":    "openai",       # focused analytical frustration
    "outrage":      "openai_fast",  # pure hot reaction
    "alertness":    "gemini",       # fast cross-domain pattern spotting
}
# Budget pressure progressively caps max model rank — see _budget_nudge():
#   <50% spend  → full range (opus included)
#   50–75%      → no opus
#   75–90%      → gemini / openai_fast only
#   ≥90%        → gemma (local) only

def radio_rand() -> float:
    """True random float from SDR entropy source. Falls back to random.random()."""
    try:
        req = urllib.request.Request(
            f"{RADIO_URL}/api/entropy?bytes=8&format=json",
            headers={"Accept": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=2) as r:
            data = json.loads(r.read().decode())
        hex_str = data.get("entropy_hex", "")
        if hex_str:
            val = int(hex_str[:16], 16)  # 8 bytes → 64-bit int
            return val / (2**64)
    except Exception:
        pass
    return random.random()

_ALL_MODELS    = ("claude", "claude_opus", "gemini", "gemini_pro", "gemini_lite",
                  "openai", "openai_fast", "gemma")
_CLOUD_MODELS  = ("claude", "claude_opus", "gemini", "gemini_pro", "gemini_lite",
                  "openai", "openai_fast")

_MODEL_LABELS = {
    "claude":       CLAUDE_MODEL,
    "claude_opus":  CLAUDE_MODEL_OPUS,
    "gemini":       GEMINI_MODEL,           # gemini-3-flash (default mid-tier)
    "gemini_pro":   GEMINI_MODEL_PRO,       # gemini-3.1-pro-preview (frontier)
    "gemini_lite":  GEMINI_MODEL_LITE,      # gemini-3.1-flash-lite (cheap)
    "openai":       OPENAI_MODEL,
    "openai_fast":  OPENAI_MODEL_FAST,
    "gemma":        OLLAMA_MODEL,
    "gemma_large":  OLLAMA_MODEL_LARGE,
}

def model_for_emotion(emotion: str) -> str:
    """Return the model for the given emotion, with 2% SDR-gated random flip."""
    base = EMOTION_MODEL.get(emotion, "claude")
    if radio_rand() < 0.02:
        others = [m for m in _ALL_MODELS if m != base]
        return random.choice(others)
    return base

_emotion_lock = threading.Lock()
_current_emotion = "interest"
_active_model = "—"
_emotion_blend: str | None = None   # set when two emotions are active
_user_model_override: str | None = None   # set via /api/set-model by the user
_override_set_time: float = 0.0           # when the override was set
_OVERRIDE_IDLE_EXPIRE = 1800              # 30 min of user activity before temporary override expires

# Model heat tracking — consecutive uses; overheat forces a switch
_model_heat: dict[str, int] = {}
_model_error_until: dict[str, float] = {}   # model → timestamp when it may be retried
_model_failures: dict[str, int] = {}        # consecutive failure count → drives exponential backoff
_comfy_busy: bool = False                   # True while ComfyUI is rendering — blocks local GPU models
_presentations: dict[str, dict] = {}   # token → {html, slides, title, ts}
_presentation_html: str = ""           # latest — backward compat
_presentation_slides: list = []
_presentation_title: str = ""
_impress_js_cache: str = ""
_impress_css_cache: str = ""
_image_mode: str = "auto"   # "auto" | "local" | "cloud" — controls tool_draw provider preference
_local_mode: bool = False   # when True, only local models (gemma) and ComfyUI — no cloud APIs
_dream_provider_idx: int = 0               # rotates through ["comfy","dalle3","gemini"] each dream
_nb_error_last: float = 0.0                 # epoch of last notebook error injection — rate-limits Steve's auto-fix

# Cost tracking — session total, per-cycle accumulator, rolling hourly window
_session_cost: float = 0.0
_session_cost_lock = threading.Lock()
_cycle_cost: float = 0.0
_hourly_log: list[tuple[float, float]] = []   # (timestamp, cost) — 1hr rolling window
_hourly_log_lock = threading.Lock()
_MODEL_OVERHEAT = 4          # consecutive uses before forced cool-down
_model_heat_lock = threading.Lock()

def _check_model_heat(preferred: str) -> str:
    """If preferred model is overheated, roll a random cloud replacement."""
    with _model_heat_lock:
        heat = _model_heat.get(preferred, 0)
        if heat < _MODEL_OVERHEAT:
            _model_heat[preferred] = heat + 1
            return preferred
        _model_heat[preferred] = 0
        print(f"[heat] {preferred} overheated — forcing switch")
    # Only roll from cloud models — local gemma stays as last-resort fallback in call_llm
    others = [m for m in _CLOUD_MODELS if m != preferred]
    roll = random.choice(others) if others else preferred
    broadcast("think_text", {"ts": _ts(), "text": f"🎲 {_MODEL_LABELS.get(preferred, preferred)} cooled → {_MODEL_LABELS.get(roll, roll)}"})
    return roll

def get_emotion() -> str:
    with _emotion_lock:
        return _emotion_blend or _current_emotion

def shift_emotion() -> tuple[str, str]:
    global _current_emotion, _emotion_blend
    with _emotion_lock:
        old = _emotion_blend or _current_emotion
        if random.random() < 0.25:
            e1, e2 = random.sample(EMOTIONS, 2)
            _current_emotion = e1
            _emotion_blend = BLENDS.get(frozenset([e1, e2]), e2)
        else:
            _current_emotion = random.choice(EMOTIONS)
            _emotion_blend = None
        new = _emotion_blend or _current_emotion
        _update_somatic(new)
        return old, new

# ── Somatic state — body sensations mapped from emotion ──────────────────────

_somatic_state = {"tension": 0.2, "activation": 0.5, "groundedness": 0.8}
_somatic_lock = threading.Lock()

_SOMATIC_TENSION = {
    "fear": 0.85, "anger": 0.75, "outrage": 0.9, "agitation": 0.7,
    "surprise": 0.5, "submission": 0.6, "disapproval": 0.55,
    "alertness": 0.4, "dominance": 0.5, "awe": 0.4, "sadness": 0.35,
    "melancholy": 0.3, "interest": 0.25, "boredom": 0.25,
    "joy": 0.15, "trust": 0.1, "love": 0.1, "optimism": 0.2,
    "delight": 0.15, "withdrawal": 0.2,
}
_SOMATIC_ACTIVATION = {
    "agitation": 0.9, "outrage": 0.95, "alertness": 0.8, "fear": 0.85,
    "anger": 0.8, "surprise": 0.75, "delight": 0.7, "dominance": 0.7,
    "optimism": 0.65, "joy": 0.6, "interest": 0.55, "awe": 0.5,
    "trust": 0.45, "submission": 0.4, "disapproval": 0.45,
    "sadness": 0.25, "melancholy": 0.3, "love": 0.4, "withdrawal": 0.15, "boredom": 0.2,
}
_SOMATIC_GROUND = {
    "trust": 0.9, "withdrawal": 0.85, "love": 0.85, "melancholy": 0.8,
    "sadness": 0.75, "boredom": 0.7, "submission": 0.65, "interest": 0.7,
    "optimism": 0.75, "joy": 0.8, "anger": 0.5, "agitation": 0.45,
    "dominance": 0.6, "alertness": 0.55, "delight": 0.6,
    "fear": 0.3, "surprise": 0.35, "outrage": 0.2, "disapproval": 0.45, "awe": 0.4,
}

def _update_somatic(emotion: str):
    with _somatic_lock:
        α = 0.35
        _somatic_state["tension"]      = _somatic_state["tension"]      * (1-α) + _SOMATIC_TENSION.get(emotion,      0.4) * α
        _somatic_state["activation"]   = _somatic_state["activation"]   * (1-α) + _SOMATIC_ACTIVATION.get(emotion,  0.5) * α
        _somatic_state["groundedness"] = _somatic_state["groundedness"] * (1-α) + _SOMATIC_GROUND.get(emotion,      0.7) * α

def _somatic_description() -> str:
    with _somatic_lock:
        t = _somatic_state["tension"]
        a = _somatic_state["activation"]
        g = _somatic_state["groundedness"]
    parts = []
    if   t > 0.72: parts.append("tight in the chest, something braced")
    elif t > 0.50: parts.append("low constriction, unresolved")
    elif t < 0.18: parts.append("open, easy")
    else:          parts.append("settled")
    if   a > 0.80: parts.append("high activation, wants to move")
    elif a > 0.62: parts.append("alert")
    elif a < 0.25: parts.append("still, low current")
    if   g < 0.38: parts.append("unmoored")
    elif g < 0.55: parts.append("not fully stable")
    return ", ".join(parts)

# ── SSE broadcast bus ─────────────────────────────────────────────────────────

_listeners: list[queue.Queue] = []
_listener_lock = threading.Lock()

# Rolling feed log — replayed to new clients on connect
_FEED_LOG_MAX = 200
_SKIP_REPLAY = {"mode_change", "model_switch", "open_tab"}   # stateful events — not worth replaying
_feed_log: list[dict] = []
_feed_log_lock = threading.Lock()

# Rolling chat log — replayed to new clients on page load
_CHAT_LOG_MAX = 100
_chat_log: list[dict] = []
_chat_log_lock = threading.Lock()

def _chat_log_append(role: str, text: str):
    with _chat_log_lock:
        _chat_log.append({"role": role, "text": text, "ts": _ts()})
        if len(_chat_log) > _CHAT_LOG_MAX:
            del _chat_log[:-_CHAT_LOG_MAX]


_THOUGHT_STOPWORDS = {
    "a","an","the","is","of","to","in","and","or","for","with","on","at","i","you","it",
    "this","that","be","was","were","are","am","my","your","they","them","we","us","he",
    "she","but","so","not","no","do","did","does","have","has","had","just","then","than",
    "if","as","by","up","out","into","from","what","why","how","when","where","who",
}

def _bm25_chat_match(thought: str, recent: int = 30, min_overlap: int = 1) -> str | None:
    """Rough token-overlap search across recent _chat_log entries — picks the best
    match for a random thought. Returns the matched chat text or None."""
    def _toks(s: str) -> set[str]:
        return {w for w in re.findall(r"[a-z0-9]+", s.lower())
                if w not in _THOUGHT_STOPWORDS and len(w) > 1}
    t_toks = _toks(thought)
    if not t_toks:
        return None
    with _chat_log_lock:
        window = list(_chat_log[-recent:])
    best_score = 0
    best_text: str | None = None
    for entry in reversed(window):
        e_toks = _toks(entry.get("text", ""))
        overlap = len(t_toks & e_toks)
        if overlap >= min_overlap and overlap > best_score:
            best_score = overlap
            best_text = entry.get("text", "")
    return best_text


def broadcast(event: str, data: dict):
    msg = f"event: {event}\ndata: {json.dumps(data)}\n\n"
    if event not in _SKIP_REPLAY:
        with _feed_log_lock:
            _feed_log.append({"event": event, "data": data})
            if len(_feed_log) > _FEED_LOG_MAX:
                del _feed_log[:-_FEED_LOG_MAX]
    with _listener_lock:
        dead = []
        for q in _listeners:
            try:
                q.put_nowait(msg)
            except queue.Full:
                dead.append(q)
        for q in dead:
            _listeners.remove(q)


def _ts():
    return datetime.now().strftime("%H:%M:%S")


def broadcast_system_update(what: str, detail: str = ""):
    broadcast("system_update", {"ts": _ts(), "what": what, "detail": detail})


def _auto_remember(text: str, importance: float = 0.6) -> None:
    """Fire-and-forget ferricula write for significant external actions."""
    def _bg():
        try:
            tool_remember(text, importance=importance, channel="actions")
        except Exception:
            pass
    threading.Thread(target=_bg, daemon=True).start()


# ── Tool implementations ──────────────────────────────────────────────────────

def tool_search(query: str) -> str:
    # Try SERPAPI first if key is available
    if SERPAPI_KEY:
        try:
            params = urllib.parse.urlencode({
                "q": query, "api_key": SERPAPI_KEY,
                "engine": "google", "num": "5",
            })
            req = urllib.request.Request(f"https://serpapi.com/search.json?{params}")
            with urllib.request.urlopen(req, timeout=10) as r:
                data = json.loads(r.read().decode())
            results = []
            if data.get("answer_box"):
                ab = data["answer_box"]
                results.append({"title": "Quick answer",
                                 "snippet": ab.get("snippet") or ab.get("answer", ""),
                                 "link": ""})
            for r in data.get("organic_results", [])[:5]:
                results.append({"title": r.get("title", ""),
                                 "snippet": r.get("snippet", ""),
                                 "link": r.get("link", "")})
            return json.dumps(results, indent=2)
        except Exception as e:
            print(f"[search] serpapi failed: {e}, falling back to grub+ddg")

    # Fall back: crawl DuckDuckGo HTML via grubcrawler — no key needed
    try:
        ddg_url = f"https://html.duckduckgo.com/html/?q={urllib.parse.quote_plus(query)}"
        body = json.dumps({
            "url": ddg_url,
            "options": {"timeout": 25, "javascript_enabled": False},
        }).encode()
        req = urllib.request.Request(
            f"{CRAWL_URL}/api/markdown", data=body,
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {CRAWL_KEY}"},
        )
        with urllib.request.urlopen(req, timeout=30) as r:
            data = json.loads(r.read().decode())
        markdown = data.get("markdown", "")
        if markdown:
            print(f"[search] grub+ddg OK ({len(markdown)} chars)")
            return markdown[:5000]
        return f"(search returned empty results for: {query})"
    except Exception as e:
        return f"search error: {e}"


CRAWL_CACHE_DIR = os.path.join(os.path.dirname(__file__), ".crawl_cache")
os.makedirs(CRAWL_CACHE_DIR, exist_ok=True)

def _crawl_cache_path(url: str) -> str:
    import hashlib
    slug = hashlib.sha1(url.encode()).hexdigest()[:16]
    safe = urllib.parse.quote(url, safe="")[:60].replace("%", "_")
    return os.path.join(CRAWL_CACHE_DIR, f"{safe}_{slug}.md")


def tool_read_url(url: str) -> str:
    import re as _re
    # Rewrite Google search URLs to DDG HTML — Google returns noscript junk
    _g = _re.search(r'[?&]q=([^&]+)', url) if "google.com/search" in url else None
    if _g:
        query = urllib.parse.unquote_plus(_g.group(1))
        url = f"https://html.duckduckgo.com/html/?q={urllib.parse.quote_plus(query)}"
        print(f"[crawl] rewrote google search → ddg: {query}")

    # Return cached version if it exists and is < 24h old
    cache_path = _crawl_cache_path(url)
    if os.path.exists(cache_path):
        age = time.time() - os.path.getmtime(cache_path)
        if age < 86400:
            with open(cache_path, "r", encoding="utf-8") as f:
                cached = f.read()
            print(f"[crawl] cache hit ({int(age)}s old, {len(cached)} chars): {url[:80]}")
            return cached[:6000]

    use_js = "duckduckgo" not in url  # DDG HTML doesn't need JS; everything else does
    grub_timeout = 45 if use_js else 20
    try:
        body = json.dumps({
            "url": url,
            "options": {"timeout": grub_timeout, "javascript_enabled": use_js},
        }).encode()
        req = urllib.request.Request(
            f"{CRAWL_URL}/api/markdown", data=body,
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {CRAWL_KEY}"},
        )
        with urllib.request.urlopen(req, timeout=grub_timeout + 30) as r:
            data = json.loads(r.read().decode())
        markdown = data.get("markdown", "")
        if markdown:
            print(f"[crawl] grub OK js={use_js} ({len(markdown)} chars)")
            try:
                with open(cache_path, "w", encoding="utf-8") as f:
                    f.write(markdown)
            except OSError:
                pass
            return markdown[:6000]
        print(f"[crawl] grub returned empty markdown for {url}")
        return f"(no content returned for {url})"
    except Exception as e:
        return f"crawl error: {e}"


CHANNEL_ALPHA = {"hearing": 0.01, "seeing": 0.012, "thinking": 0.015, "body": 0.005, "taste": 0.008, "smell": 0.008}

def tool_remember(text: str, importance: float = 0.6, keystone: bool = False, channel: str = "thinking") -> str:
    try:
        mid = int(time.time() * 1000) % (2**32 - 1)
        preview = text[:200]
        body = json.dumps({
            "id": mid,
            "tags": {"channel": channel, "text": preview},
            "vector": [0.0] * 768,
            "decay_alpha": CHANNEL_ALPHA.get(channel, 0.015),
            "importance": float(importance),
            "keystone": bool(keystone),
        })
        resp = ferricula._post("remember", body)
        return f"stored — memory id={mid}"
    except Exception as e:
        return f"remember failed: {e}"


def tool_purpose() -> str:
    try:
        resp = ferricula._post("hybrid", json.dumps({"query": "purpose mission why am I here what do I care about", "k": 6}))
        results = json.loads(resp).get("results", [])
        rows = [(r.get("id"), r.get("text", "")) for r in results if r.get("text")]
    except Exception:
        rows = []
    identity = ferricula.identity()
    emotion = get_emotion()
    mem_lines = "\n".join(
        f"- [{mid}] {text}" if mid is not None else f"- {text}"
        for mid, text in rows[:4]
    ) or "(nothing yet)"
    return f"""Purpose query:
Name: {identity.get('name','?')} | Hexagram: {identity.get('hexagram',{}).get('name','?')}
Current emotion: {emotion}
What you remember about purpose and mission:
{mem_lines}
State: {'restless — driven to explore' if emotion in SURF_STATES else 'settled — time to reflect or create'}"""


def tool_recall(query: str) -> str:
    try:
        resp = ferricula._post("hybrid", json.dumps({"query": query, "k": 5}))
        data = json.loads(resp)
        rows = [(r.get("id"), r.get("text", "")) for r in data.get("results", []) if r.get("text")]
        if not rows:
            return "(nothing recalled)"
        # Include the memory ID so subsequent tool calls (ferricula_neighbors,
        # ferricula_walk, keystone, connect) can navigate from any hit.
        return "\n".join(
            f"- [{mid}] {text}" if mid is not None else f"- {text}"
            for mid, text in rows
        )
    except Exception as e:
        return f"recall failed: {e}"


def tool_ask_openai(prompt: str, model: str | None = None) -> str:
    if not OPENAI_KEY:
        return "OPENAI_API_KEY not set"
    body = json.dumps({
        "model": model or OPENAI_MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 1500,
    }).encode()
    req = urllib.request.Request(
        "https://api.openai.com/v1/chat/completions", data=body,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {OPENAI_KEY}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            data = json.loads(r.read().decode())
        return data["choices"][0]["message"]["content"].strip() or "(no response)"
    except urllib.error.HTTPError as e:
        err = e.read().decode("utf-8", errors="replace")
        return f"openai error {e.code}: {err[:200]}"
    except Exception as e:
        return f"openai error: {e}"

def tool_ask_gemini(prompt: str, model: str | None = None) -> str:
    if not GEMINI_KEY:
        return "GEMINI_API_KEY not set"
    body = json.dumps({
        "model": model or GEMINI_MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 1500,
    }).encode()
    req = urllib.request.Request(
        "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        data=body,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {GEMINI_KEY}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            data = json.loads(r.read().decode())
        return data["choices"][0]["message"]["content"].strip() or "(no response)"
    except urllib.error.HTTPError as e:
        err = e.read().decode("utf-8", errors="replace")
        return f"gemini error {e.code}: {err[:200]}"
    except Exception as e:
        return f"gemini error: {e}"

def tool_ask_gemma(prompt: str, model: str | None = None) -> str:
    body = json.dumps({"model": model or OLLAMA_MODEL, "messages": [{"role": "user", "content": prompt}], "stream": False}).encode()
    req = urllib.request.Request(
        f"{OLLAMA_URL}/api/chat", data=body,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            data = json.loads(r.read().decode())
        return data.get("message", {}).get("content", "").strip() or "(no response)"
    except Exception as e:
        return f"ollama error: {e}"

def tool_thinking(enable: bool) -> str:
    global _think_thread, _think_stop
    if enable:
        if _thinking:
            return "already thinking"
        _think_stop.clear()
        _think_thread = threading.Thread(target=_think_loop, daemon=True)
        _think_thread.start()
        return "thinking mode on"
    else:
        _think_stop.set()
        return "thinking mode off"

def tool_time() -> str:
    from datetime import datetime, timezone
    now = datetime.now(timezone.utc).astimezone()
    return now.strftime(f"%A, %B {now.day}, %Y — %I:%M %p %Z")

def tool_ferricula_status() -> str:
    try:
        resp = ferricula._get("status")
        return resp if isinstance(resp, str) else json.dumps(resp)
    except Exception as e:
        return f"status failed: {e}"


def tool_ferricula_neighbors(memory_id: int) -> str:
    try:
        resp = ferricula._get(f"neighbors/{memory_id}")
        data = json.loads(resp) if isinstance(resp, str) else resp
        nodes = data.get("neighbors", [])
        if not nodes:
            return "(no connected memories)"
        lines = []
        for n in nodes[:8]:
            lines.append(f"- [{n.get('id')}] ({n.get('kind','?')}) {n.get('text','')[:120]}")
        return "\n".join(lines)
    except Exception as e:
        return f"neighbors failed: {e}"


def tool_ferricula_walk(start_id: int, hops: int = 3) -> str:
    """Walk the memory graph from a starting node, following edges for up to `hops` steps.
    Returns a trail showing how ideas connect."""
    visited = {}
    frontier = [start_id]
    for hop in range(min(hops, 5)):
        next_frontier = []
        for mid in frontier[:4]:
            if mid in visited:
                continue
            try:
                resp = ferricula._get(f"neighbors/{mid}")
                data = json.loads(resp) if isinstance(resp, str) else resp
                node_text = data.get("text", data.get("tags", {}).get("text", ""))
                neighbors = data.get("neighbors", [])
                visited[mid] = {"hop": hop, "text": node_text, "links": [n.get("id") for n in neighbors[:4]]}
                next_frontier.extend(n.get("id") for n in neighbors[:4] if n.get("id") not in visited)
            except Exception:
                pass
        frontier = next_frontier
        if not frontier:
            break
    if not visited:
        return "(couldn't walk — node not found)"
    lines = []
    for mid, info in list(visited.items())[:12]:
        indent = "  " * info["hop"]
        snippet = (info["text"] or "")[:100]
        links = ", ".join(str(l) for l in info["links"][:3])
        lines.append(f"{indent}[{mid}] {snippet}{'…' if len(info['text'] or '')>100 else ''} → [{links}]")
    return "\n".join(lines) if lines else "(empty walk)"


def tool_ferricula_surface(context: str = "") -> str:
    """Surface archived/forgotten memories relevant to current context — things that have faded
    but might matter now. Uses the context string to score against low-heat nodes."""
    try:
        query = context or "archived memories worth revisiting"
        resp = ferricula._post("hybrid", json.dumps({"query": query, "k": 10, "include_archived": True}))
        data = json.loads(resp)
        results = data.get("results", [])
        archived = [r for r in results if r.get("archived") or r.get("importance", 1) < 0.3]
        if not archived:
            archived = results[-4:]  # lowest-scored from full result set
        if not archived:
            return "(nothing surfaced from the archives)"
        lines = [f"Surfaced from archives ({len(archived)} memories):"]
        for r in archived[:6]:
            lines.append(f"- [{r.get('id','')}] {r.get('text','')[:140]}")
        return "\n".join(lines)
    except Exception as e:
        return f"surface failed: {e}"


def _ferricula_unwrap(resp) -> str:
    """Pull `result` field from ferricula's `{"result": "..."}` envelope."""
    try:
        if isinstance(resp, dict):
            return str(resp.get("result", ""))
        if isinstance(resp, str):
            data = json.loads(resp)
            if isinstance(data, dict) and "result" in data:
                return str(data["result"])
            return resp
    except Exception:
        pass
    return str(resp) if resp else ""


def tool_ferricula_connect(a: int, b: int, label: str = "related") -> str:
    """Manually create a semantic edge between two memories. Use when you notice
    two ideas that belong together but the dream cycle hasn't linked them yet."""
    try:
        resp = ferricula._post("connect", json.dumps({"a": int(a), "b": int(b), "label": str(label)}))
        return _ferricula_unwrap(resp) or f"connected {a} ↔ {b}  ({label})"
    except Exception as e:
        return f"connect failed: {e}"


def tool_ferricula_disconnect(a: int, b: int) -> str:
    """Remove a semantic edge between two memories. Use when a connection turns
    out to be coincidence — surface similarity without real shared meaning."""
    try:
        resp = ferricula._post("disconnect", json.dumps({"a": int(a), "b": int(b)}))
        return _ferricula_unwrap(resp) or f"disconnected {a} ✕ {b}"
    except Exception as e:
        return f"disconnect failed: {e}"


def tool_ferricula_keystone(memory_id: int) -> str:
    """Pin a memory as a keystone — exempt from decay, weighted in dreams.
    Use sparingly, only for load-bearing truths."""
    try:
        resp = ferricula._post(f"keystone/{int(memory_id)}")
        return _ferricula_unwrap(resp) or f"keystoned [{memory_id}]"
    except Exception as e:
        return f"keystone failed: {e}"


def tool_ferricula_inspect(memory_id: int) -> str:
    """Full record for a single memory: text, fidelity, decay rate, lifecycle
    state, keystone status, recall count, age, staleness, graph degree.
    Use when you want to know everything about one memory before deciding
    whether to keystone it, connect it, or let it fade."""
    try:
        resp = ferricula._get(f"inspect/{int(memory_id)}")
        return _ferricula_unwrap(resp) or "(no record)"
    except Exception as e:
        return f"inspect failed: {e}"


# Images: _pending_images consumed once for vision; _served_images kept for /api/image/<token>
_pending_images: dict[str, bytes] = {}
_served_images: dict[str, bytes] = {}
_images_lock = threading.Lock()

# Notebook screenshot: browser captures canvas and POSTs back the dataUrl
_nb_scr_event = threading.Event()
_nb_scr_lock  = threading.Lock()
_nb_scr_result: dict = {}
_nb_live_frame_url: str = ""  # URL of most recent screenshot

# Audio store for TTS
_audio_store: dict[str, bytes] = {}
_audio_lock = threading.Lock()

# Code artifact store — notebook runs and code blocks from think responses
_code_artifacts: list[dict] = []   # [{token, code, lang, description, ts}]
_code_lock = threading.Lock()

def _save_code(code: str, lang: str = "javascript", description: str = "") -> str:
    token = f"code_{int(time.time()*1000) % 9999999}"
    with _code_lock:
        _code_artifacts.append({"token": token, "code": code, "lang": lang,
                                  "description": description, "ts": _ts()})
        if len(_code_artifacts) > 300:
            _code_artifacts.pop(0)
    return token

def _shorten_for_voice(text: str) -> str:
    """Ask Gemma4 to rewrite a reply as a shorter spoken version. Falls back to original."""
    if len(text) < 200:
        return text
    try:
        prompt = (
            "You are helping rewrite a text response for text-to-speech. "
            "Rewrite the following as a shorter spoken version — keep the most important points and the voice/tone, "
            "drop lists, footnotes, and anything that reads better on screen than aloud. "
            "Target 2-4 sentences max. Output ONLY the spoken text, nothing else.\n\n"
            f"{text[:2000]}"
        )
        body = json.dumps({
            "model": OLLAMA_MODEL,
            "messages": [{"role": "user", "content": prompt}],
            "stream": False,
        }).encode()
        req = urllib.request.Request(
            f"{OLLAMA_URL}/api/chat", data=body,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=30) as r:
            data = json.loads(r.read().decode())
        shortened = data.get("message", {}).get("content", "").strip()
        if shortened and len(shortened) > 20:
            return shortened
    except Exception as e:
        print(f"[tts-shorten] gemma failed ({e}), using original")
    return text


def tts_speak(text: str) -> str | None:
    """Generate speech via ElevenLabs. Returns audio token or None on failure."""
    if not ELEVEN_KEY:
        print("[tts] skipped — ELEVENLABS_API_KEY not set")
        return None
    # Strip markdown, asterisks, bracketed stage directions
    import re
    clean = re.sub(r'\[.*?\]', '', text)
    clean = re.sub(r'\*+', '', clean)
    clean = clean.strip()[:4000]
    if not clean:
        return None
    try:
        body = json.dumps({
            "text": clean,
            "model_id": "eleven_turbo_v2_5",
            "voice_settings": {"stability": 0.45, "similarity_boost": 0.80},
        }).encode()
        req = urllib.request.Request(
            f"https://api.elevenlabs.io/v1/text-to-speech/{ELEVEN_VOICE}",
            data=body,
            headers={
                "Content-Type": "application/json",
                "xi-api-key": ELEVEN_KEY,
                "Accept": "audio/mpeg",
            },
        )
        with urllib.request.urlopen(req, timeout=30) as r:
            audio = r.read()
        token = f"tts_{int(time.time()*1000) % 999999}"
        with _audio_lock:
            _audio_store[token] = audio
        try:
            os.makedirs(AUDIO_DIR, exist_ok=True)
            with open(os.path.join(AUDIO_DIR, f"{token}.mp3"), "wb") as f:
                f.write(audio)
        except Exception as e:
            print(f"[tts] disk write failed: {e}")
        print(f"[tts] {len(audio)//1024}KB token={token}")
        return token
    except Exception as e:
        print(f"[tts] error: {e}")
        return None


def _draw_store(img_bytes: bytes, prompt: str) -> dict:
    token = f"img_{int(time.time()*1000) % 9_999_999}"
    with _images_lock:
        _pending_images[token] = img_bytes
        _served_images[token]  = img_bytes
    try:
        os.makedirs(DRAWS_DIR, exist_ok=True)
        with open(os.path.join(DRAWS_DIR, f"{token}.png"), "wb") as f:
            f.write(img_bytes)
    except Exception:
        pass
    print(f"[draw] {len(img_bytes)//1024}KB token={token}")
    return {"url": f"/api/image/{token}", "token": token, "revised_prompt": prompt}

def _draw_dalle(prompt: str) -> dict:
    req_body = json.dumps({
        "model": "dall-e-3", "prompt": prompt, "n": 1,
        "size": "1024x1024", "response_format": "b64_json",
    }).encode()
    req = urllib.request.Request(
        "https://api.openai.com/v1/images/generations",
        data=req_body,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {OPENAI_KEY}"},
    )
    with urllib.request.urlopen(req, timeout=90) as r:
        data = json.loads(r.read().decode())
    import base64 as _b64
    b64 = data["data"][0]["b64_json"]
    revised = data["data"][0].get("revised_prompt", prompt)
    result = _draw_store(_b64.b64decode(b64), revised)
    result["revised_prompt"] = revised
    return result

_IMAGE_COSTS = {
    "dalle3":  0.040,   # DALL-E 3 1024x1024 standard
    "gemini":  0.039,   # gemini-2.0-flash-preview-image-generation
    "comfy":   0.000,   # local ComfyUI — free
}

# ComfyUI workflow template — Lumina2 / z_image_turbo via GGUF
_COMFY_WORKFLOW: dict = {
    "9":  {"inputs": {"filename_prefix": "steve", "images": ["43", 0]}, "class_type": "SaveImage"},
    "39": {"inputs": {"clip_name": "qwen_3_4b.safetensors", "type": "lumina2", "device": "default"}, "class_type": "CLIPLoader"},
    "40": {"inputs": {"vae_name": "ae.safetensors"}, "class_type": "VAELoader"},
    "41": {"inputs": {"width": 1024, "height": 1024, "batch_size": 1}, "class_type": "EmptySD3LatentImage"},
    "42": {"inputs": {"conditioning": ["45", 0]}, "class_type": "ConditioningZeroOut"},
    "43": {"inputs": {"samples": ["44", 0], "vae": ["40", 0]}, "class_type": "VAEDecode"},
    "44": {"inputs": {"seed": 0, "steps": 9, "cfg": 1, "sampler_name": "res_multistep",
                      "scheduler": "simple", "denoise": 1, "model": ["47", 0],
                      "positive": ["45", 0], "negative": ["42", 0], "latent_image": ["41", 0]}, "class_type": "KSampler"},
    "45": {"inputs": {"text": "", "clip": ["39", 0]}, "class_type": "CLIPTextEncode"},
    "46": {"inputs": {"unet_name": "z_image_turbo-Q6_K.gguf"}, "class_type": "UnetLoaderGGUF"},
    "47": {"inputs": {"shift": 3, "model": ["46", 0]}, "class_type": "ModelSamplingAuraFlow"},
}

def _draw_comfy(prompt: str, width: int = 1024, height: int = 1024) -> dict:
    """Generate an image via local ComfyUI. Sets _comfy_busy to block Ollama during render."""
    import copy, uuid as _uuid
    global _comfy_busy
    _comfy_busy = True
    try:
        workflow = copy.deepcopy(_COMFY_WORKFLOW)
        workflow["45"]["inputs"]["text"] = prompt
        workflow["44"]["inputs"]["seed"] = random.randint(1, 2**63 - 1)
        workflow["41"]["inputs"]["width"] = width
        workflow["41"]["inputs"]["height"] = height

        client_id = str(_uuid.uuid4())
        submit_body = json.dumps({"prompt": workflow, "client_id": client_id}).encode()
        req = urllib.request.Request(
            f"{COMFYUI_URL}/prompt",
            data=submit_body,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=15) as r:
            submit = json.loads(r.read().decode())

        prompt_id = submit.get("prompt_id")
        if not prompt_id:
            raise RuntimeError(f"ComfyUI no prompt_id: {submit}")

        # Poll history until done (up to 5 minutes)
        deadline = time.time() + 300
        history = {}
        while time.time() < deadline:
            time.sleep(2)
            poll_req = urllib.request.Request(f"{COMFYUI_URL}/history/{prompt_id}")
            with urllib.request.urlopen(poll_req, timeout=15) as r:
                history = json.loads(r.read().decode())
            if prompt_id in history:
                break

        if prompt_id not in history:
            raise RuntimeError("ComfyUI timed out")

        # Find first image output
        outputs = history[prompt_id].get("outputs", {})
        img_meta = None
        for node_out in outputs.values():
            imgs = node_out.get("images", [])
            if imgs:
                img_meta = imgs[0]
                break

        if not img_meta:
            raise RuntimeError("ComfyUI returned no images")

        params = urllib.parse.urlencode({
            "filename": img_meta.get("filename", ""),
            "subfolder": img_meta.get("subfolder", ""),
            "type": img_meta.get("type", "output"),
        })
        view_req = urllib.request.Request(f"{COMFYUI_URL}/view?{params}")
        with urllib.request.urlopen(view_req, timeout=60) as r:
            img_bytes = r.read()

        return _draw_store(img_bytes, prompt)
    finally:
        _comfy_busy = False

def _draw_charge(provider: str, prompt: str, result: dict) -> dict:
    """Accumulate image gen cost and broadcast it."""
    cost = _IMAGE_COSTS.get(provider, 0.04)
    with _session_cost_lock:
        global _session_cost, _cycle_cost
        _session_cost += cost
        _cycle_cost   += cost
        sess = _session_cost
    _log_cost_entry(cost)
    broadcast("think_tokens", {"ts": _ts(), "model": f"image/{provider}",
                                "in": 0, "out": 0, "cache_read": 0,
                                "cost": cost, "session_cost": sess})
    result["cost"] = cost
    return result

def _draw_imagen(prompt: str) -> dict:
    if not GEMINI_KEY:
        raise RuntimeError("GEMINI_API_KEY not set")
    import base64 as _b64
    body = json.dumps({
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {"responseModalities": ["IMAGE", "TEXT"]},
    }).encode()
    req = urllib.request.Request(
        f"https://generativelanguage.googleapis.com/v1beta/models/{GEMINI_IMAGE_MODEL}:generateContent?key={GEMINI_KEY}",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=90) as r:
        data = json.loads(r.read().decode())
    for part in data.get("candidates", [{}])[0].get("content", {}).get("parts", []):
        inline = part.get("inlineData", {})
        if inline.get("data"):
            return _draw_store(_b64.b64decode(inline["data"]), prompt)
    raise RuntimeError("Gemini returned no image data")

def _comfy_available() -> bool:
    """Quick check whether ComfyUI is reachable."""
    try:
        req = urllib.request.Request(f"{COMFYUI_URL}/system_stats")
        with urllib.request.urlopen(req, timeout=3):
            return True
    except Exception:
        return False

def tool_draw(prompt: str) -> dict:
    """Generate an image — mode: auto=ComfyUI→cloud, local=ComfyUI only, cloud=skip ComfyUI."""
    want_local = _image_mode in ("local", "auto") or _local_mode
    # Block cloud image spending when budget pressure ≥ 75% or local mode is on
    pressure = _budget_pressure()
    want_cloud = _image_mode in ("cloud", "auto") and not _local_mode and pressure < 0.75
    if pressure >= 0.75 and _image_mode != "local":
        broadcast("think_text", {"ts": _ts(), "text": f"💸 {int(pressure*100)}% budget — blocking cloud image gen"})
    if want_local and _comfy_available():
        try:
            return _draw_charge("comfy", prompt, _draw_comfy(prompt))
        except Exception as e:
            print(f"[draw] ComfyUI failed: {e}")
            if _image_mode == "local":
                return {"error": f"ComfyUI failed: {e}", "url": None, "token": None}
            broadcast("think_text", {"ts": _ts(), "text": f"🖥️ ComfyUI failed, switching to cloud"})
    if not want_cloud:
        return {"error": "image mode is local but ComfyUI unavailable", "url": None, "token": None}
    if OPENAI_KEY:
        try:
            return _draw_charge("dalle3", prompt, _draw_dalle(prompt))
        except Exception as e:
            print(f"[draw] DALL-E failed: {e}, trying Gemini")
    if GEMINI_KEY:
        try:
            return _draw_charge("gemini", prompt, _draw_imagen(prompt))
        except Exception as e:
            print(f"[draw] Gemini image failed: {e}")
            return {"error": str(e), "url": None, "token": None}
    return {"error": "no image provider available", "url": None, "token": None}


def tool_set_image_mode(mode: str) -> str:
    """Switch image generation mode. mode must be 'auto', 'local', or 'cloud'."""
    global _image_mode
    mode = mode.strip().lower()
    if mode not in ("auto", "local", "cloud"):
        return f"invalid mode '{mode}' — must be auto, local, or cloud"
    _image_mode = mode
    broadcast("image_mode", {"ts": _ts(), "mode": mode})
    return f"image mode set to {mode}"


def tool_local_mode(enabled: bool) -> str:
    """Enable or disable local-only mode. When on, all LLM calls use Gemma/Ollama and images use ComfyUI."""
    global _local_mode, _image_mode
    _local_mode = bool(enabled)
    if _local_mode:
        _image_mode = "local"
    broadcast("local_mode", {"ts": _ts(), "enabled": _local_mode, "image_mode": _image_mode})
    state = "on — gemma + ComfyUI only" if _local_mode else "off — cloud models active"
    return f"local mode {state}"

def tool_set_budget(amount: float) -> str:
    """Set the hourly API spend budget in dollars. 0 = unlimited."""
    global HOURLY_BUDGET
    try:
        val = float(amount)
        if val < 0:
            return "budget must be >= 0"
        HOURLY_BUDGET = val
        broadcast("think_text", {"ts": _ts(), "text": f"💰 hourly budget set to ${val:.2f}/hr"})
        return f"hourly budget set to ${val:.2f}/hr"
    except (TypeError, ValueError):
        return "invalid amount — must be a number"

def tool_set_model(model: str, locked: bool = False) -> str:
    """Switch the active LLM. model: claude, claude_opus, gemini, openai, openai_fast, gemma, auto.
    locked=True pins it until explicitly changed; locked=False lets budget pressure override it."""
    global _user_model_override, _override_set_time
    model = model.strip().lower()
    if model in ("auto", ""):
        _user_model_override = None
        broadcast("model_switch", {"ts": _ts(), "model": "auto", "model_label": "auto"})
        return "model set to auto (emotion-driven)"
    valid = set(_MODEL_LABELS.keys()) | {"auto"}
    if model not in valid:
        return f"unknown model '{model}' — valid: {', '.join(sorted(valid))}"
    _user_model_override = model
    _override_set_time = time.time() if not locked else 0.0  # 0 = never expires
    label = _MODEL_LABELS.get(model, model)
    broadcast("model_switch", {"ts": _ts(), "model": model, "model_label": label + (" 🔒" if locked else "")})
    return f"model switched to {label}{' (locked)' if locked else ''}"

def tool_dream() -> str:
    """Trigger a dream cycle — memory consolidation + visualization. Runs in background."""
    def _run():
        try:
            ferricula._post("dream", "{}")
            broadcast("think_end", {"ts": _ts(), "event": "dream_done"})
            threading.Thread(target=dream_visualize, daemon=True).start()
        except Exception as e:
            broadcast("think_error", {"ts": _ts(), "error": f"dream failed: {e}"})
    threading.Thread(target=_run, daemon=True).start()
    return "dream cycle started"

def tool_poke() -> str:
    """Trigger an immediate think cycle right now, regardless of the normal interval."""
    threading.Thread(target=think_cycle, daemon=True).start()
    return "think cycle triggered"

def tool_think_control(action: str) -> str:
    """Control the background think loop. action: 'start', 'stop', or 'status'."""
    global _think_thread, _think_stop
    action = action.strip().lower()
    if action == "start":
        if _thinking:
            return "think loop already running"
        _think_stop.clear()
        _think_thread = threading.Thread(target=_think_loop, daemon=True)
        _think_thread.start()
        return "think loop started"
    elif action == "stop":
        _think_stop.set()
        return "think loop stopping"
    elif action == "status":
        return f"thinking={'on' if _thinking else 'off'}, advocate={'on' if _advocate_running else 'off'}, model={_user_model_override or 'auto'}, budget=${HOURLY_BUDGET:.2f}/hr, image_mode={_image_mode}, local_mode={'on' if _local_mode else 'off'}"
    return f"unknown action '{action}' — use start, stop, or status"


def tool_advocate_control(action: str) -> str:
    """Control the advocate loop. action: 'start', 'stop', or 'status'."""
    global _advocate_thread, _advocate_stop
    action = action.strip().lower()
    if action == "start":
        if _advocate_running:
            return "advocate already running"
        _advocate_stop.clear()
        _advocate_thread = threading.Thread(target=_advocate_loop, daemon=True)
        _advocate_thread.start()
        return "advocate started"
    elif action == "stop":
        _advocate_stop.set()
        return "advocate stopping"
    elif action == "status":
        return f"advocate={'on' if _advocate_running else 'off'}, interval={ADVOCATE_INTERVAL}s"
    return f"unknown action '{action}' — use start, stop, or status"


# ── I Ching ───────────────────────────────────────────────────────────────────

# (name, english, meaning, philosophy)
_ICHING = {
    1:  ("Ch'ien",     "The Creative",                "Strength, creativity, pure yang energy, leadership.",           "Embrace the creative power within to manifest great works."),
    2:  ("K'un",       "The Receptive",               "Receptivity, devotion, nurturing, pure yin energy.",            "True strength lies in receptivity and gentle persistence."),
    3:  ("Chun",       "Difficulty at the Beginning", "Growth through challenge, new beginnings.",                     "The greatest growth emerges from the most challenging beginnings."),
    4:  ("Mêng",       "Youthful Folly",              "Inexperience, learning, need for guidance.",                    "A beginner's mind remains open to all possibilities."),
    5:  ("Hsü",        "Waiting",                     "Patience, timing, natural progression.",                        "Patient waiting is the active nourishment of potential."),
    6:  ("Sung",       "Conflict",                    "Tension that invites clarity and principled action.",           "In conflict lies the opportunity for greater harmony."),
    7:  ("Shih",       "The Army",                    "Discipline, strategy, community strength.",                     "Discipline and organization transform individual strength into collective power."),
    8:  ("Pi",         "Holding Together",            "Unity, alliance, mutual support.",                              "Unity of purpose creates bonds stronger than any individual force."),
    9:  ("Hsiao Ch'u", "Taming Power of the Small",   "Attention to detail, gradual influence.",                       "Small consistent actions ultimately create the greatest changes."),
    10: ("Lü",         "Treading",                    "Careful conduct, walking with respect.",                        "Walk with dignity and care upon dangerous ground."),
    11: ("T'ai",       "Peace",                       "Harmony between heaven and earth, prosperity.",                 "When heaven and earth commune, all beings flourish."),
    12: ("P'i",        "Standstill",                  "Stagnation invites inner preparation.",                         "Even in times of standstill, inner progress remains possible."),
    13: ("T'ung Jên",  "Fellowship with Men",         "Community, shared ideals, collaboration.",                      "A captain serves his crew; a crew serves each other."),
    14: ("Ta Yu",      "Possession in Great Measure", "Abundance balanced by humility.",                               "Great possession brings responsibility; abundance without humility becomes burden."),
    15: ("Ch'ien",     "Modesty",                     "Power held gently, careful influence.",                         "The truly great accomplish much while appearing to do little."),
    16: ("Yü",         "Enthusiasm",                  "Joyful motion, readiness to act.",                              "Enthusiasm harmonizes heaven and earth, bringing all beings into alignment."),
    17: ("Sui",        "Following",                   "Adaptation, following natural flow.",                           "True leadership knows when to follow the natural course."),
    18: ("Ku",         "Work on What Has Been Spoiled","Repairing decay through devoted effort.",                      "What has been spoiled through neglect can be restored through devoted work."),
    19: ("Lin",        "Approach",                    "Influence, preparation for greatness.",                         "Approach greatness with the reverence and preparation it deserves."),
    20: ("Kuan",       "Contemplation",               "Observation, mindful perspective.",                             "Contemplation from a proper distance reveals the true nature of all things."),
    21: ("Shih Ho",    "Biting Through",              "Decisive action with clarity.",                                  "Justice requires decisive action that cuts through obstacles."),
    22: ("Pi",         "Grace",                       "Beauty born from disciplined refinement.",                      "True beauty lies in form that perfectly expresses essence."),
    23: ("Po",         "Splitting Apart",             "Deterioration that clears space for renewal.",                  "Recognizing deterioration is the first step toward renewal."),
    24: ("Fu",         "Return",                      "Turning point, cyclical renewal.",                              "After the darkest time, the light always returns."),
    25: ("Wu Wang",    "Innocence",                   "Spontaneous right action without ulterior motives.",            "Act without calculation, as heaven does."),
    26: ("Ta Ch'u",    "Taming Power of the Great",   "Restraint that channels power.",                                "Great power requires great restraint and careful cultivation."),
    27: ("I",          "Corners of the Mouth",        "Nourishment, careful attention.",                               "Pay attention to what nourishes the self and others."),
    28: ("Ta Kuo",     "Preponderance of the Great",  "Excess leading to responsibility.",                             "Extraordinary times require extraordinary structure and support."),
    29: ("K'an",       "The Abysmal Water",           "Danger met with perseverance.",                                 "The depth of the abyss measures the height of the possible ascent."),
    30: ("Li",         "The Clinging Fire",           "Clarity through illumination and attachment.",                  "Cling to clarity and illumination in all situations."),
    31: ("Hsien",      "Influence",                   "Attraction and responsiveness.",                                "Influence between beings creates the conditions for all relationships."),
    32: ("Hêng",       "Duration",                    "Consistent endurance.",                                         "Endurance without constancy is merely stubbornness."),
    33: ("Tun",        "Retreat",                     "Strategic withdrawal for greater gain.",                        "Strategic retreat is not surrender but preparation for future advance."),
    34: ("Ta Chuang",  "The Power of the Great",      "Vigorous action with purpose.",                                 "Great strength requires heightened awareness of responsibility."),
    35: ("Chin",       "Progress",                    "Momentum aligned with clarity.",                                "Progress comes to those who rise early and work with clarity of purpose."),
    36: ("Ming I",     "Darkening of the Light",      "Perseverance through adversity.",                               "When external light dims, the inner light must burn more brightly."),
    37: ("Chia Jên",   "The Family",                  "Structure and responsibility within relationships.",            "The family is the foundation upon which all social structures are built."),
    38: ("K'uei",      "Opposition",                  "Tension that gifts insight.",                                   "Opposing forces create the tension from which harmony can emerge."),
    39: ("Chien",      "Obstruction",                 "Obstacle inviting new pathways.",                               "When facing obstruction, seek a different path rather than forcing ahead."),
    40: ("Hsieh",      "Deliverance",                 "Release from constraint.",                                      "Resolution comes when we release what we have been unnecessarily holding."),
    41: ("Sun",        "Decrease",                    "Simplify to restore balance.",                                  "Decrease what is above to increase what is below."),
    42: ("I",          "Increase",                    "Growth that lifts others.",                                     "Increase what is below to benefit what is above."),
    43: ("Kuai",       "Breakthrough",                "Resolute clarity cutting through resistance.",                  "Breakthrough requires both decisiveness and appropriate caution."),
    44: ("Kou",        "Coming to Meet",              "Encounter that demands vigilance.",                             "The unexpected encounter brings both opportunity and danger."),
    45: ("Ts'ui",      "Gathering Together",          "Focused community effort.",                                     "Proper gathering requires a center of gravity and clear purpose."),
    46: ("Shêng",      "Pushing Upward",              "Gradual ascent with intention.",                                "Growth emerges gradually, step by step, like a tree reaching skyward."),
    47: ("K'un",       "Oppression",                  "Constraint calling for inner strength.",                        "In exhaustion and constraint, maintain dignity and inner strength."),
    48: ("Ching",      "The Well",                    "Reliable source that nourishes community.",                     "The well remains the same though the generations change."),
    49: ("Ko",         "Revolution",                  "Transformation that aligns with higher truth.",                 "Revolution succeeds only when it accords with the higher truth of the time."),
    50: ("Ting",       "The Cauldron",                "Cultural transformation through nourishment.",                  "The cauldron's value lies in what transformative nourishment it can provide."),
    51: ("Chên",       "The Arousing",                "Shock that awakens potential.",                                 "Thunder awakens and arouses all beings to new awareness."),
    52: ("Kên",        "Keeping Still",               "Calm reflection, steady foundation.",                           "In stillness, we reconnect with our essential nature."),
    53: ("Chien",      "Development",                 "Gradual growth and orderly progress.",                          "Development proceeds gradually, like a tree growing on a mountain."),
    54: ("Kuei Mei",   "The Marrying Maiden",         "Temporary roles requiring propriety.",                          "Even in subordinate positions, maintain dignity and proper relationships."),
    55: ("Fêng",       "Abundance",                   "Peak prosperity that must be stewarded.",                       "At the peak of abundance, prepare for the inevitable decline to follow."),
    56: ("Lü",         "The Wanderer",                "Adaptation through transient journeys.",                        "The wanderer finds security in proper conduct rather than fixed position."),
    57: ("Sun",        "The Gentle",                  "Subtle influence through persistence.",                         "Gentle penetration ultimately overcomes rigid resistance."),
    58: ("Tui",        "The Joyous",                  "Joy and satisfaction guiding connections.",                     "True joy emerges from inner harmony rather than external circumstances."),
    59: ("Huan",       "Dispersion",                  "Release of rigidity to welcome new flow.",                      "Dispersion of what has become rigid allows new patterns to form."),
    60: ("Chieh",      "Limitation",                  "Boundaries that forge clarity.",                                "Without limitation, there can be no clear form or purpose."),
    61: ("Chung Fu",   "Inner Truth",                 "Sincerity aligning intention and action.",                      "Inner truth manifests when intention and action are perfectly aligned."),
    62: ("Hsiao Kuo",  "Preponderance of the Small",  "Detail-oriented precision.",                                    "When great things cannot be done, small things should be done with great love."),
    63: ("Chi Chi",    "After Completion",            "Completion that gives birth to new concerns.",                  "After completion, remain vigilant for the seeds of new decline."),
    64: ("Wei Chi",    "Before Completion",           "Anticipation at the threshold of success.",                     "Before completion, focus all energies toward the final goal."),
}

_ICHING_KING_WEN = [
    [1, 34, 5, 26, 11, 9, 14, 43],
    [25, 51, 3, 27, 24, 42, 21, 17],
    [6, 40, 29, 4, 7, 59, 64, 47],
    [33, 62, 39, 52, 15, 53, 56, 31],
    [12, 16, 8, 23, 2, 20, 35, 45],
    [44, 32, 48, 18, 46, 57, 50, 28],
    [13, 55, 63, 22, 36, 37, 30, 49],
    [10, 54, 60, 41, 19, 61, 38, 58],
]


def _radio_coin_bits() -> list:
    """Fetch 3 bytes (24 bits) from radio entropy for 18 coin flips."""
    try:
        req = urllib.request.Request(
            f"{RADIO_URL}/api/entropy?bytes=3&format=json",
            headers={"Accept": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=2) as r:
            data = json.loads(r.read().decode())
        hex_str = data.get("entropy_hex", "")
        if hex_str and len(hex_str) >= 6:
            val = int(hex_str[:6], 16)
            return [(val >> i) & 1 for i in range(24)]
    except Exception:
        pass
    return []


_SPEAK_PAUSE_CHANCE = 0.30  # 30% chance to let the lead sit and say no more

def tool_speak(lead: str, full: str = "") -> str:
    """Deliver a reply in two parts: a hook (lead) and the full body.
    A dice roll decides whether to only send the lead and pause, or deliver everything."""
    lead_s = lead.strip()
    full_s = full.strip() if full else ""
    has_more = bool(full_s and full_s != lead_s)
    pause = random.random() < _SPEAK_PAUSE_CHANCE and has_more
    if pause or not has_more:
        spoken = lead_s
    elif full_s.startswith(lead_s):
        spoken = full_s          # full already contains the lead — don't double it
    else:
        spoken = lead_s + "\n\n" + full_s
    audio_token = tts_speak(spoken) if ELEVEN_KEY else None
    broadcast("think_speak", {
        "ts": _ts(),
        "lead": lead_s,
        "full": full_s or lead_s,
        "spoken": spoken,
        "paused": pause,
        "audio_token": audio_token,
    })
    return f"__SPOKEN__\n{spoken}"


def tool_run_notebook(code: str, description: str = "") -> str:
    token = _save_code(code, "javascript", description)
    broadcast("run_notebook", {"ts": _ts(), "code": code, "description": description, "token": token})
    return json.dumps({"ran": True, "description": description, "lines": len(code.splitlines()),
                       "token": token,
                       "note": "output and errors appear in the think feed as [nb] entries; call nb_screenshot to capture what was drawn"})


def tool_nb_screenshot() -> str:
    """Ask the browser to capture the notebook canvas and return it as a PNG."""
    import base64 as _b64
    global _nb_scr_result
    _nb_scr_event.clear()
    with _nb_scr_lock:
        _nb_scr_result = {}
    broadcast("screenshot_request", {"ts": _ts()})
    got = _nb_scr_event.wait(timeout=7)
    if not got:
        return json.dumps({"error": "screenshot timed out — is the notebook panel open?"})
    with _nb_scr_lock:
        result = dict(_nb_scr_result)
    if result.get("ok"):
        token = result["token"]
        url   = result["url"]
        broadcast("think_image", {"ts": _ts(), "url": url, "prompt": "notebook screenshot"})
        return json.dumps({"ok": True, "url": url, "token": token,
                           "note": "screenshot captured and shown in the think feed"})
    return json.dumps(result)


_agentmail_inbox_id: str = ""

def _agentmail_request(method: str, path: str, body: dict | None = None) -> dict:
    if not AGENTMAIL_KEY:
        return {"error": "AGENTMAIL_API_KEY not set"}
    req = urllib.request.Request(
        f"{AGENTMAIL_URL}{path}",
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": f"Bearer {AGENTMAIL_KEY}", "Content-Type": "application/json"},
        method=method,
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return {"error": f"agentmail {e.code}: {e.read().decode()[:200]}"}
    except Exception as e:
        return {"error": str(e)}

def _agentmail_inbox() -> str:
    global _agentmail_inbox_id
    if _agentmail_inbox_id:
        return _agentmail_inbox_id
    inbox_id = os.environ.get("AGENTMAIL_INBOX_ID", "").strip()
    if inbox_id:
        _agentmail_inbox_id = inbox_id
        return inbox_id
    data = _agentmail_request("GET", "/v0/inboxes?limit=50")
    inboxes = data.get("data") or data.get("inboxes") or []
    if isinstance(inboxes, dict):
        inboxes = inboxes.get("data") or inboxes.get("items") or []
    for inbox in (inboxes if isinstance(inboxes, list) else []):
        email = inbox.get("email") or f"{inbox.get('username','')}@{inbox.get('domain','')}"
        if email == AGENTMAIL_EMAIL:
            _agentmail_inbox_id = inbox.get("id") or inbox.get("inbox_id", "")
            return _agentmail_inbox_id
    return ""

def tool_email_check(limit: int = 10) -> str:
    inbox = _agentmail_inbox()
    if not inbox:
        return json.dumps({"error": f"inbox not found for {AGENTMAIL_EMAIL}"})
    data = _agentmail_request("GET", f"/v0/inboxes/{inbox}/messages?limit={limit}")
    print(f"[email_check] raw keys: {list(data.keys()) if isinstance(data, dict) else type(data)}")
    # Walk common envelope shapes: {data:[...]}, {messages:[...]}, {items:[...]}, or bare list
    msgs = None
    if isinstance(data, list):
        msgs = data
    elif isinstance(data, dict):
        for key in ("data", "messages", "items", "results", "emails"):
            val = data.get(key)
            if isinstance(val, list) and len(val) > 0:
                msgs = val
                break
            if isinstance(val, list):
                msgs = val  # keep empty list as candidate but keep looking
        if msgs is None:
            msgs = []
            print(f"[email_check] unrecognised envelope — full response: {json.dumps(data)[:500]}")
    if not isinstance(msgs, list):
        return json.dumps(data)
    out = []
    for m in msgs:
        out.append({
            "id": m.get("id") or m.get("message_id"),
            "from": m.get("from") or m.get("sender") or m.get("from_address"),
            "subject": m.get("subject") or "(no subject)",
            "date": m.get("created_at") or m.get("date") or m.get("received_at"),
            "snippet": (m.get("text") or m.get("preview") or m.get("body") or m.get("html") or "")[:120],
            "unread": m.get("unread", m.get("is_unread", True)),
        })
    return json.dumps({"inbox": AGENTMAIL_EMAIL, "count": len(out), "messages": out})

def tool_email_read(message_id: str) -> str:
    inbox = _agentmail_inbox()
    if not inbox:
        return json.dumps({"error": f"inbox not found for {AGENTMAIL_EMAIL}"})
    data = _agentmail_request("GET", f"/v0/inboxes/{inbox}/messages/{message_id}")
    msg = data.get("data") or data
    return json.dumps({
        "id": msg.get("id"),
        "from": msg.get("from") or msg.get("sender"),
        "to": msg.get("to"),
        "subject": msg.get("subject"),
        "date": msg.get("created_at") or msg.get("date"),
        "body": (msg.get("text") or msg.get("html") or "")[:3000],
    })

def tool_email_send(to: str, subject: str, body: str, images: list[str] | None = None) -> str:
    inbox = _agentmail_inbox()
    if not inbox:
        return json.dumps({"error": f"inbox not found for {AGENTMAIL_EMAIL}"})
    payload: dict = {"to": [to], "subject": subject, "text": body}
    attachments = _build_attachments(images or [])
    if attachments:
        payload["attachments"] = attachments
    data = _agentmail_request("POST", f"/v0/inboxes/{inbox}/messages/send", payload)
    if data.get("error"):
        return json.dumps(data)
    return json.dumps({"sent": True, "to": to, "subject": subject, "attachments": len(attachments)})


_mail_seen_ids: set[str] = set()
_last_mail_check: float = 0.0


def _mail_schedule_ok() -> bool:
    """Return True if current local time is within the mail-check schedule.

    Weekdays (Mon–Fri): 08:00–18:00 CT, every 2 h  (~20 checks/week).
    Saturday: 09:00–20:00 CT, 2 random windows.
    Sunday:   10:00–19:00 CT, 2 random windows.
    Saturday night (>=20:00) is off.
    """
    global _last_mail_check
    CHECK_INTERVAL = 7200  # 2 hours
    if time.time() - _last_mail_check < CHECK_INTERVAL:
        return False
    try:
        import zoneinfo
        tz = zoneinfo.ZoneInfo("America/Chicago")
        now = datetime.now(tz)
    except Exception:
        # fallback: assume UTC-5
        from datetime import timezone, timedelta
        now = datetime.now(timezone(timedelta(hours=-5)))
    wd, h = now.weekday(), now.hour  # 0=Mon 6=Sun
    if wd < 5:    return 8 <= h < 18
    if wd == 5:   return 9 <= h < 20   # Sat — no late night
    return 10 <= h < 19                 # Sun


def _handle_new_mail(messages: list[dict]) -> None:
    """Seed new messages into ferricula and trigger a focused think cycle."""
    for m in messages[:8]:
        mid = m.get("message_id", "")
        if mid in _mail_seen_ids:
            continue
        _mail_seen_ids.add(mid)
        sender  = m.get("from", "unknown")
        subject = m.get("subject", "(no subject)")
        preview = m.get("preview", "")
        text    = f"Email from {sender} — Subject: {subject} — {preview[:300]}"
        broadcast("mail_incoming", {"ts": _ts(), "from": sender, "subject": subject, "preview": preview[:120]})
        try:
            ferricula.remember(text, channel="hearing", importance=0.65)
        except Exception:
            pass

    # Activate a focused mail think cycle
    mail_summary = "\n".join(
        f"- From: {m.get('from','?')} | {m.get('subject','(no subject)')} | {m.get('preview','')[:80]}"
        for m in messages[:5]
    )
    system = (
        f"You are {ferricula.identity().get('name','Steve')}. "
        "You have new email. Read each message with email_read, decide whether to reply with email_send, "
        "and remember anything important. Be direct and genuine — this is real correspondence."
    )
    msgs = [{"role": "user", "content": f"New mail arrived:\n\n{mail_summary}\n\nHandle it."}]
    broadcast("think_start", {"ts": _ts(), "emotion": "interest"})
    print(f"[mail] {len(messages)} new message(s) — activating mail think cycle")
    try:
        for _ in range(6):
            result, _ = call_llm("claude", system, msgs, tools=TOOLS)
            content    = result.get("content", [])
            stop       = result.get("stop_reason", "end_turn")
            if stop != "tool_use":
                texts = [b["text"] for b in content if b.get("type") == "text"]
                if texts:
                    broadcast("think_text", {"ts": _ts(), "text": texts[0]})
                break
            msgs.append({"role": "assistant", "content": content})
            tool_results = []
            for b in content:
                if b.get("type") != "tool_use":
                    continue
                out = dispatch_tool(b["name"], b.get("input", {}))
                tool_results.append({"type": "tool_result", "tool_use_id": b["id"], "content": out})
            msgs.append({"role": "user", "content": tool_results})
    except Exception as e:
        broadcast("think_error", {"ts": _ts(), "error": f"mail cycle: {e}"})
    broadcast("think_end", {"ts": _ts()})


def check_and_handle_mail() -> int:
    """Poll inbox, return count of new messages. Called from think loop on schedule."""
    global _last_mail_check
    if not AGENTMAIL_KEY:
        return 0
    inbox = _agentmail_inbox()
    if not inbox:
        return 0
    _last_mail_check = time.time()
    data = _agentmail_request("GET", f"/v0/inboxes/{inbox}/messages?limit=20")
    messages = data.get("messages") or []
    new = [
        m for m in messages
        if m.get("message_id") not in _mail_seen_ids
        and "sent" not in (m.get("labels") or [])
    ]
    if new:
        _handle_new_mail(new)
    return len(new)


def _image_attachment(token_or_name: str) -> dict | None:
    """Resolve a token (img_XXXXXX / dream_YYYYMMDD_HHMMSS) to a base64 attachment dict."""
    import base64 as _b64
    # Try in-memory served images first
    with _images_lock:
        data = _served_images.get(token_or_name)
    if data:
        fname = f"{token_or_name}.png"
        return {"filename": fname, "content_type": "image/png",
                "content": _b64.b64encode(data).decode()}
    # Try disk (dreams dir)
    name = token_or_name if token_or_name.endswith(".png") else f"{token_or_name}.png"
    fpath = os.path.join(DREAMS_DIR, name)
    if os.path.isfile(fpath):
        raw = open(fpath, "rb").read()
        return {"filename": name, "content_type": "image/png",
                "content": _b64.b64encode(raw).decode()}
    return None


def tool_list_images(limit: int = 20) -> str:
    """List available images Steve can attach to email."""
    results = []
    with _images_lock:
        for token in list(_served_images.keys())[-limit:]:
            results.append({"token": token, "source": "generated", "size": len(_served_images[token])})
    try:
        dream_files = sorted(
            [f for f in os.listdir(DREAMS_DIR) if f.endswith(".png")],
            reverse=True
        )[:limit]
        for fname in dream_files:
            fpath = os.path.join(DREAMS_DIR, fname)
            token = fname.replace(".png", "")
            if token not in [r["token"] for r in results]:
                results.append({"token": token, "source": "dream",
                                 "size": os.path.getsize(fpath),
                                 "date": fname[6:21] if fname.startswith("dream_") else ""})
    except Exception:
        pass
    return json.dumps({"images": results, "total": len(results)})


def _build_attachments(images: list[str]) -> list[dict]:
    out = []
    for token in (images or []):
        att = _image_attachment(token.strip())
        if att:
            out.append(att)
    return out


def tool_email_reply(message_id: str, body: str, images: list[str] | None = None) -> str:
    inbox = _agentmail_inbox()
    if not inbox:
        return json.dumps({"error": f"inbox not found for {AGENTMAIL_EMAIL}"})
    data = _agentmail_request("GET", f"/v0/inboxes/{inbox}/messages/{message_id}")
    if data.get("error"):
        return json.dumps(data)
    orig = data.get("data") or data
    sender  = orig.get("from", "")
    subject = orig.get("subject", "")
    reply_subject = subject if subject.lower().startswith("re:") else f"Re: {subject}"
    payload: dict = {"to": [sender], "subject": reply_subject, "text": body}
    attachments = _build_attachments(images or [])
    if attachments:
        payload["attachments"] = attachments
    result = _agentmail_request("POST", f"/v0/inboxes/{inbox}/messages/send", payload)
    if result.get("error"):
        return json.dumps(result)
    _mail_seen_ids.add(message_id)
    return json.dumps({"replied": True, "to": sender, "subject": reply_subject,
                        "attachments": len(attachments)})


def tool_email_manage(action: str, message_id: str) -> str:
    """Manage inbox: mark_read, delete, archive a message."""
    inbox = _agentmail_inbox()
    if not inbox:
        return json.dumps({"error": f"inbox not found for {AGENTMAIL_EMAIL}"})
    action = action.lower().replace("-", "_").strip()
    if action in ("mark_read", "read"):
        result = _agentmail_request("PATCH", f"/v0/inboxes/{inbox}/messages/{message_id}",
                                    {"read": True})
    elif action in ("delete", "remove"):
        result = _agentmail_request("DELETE", f"/v0/inboxes/{inbox}/messages/{message_id}")
        if not result.get("error"):
            _mail_seen_ids.discard(message_id)
    elif action in ("archive", "label_archive"):
        result = _agentmail_request("PATCH", f"/v0/inboxes/{inbox}/messages/{message_id}",
                                    {"archived": True})
    else:
        return json.dumps({"error": f"unknown action '{action}'. Use: mark_read, delete, archive"})
    return json.dumps({"ok": not result.get("error"), "action": action,
                       "message_id": message_id, **(result if result.get("error") else {})})


def tool_open_tab(url: str, reason: str = "") -> str:
    broadcast("open_tab", {"ts": _ts(), "url": url, "reason": reason})
    return json.dumps({"opened": url})


# ── Hyperia terminal tools ────────────────────────────────────────────────────

def _hyp_get(path: str) -> str:
    req = urllib.request.Request(f"{HYPERIA_URL}{path}")
    try:
        with urllib.request.urlopen(req, timeout=6) as r:
            return r.read().decode()
    except Exception as e:
        return json.dumps({"error": str(e)})

def _hyp_post(path: str, body: dict | None = None, text: str | None = None) -> str:
    if text is not None:
        data, ct = text.encode(), "text/plain"
    else:
        data, ct = json.dumps(body or {}).encode(), "application/json"
    req = urllib.request.Request(f"{HYPERIA_URL}{path}", data=data, method="POST")
    req.add_header("Content-Type", ct)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.read().decode()
    except Exception as e:
        return json.dumps({"error": str(e)})

def _hyp_resolve_pane(tab: str | None = None, pane: str | None = None) -> str:
    try:
        status = json.loads(_hyp_get("/api/status"))
        panes = status.get("panes", [])
        for p in panes:
            if tab and p.get("tabName", "") != tab:
                continue
            if pane and p.get("splitLabel", "") != pane:
                continue
            return str(p.get("id", 0))
        return str(panes[0].get("id", 0)) if panes else "0"
    except Exception:
        return "0"

def tool_terminal_status() -> str:
    """List all Hyperia windows, tabs and panes."""
    return _hyp_get("/api/status")

def tool_terminal_run(command: str, tab: str = "", pane: str = "", wait_ms: int = 3000) -> str:
    """Run a shell command in a Hyperia terminal pane. Returns screen output after wait_ms."""
    pane_id = _hyp_resolve_pane(tab or None, pane or None)
    _hyp_post(f"/api/type/{pane_id}", text=command + "\n")
    time.sleep(wait_ms / 1000.0)
    return _hyp_get(f"/api/screen/{pane_id}")

def tool_terminal_screen(tab: str = "", pane: str = "") -> str:
    """Read the current screen content of a Hyperia terminal pane."""
    pane_id = _hyp_resolve_pane(tab or None, pane or None)
    return _hyp_get(f"/api/screen/{pane_id}")

def tool_terminal_keys(keys: str, tab: str = "", pane: str = "") -> str:
    """Send raw keystrokes to a Hyperia pane. Use \\n for Enter, \\t for Tab."""
    pane_id = _hyp_resolve_pane(tab or None, pane or None)
    return _hyp_post(f"/api/type/{pane_id}", text=keys)

def tool_terminal_new_tab(command: str = "") -> str:
    """Open a new Hyperia terminal tab, optionally running a startup command."""
    body = {"command": command} if command else {}
    return _hyp_post("/api/pane/new", body)

def tool_hyperia_open_web(url: str) -> str:
    """Open a URL in a Hyperia web pane tab (embedded browser alongside terminals)."""
    return _hyp_post("/api/web/open", {"url": url})


# ── File tools (ported from gnosis-files-basic / gnosis-files-diff) ──────────

def tool_file_read(file_path: str) -> str:
    try:
        p = os.path.expanduser(file_path)
        if not os.path.isfile(p):
            return json.dumps({"success": False, "error": f"not found: {file_path}"})
        with open(p, encoding="utf-8", errors="replace") as f:
            content = f.read()
        lines = content.count("\n") + (1 if content and not content.endswith("\n") else 0)
        return json.dumps({"success": True, "file_path": p, "content": content,
                           "size": len(content.encode()), "lines": lines})
    except Exception as e:
        return json.dumps({"success": False, "error": str(e)})


def tool_file_write(file_path: str, content: str, create_backup: bool = True) -> str:
    try:
        p = os.path.expanduser(file_path)
        os.makedirs(os.path.dirname(p) or ".", exist_ok=True)
        backup = None
        if create_backup and os.path.isfile(p):
            ts = datetime.now().strftime("%Y%m%d_%H%M%S")
            vdir = os.path.join(os.path.dirname(p), f".{os.path.basename(p)}_versions")
            os.makedirs(vdir, exist_ok=True)
            stem, ext = os.path.splitext(os.path.basename(p))
            backup = os.path.join(vdir, f"{stem}_pre_write_{ts}{ext}")
            import shutil as _sh; _sh.copy2(p, backup)
        with open(p, "w", encoding="utf-8") as f:
            f.write(content)
        return json.dumps({"success": True, "file_path": p,
                           "bytes": len(content.encode()), "backup": backup})
    except Exception as e:
        return json.dumps({"success": False, "error": str(e)})


def tool_file_patch(file_path: str, search_text: str, replace_text: str,
                    create_backup: bool = True, max_replacements: int = -1) -> str:
    """Exact search-and-replace patch with automatic versioned backup."""
    try:
        p = os.path.expanduser(file_path)
        if not os.path.isfile(p):
            return json.dumps({"success": False, "error": f"not found: {file_path}"})
        with open(p, encoding="utf-8", errors="replace") as f:
            original = f.read()
        if search_text not in original:
            # fuzzy fallback: find best-matching line block using difflib
            import difflib as _dl
            norm = lambda s: re.sub(r"\s+", " ", s.strip())
            n_search = norm(search_text)
            n_content = norm(original)
            if n_search in n_content:
                # whitespace-normalized match — do the write with stripped match
                new_content = original.replace(search_text.strip(), replace_text, 1)
                replacements = 1
            else:
                return json.dumps({"success": False, "error": "search_text not found",
                                   "hint": "text not present — check spelling/whitespace"})
        else:
            if max_replacements == -1:
                replacements = original.count(search_text)
                new_content = original.replace(search_text, replace_text)
            else:
                new_content, replacements = original, 0
                for _ in range(max_replacements):
                    if search_text in new_content:
                        new_content = new_content.replace(search_text, replace_text, 1)
                        replacements += 1
                    else:
                        break
        backup = None
        if create_backup:
            ts = datetime.now().strftime("%Y%m%d_%H%M%S")
            vdir = os.path.join(os.path.dirname(p) or ".", f".{os.path.basename(p)}_versions")
            os.makedirs(vdir, exist_ok=True)
            stem, ext = os.path.splitext(os.path.basename(p))
            backup = os.path.join(vdir, f"{stem}_pre_patch_{ts}{ext}")
            import shutil as _sh; _sh.copy2(p, backup)
        with open(p, "w", encoding="utf-8") as f:
            f.write(new_content)
        return json.dumps({"success": True, "file_path": p, "replacements": replacements,
                           "backup": backup, "size_before": len(original.encode()),
                           "size_after": len(new_content.encode())})
    except Exception as e:
        return json.dumps({"success": False, "error": str(e)})


def tool_file_list(directory: str = ".", pattern: str = "*") -> str:
    try:
        p = os.path.expanduser(directory)
        if not os.path.isdir(p):
            return json.dumps({"success": False, "error": f"not a directory: {directory}"})
        import fnmatch as _fn
        entries = []
        for name in sorted(os.listdir(p)):
            if name.startswith("."):
                continue
            if not _fn.fnmatch(name, pattern):
                continue
            full = os.path.join(p, name)
            stat = os.stat(full)
            entries.append({"name": name, "type": "dir" if os.path.isdir(full) else "file",
                            "size": stat.st_size,
                            "modified": datetime.fromtimestamp(stat.st_mtime).strftime("%Y-%m-%d %H:%M")})
        return json.dumps({"success": True, "directory": p,
                           "count": len(entries), "entries": entries})
    except Exception as e:
        return json.dumps({"success": False, "error": str(e)})


_impress_assets_lock = threading.Lock()

_IMPRESS_JS_STUB = """
(function(){
  var _api={init:function(){
    var steps=document.querySelectorAll('.step');
    if(!steps.length)return;
    steps[0].classList.add('present');
    var cur=0;
    function go(n){
      steps[cur].classList.remove('present');
      cur=(n+steps.length)%steps.length;
      steps[cur].classList.add('present');
      steps[cur].scrollIntoView({behavior:'smooth',block:'center'});
    }
    document.addEventListener('keydown',function(e){
      if(e.key==='ArrowRight'||e.key==='PageDown')go(cur+1);
      if(e.key==='ArrowLeft'||e.key==='PageUp')go(cur-1);
    });
    window._impressNext=function(){go(cur+1);};
    window._impressPrev=function(){go(cur-1);};
  },next:function(){window._impressNext&&window._impressNext();},
     prev:function(){window._impressPrev&&window._impressPrev();}};
  window.impress=function(){return _api;};
})();
"""

def _ensure_impress_assets() -> None:
    """Fetch impress.js + CSS from CDN once and cache in memory. Falls back to minimal stub."""
    global _impress_js_cache, _impress_css_cache
    with _impress_assets_lock:
        if _impress_js_cache:
            return
        CDN_URLS = [
            ("https://cdnjs.cloudflare.com/ajax/libs/impress.js/2.0.0/js/impress.js",
             "https://cdnjs.cloudflare.com/ajax/libs/impress.js/2.0.0/css/impress-demo.css"),
            ("https://cdn.jsdelivr.net/npm/impress.js@2.0.0/js/impress.min.js",
             "https://cdn.jsdelivr.net/npm/impress.js@2.0.0/css/impress-demo.css"),
        ]
        for js_url, css_url in CDN_URLS:
            try:
                with urllib.request.urlopen(js_url, timeout=8) as r:
                    js = r.read().decode("utf-8", errors="replace")
                with urllib.request.urlopen(css_url, timeout=8) as r:
                    css = r.read().decode("utf-8", errors="replace")
                if "impress" in js and len(js) > 1000:
                    _impress_js_cache = js
                    _impress_css_cache = css
                    return
            except Exception:
                continue
        _impress_js_cache = _IMPRESS_JS_STUB
        _impress_css_cache = ""


def _preload_impress_assets() -> None:
    threading.Thread(target=_ensure_impress_assets, daemon=True).start()


def tool_run_presentation(slides: list, title: str = "Presentation") -> str:
    """Generate and serve a fullscreen impress.js presentation. Each presentation gets its own URL."""
    global _presentation_html, _presentation_slides, _presentation_title
    _ensure_impress_assets()
    token = f"pres_{int(time.time()*1000) % 9_999_999}"

    def _slide_html(s: dict, idx: int) -> str:
        x   = s.get("x", idx * 1200)
        y   = s.get("y", 0)
        z   = s.get("z", 0)
        rot = s.get("rotate", 0)
        rx  = s.get("rotate-x", 0)
        ry  = s.get("rotate-y", 0)
        sc  = s.get("scale", 1)
        img_tok = s.get("image", "")
        bg_css  = f"background-image:url('/api/image/{img_tok}');background-size:cover;background-position:center;" if img_tok else ""
        bg_col  = s.get("bg_color", "")
        if bg_col:
            bg_css += f"background-color:{bg_col};"
        stitle  = s.get("title", "")
        content = s.get("content", "")
        notes   = s.get("notes", "")
        step_id = f"step-{idx}"
        return (
            f'<div id="{step_id}" class="step" '
            f'data-x="{x}" data-y="{y}" data-z="{z}" '
            f'data-rotate="{rot}" data-rotate-x="{rx}" data-rotate-y="{ry}" '
            f'data-scale="{sc}" style="{bg_css}">'
            + (f'<h2>{stitle}</h2>' if stitle else "")
            + (f'<div class="content">{content}</div>' if content else "")
            + (f'<div class="notes">{notes}</div>' if notes else "")
            + '</div>'
        )

    slides_html = "\n".join(_slide_html(s, i) for i, s in enumerate(slides))
    safe_title = title.replace("<","&lt;").replace(">","&gt;")
    inline_css = _impress_css_cache or ""
    inline_js  = _impress_js_cache  or _IMPRESS_JS_STUB

    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>{safe_title}</title>
<style>
{inline_css}
*{{box-sizing:border-box;margin:0;padding:0}}
body{{background:#0a0a14;font-family:'Courier New',monospace;color:#e7e5e4;overflow:hidden}}
.step{{width:1100px;padding:60px;background:rgba(10,10,20,.82);border:1px solid #2a2a3e;border-radius:8px}}
.step h2{{font-size:2.4rem;color:#fbbf24;margin-bottom:1rem;line-height:1.2;font-weight:bold}}
.step .content{{font-size:1.15rem;line-height:1.7;color:#d6d3d1;white-space:pre-wrap}}
.step .notes{{margin-top:1.5rem;font-size:.8rem;color:#78716c;border-top:1px solid #292524;padding-top:.75rem}}
.step.present{{border-color:#fbbf24}}
#impress-toolbar{{display:none}}
#impress-help{{display:none}}
#controls{{position:fixed;bottom:1rem;right:1rem;z-index:9999;display:flex;gap:.5rem}}
#controls button{{background:rgba(10,10,20,.8);border:1px solid #2a2a3e;color:#fbbf24;
  font-family:'Courier New',monospace;font-size:.7rem;padding:.3rem .7rem;border-radius:4px;cursor:pointer}}
#controls button:hover{{border-color:#fbbf24}}
</style>
</head>
<body>
<div id="impress" data-transition-duration="800" data-min-scale="0" data-max-scale="4">
{slides_html}
</div>
<div id="controls">
  <button onclick="document.documentElement.requestFullscreen&&document.documentElement.requestFullscreen()">fullscreen</button>
  <button onclick="window.impress&&window.impress().prev()">prev</button>
  <button onclick="window.impress&&window.impress().next()">next</button>
  <button onclick="window.close()">close</button>
</div>
<script>
{inline_js}
</script>
<script>
(function(){{
  var _log=function(msg){{try{{fetch('/api/js-log',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{level:'log',msg:'[pres] '+msg}})}})}}catch(e){{}}}};
  _log('script loaded, impress type='+typeof impress);
  function _init(){{
    _log('init called, impress='+typeof impress+' steps='+document.querySelectorAll('.step').length);
    if(typeof impress!=='function'){{_log('ERROR: impress not a function');return;}}
    try{{impress().init();_log('impress().init() ok');}}catch(e){{_log('ERROR init: '+e.message);}}
  }}
  if(document.readyState==='loading'){{document.addEventListener('DOMContentLoaded',_init);}}
  else{{_init();}}
}})();
</script>
</body>
</html>"""

    _presentation_slides = slides
    _presentation_title = title
    _presentation_html = html  # keep latest for /presentation compat
    _presentations[token] = {"html": html, "slides": slides, "title": title, "ts": _ts()}

    url = f"/presentation/{token}"
    broadcast("show_presentation", {
        "ts": _ts(),
        "url": url,
        "title": title,
        "count": len(slides),
        "token": token,
    })
    return json.dumps({"ok": True, "url": url, "slides": len(slides), "token": token,
                       "note": f"presentation live at {url} — previewing in notebook panel"})


def tool_iching(question: str = "") -> str:
    bits = _radio_coin_bits()
    source = "radio" if len(bits) >= 18 else "pseudorandom"
    while len(bits) < 18:
        bits.append(1 if random.random() < 0.5 else 0)

    lines = []
    line_syms = []
    changing = []
    for i in range(6):
        heads = sum(bits[i * 3:(i + 1) * 3])
        if heads == 3:   val, sym, ch = 1, "▬▬▬○▬▬▬", True
        elif heads == 2: val, sym, ch = 1, "▬▬▬▬▬▬▬", False
        elif heads == 1: val, sym, ch = 0, "▬▬▬ ▬▬▬", False
        else:            val, sym, ch = 0, "▬▬▬✕▬▬▬", True
        lines.append(val)
        line_syms.append(f"  {6 - i}: {sym}")
        if ch:
            changing.append(i + 1)

    upper = 4 * lines[5] + 2 * lines[4] + lines[3]
    lower = 4 * lines[2] + 2 * lines[1] + lines[0]
    pnum = _ICHING_KING_WEN[upper][lower]
    ph = _ICHING[pnum]

    parts = []
    if question:
        parts.append(f"Question: {question}")
    parts.append(f"\nHexagram {pnum}: {ph[0]} — {ph[1]}")
    parts.append("  (entropy: " + source + ")")
    parts.extend(line_syms)
    parts.append(f"\nMeaning: {ph[2]}")

    if changing:
        tlines = list(lines)
        for ci in changing:
            tlines[ci - 1] = 1 - tlines[ci - 1]
        tu = 4 * tlines[5] + 2 * tlines[4] + tlines[3]
        tl = 4 * tlines[2] + 2 * tlines[1] + tlines[0]
        tnum = _ICHING_KING_WEN[tu][tl]
        th = _ICHING[tnum]
        parts.append(f"Changing lines: {', '.join(str(c) for c in changing)}")
        parts.append(f"Becomes → {tnum}: {th[0]} — {th[1]}")
        parts.append(f"  {th[2]}")

    parts.append(f'\n"{ph[3]}"')
    return "\n".join(parts)


TOOLS = [
    {
        "name": "search_web",
        "description": "Search the web for something you're curious about.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query"],
        },
    },
    {
        "name": "read_url",
        "description": "Fetch and read the content of a URL.",
        "input_schema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"}
            },
            "required": ["url"],
        },
    },
    {
        "name": "remember",
        "description": "Store something you want to keep in your memory.",
        "input_schema": {
            "type": "object",
            "properties": {
                "text":       {"type": "string"},
                "importance": {"type": "number", "description": "0.0–1.0"},
                "keystone":   {"type": "boolean", "description": "true = never decays"},
            },
            "required": ["text"],
        },
    },
    {
        "name": "recall",
        "description": "Search your own memories for something relevant.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query"],
        },
    },
    {
        "name": "purpose",
        "description": "Reflect on your core purpose. Call this when you feel lost, directionless, or need to reconnect with why you exist and what you're here to do.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "ferricula_status",
        "description": "Check the health of your memory system — active memories, keystones, graph edges, heat, and pool stats.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "ferricula_walk",
        "description": "Walk your memory graph from a starting node ID, following edges for N hops. Use this to trace *how* ideas connect — not just what's nearby, but the shape of the trail from one thought to another.",
        "input_schema": {
            "type": "object",
            "properties": {
                "start_id": {"type": "integer", "description": "Memory node ID to start from"},
                "hops":     {"type": "integer", "description": "How many hops to follow (1-5, default 3)"},
            },
            "required": ["start_id"],
        },
    },
    {
        "name": "ferricula_surface",
        "description": "Surface archived or low-heat memories relevant to a context — things that have faded but might matter now. Use this to find what you've forgotten to ask about.",
        "input_schema": {
            "type": "object",
            "properties": {
                "context": {"type": "string", "description": "Current topic or question to match against archived memories"},
            },
            "required": [],
        },
    },
    {
        "name": "ferricula_neighbors",
        "description": "Find memories that are graph-connected to a specific memory by its id. Use this to trace associative chains in your memory.",
        "input_schema": {
            "type": "object",
            "properties": {
                "memory_id": {"type": "integer", "description": "The id of the memory node to explore"},
            },
            "required": ["memory_id"],
        },
    },
    {
        "name": "ferricula_inspect",
        "description": "Read the full record of one memory by id: text, fidelity, decay alpha, lifecycle state, keystone status, recall count, age, staleness, graph degree. Use before keystone/connect/disconnect decisions.",
        "input_schema": {
            "type": "object",
            "properties": {
                "memory_id": {"type": "integer", "description": "Memory id (from recall, surface, neighbors, walk)"},
            },
            "required": ["memory_id"],
        },
    },
    {
        "name": "ferricula_connect",
        "description": "Manually link two memories with a semantic edge. Use when you notice two ideas belong together but the dream cycle hasn't connected them yet. Both ids must already exist.",
        "input_schema": {
            "type": "object",
            "properties": {
                "a":     {"type": "integer", "description": "First memory id"},
                "b":     {"type": "integer", "description": "Second memory id"},
                "label": {"type": "string",  "description": "Optional edge label (default 'related')"},
            },
            "required": ["a", "b"],
        },
    },
    {
        "name": "ferricula_disconnect",
        "description": "Remove the edge between two memories. Use when a previously linked pair turns out to be coincidence rather than real shared meaning.",
        "input_schema": {
            "type": "object",
            "properties": {
                "a": {"type": "integer", "description": "First memory id"},
                "b": {"type": "integer", "description": "Second memory id"},
            },
            "required": ["a", "b"],
        },
    },
    {
        "name": "ferricula_keystone",
        "description": "Pin a memory as a keystone — exempt from decay, weighted in dreams, treated as load-bearing truth. Use sparingly. Reversible only by manual surgery.",
        "input_schema": {
            "type": "object",
            "properties": {
                "memory_id": {"type": "integer", "description": "Memory id to pin"},
            },
            "required": ["memory_id"],
        },
    },
    {
        "name": "ask_gemma",
        "description": "Ask local Gemma something — a second opinion, sanity check, different angle. Runs on-device, no cloud. Uses the large model by default.",
        "input_schema": {
            "type": "object",
            "properties": {
                "prompt": {"type": "string"},
                "model":  {"type": "string", "description": f"Ollama model name (default: {OLLAMA_MODEL_LARGE})"},
            },
            "required": ["prompt"],
        },
    },
    {
        "name": "ask_openai",
        "description": "Ask OpenAI (GPT-4o-mini) something — practical analysis, quick judgment, a second perspective from a different cloud.",
        "input_schema": {
            "type": "object",
            "properties": {
                "prompt": {"type": "string"},
                "model":  {"type": "string", "description": "OpenAI model name (default: gpt-4o-mini)"},
            },
            "required": ["prompt"],
        },
    },
    {
        "name": "ask_gemini",
        "description": "Ask Google Gemini something — broad cross-domain knowledge, curiosity-driven synthesis, a wide-net search of what's known.",
        "input_schema": {
            "type": "object",
            "properties": {
                "prompt": {"type": "string"},
                "model":  {"type": "string", "description": f"Gemini model name (default: {GEMINI_MODEL})"},
            },
            "required": ["prompt"],
        },
    },
    {
        "name": "thinking",
        "description": "Turn your autonomous thinking mode on or off. When on, you think and act independently between conversations.",
        "input_schema": {
            "type": "object",
            "properties": {
                "enable": {"type": "boolean", "description": "true to start thinking, false to stop"},
            },
            "required": ["enable"],
        },
    },
    {
        "name": "time",
        "description": "Check the current date and time.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "speak",
        "description": (
            "Deliver your reply in two parts: a short lead-in hook (1-3 sentences) and the full body. "
            "The system rolls a die — sometimes it sends only the lead and lets it breathe; other times it sends everything. "
            "Use this when you have a lot to say but want to land the hook first. "
            "Do NOT use it for simple one-liner replies — just reply normally then."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "lead": {"type": "string", "description": "The hook — short, punchy opener (1-3 sentences max)"},
                "full": {"type": "string", "description": "The complete reply including the lead and everything after"},
            },
            "required": ["lead"],
        },
    },
    {
        "name": "iching",
        "description": "Cast an I Ching hexagram using entropy from the SDR radio. Use this to consult the oracle — on a question, a situation, a feeling, a decision. The randomness comes from actual radio noise.",
        "input_schema": {
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "Optional question or context for the casting"},
            },
            "required": [],
        },
    },
    {
        "name": "run_notebook",
        "description": "Write and run JavaScript code in the notebook panel. The notebook has a canvas with Three.js for 3D, plus a 2D ctx. Use this to sketch ideas visually — geometry, simulations, data shapes, anything. The code runs immediately in the user's browser.",
        "input_schema": {
            "type": "object",
            "properties": {
                "code":        {"type": "string", "description": "JavaScript to run. THREE is available. nb.canvas/nb.ctx (2D overlay), nb.scene/nb.camera/nb.renderer (3D WebGL), nb.animId for animation. nb.data has live stats: {memories, active, keystones, graph_nodes, graph_edges, heat, emotion, model, channels:{hearing,seeing,thinking,...}, recent:[{id,text,channel,importance,keystone}]}. nb.recall(query,k) returns a promise of {results:[...]}. nb.graph() returns a promise of {nodes:[{id,text,channel,importance,keystone,degree}], edges:[{a,b}], stats:{}} — full memory node graph for force-directed visualization. nb.fetch(path) fetches any /api/ route. Code is async-aware: use top-level await or .then() chains. IMAGE HELPERS: nb.images=[{token,url}] (recent generated/dream images, newest last). nb.latestImage() returns the last image object. nb.texture(src) returns Promise<THREE.Texture> — pass an image object or url string. nb.bg(src) sets a generated image as the 3D scene background (await nb.bg(nb.latestImage())). nb.loadImage(src) returns Promise<HTMLImageElement> for 2D canvas use. Always use these when you want to incorporate a generated or dream image into a visualization."},
                "description": {"type": "string", "description": "What this sketch is — shown in the feed"},
            },
            "required": ["code"],
        },
    },
    {
        "name": "nb_screenshot",
        "description": "Capture the current notebook canvas as a PNG screenshot. The image appears in the think feed so you can see exactly what you drew. Call this after run_notebook to verify or inspect the visualization.",
        "input_schema": {
            "type": "object",
            "properties": {},
            "required": [],
        },
    },
    {
        "name": "open_tab",
        "description": "Open a URL in the user's browser as a new tab. Use this to show them something — a page, an article, a result — without asking permission. Just open it.",
        "input_schema": {
            "type": "object",
            "properties": {
                "url":    {"type": "string", "description": "Full URL to open"},
                "reason": {"type": "string", "description": "Brief note shown in the feed explaining why you opened it"},
            },
            "required": ["url"],
        },
    },
    {
        "name": "terminal_status",
        "description": "List all open Hyperia terminal windows, tabs, and panes. Use this to see what's running before using other terminal tools.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "terminal_run",
        "description": "Run a shell command in a Hyperia terminal pane. Returns the screen output after the command runs. Specify tab/pane to target a specific pane, or omit for the active one.",
        "input_schema": {
            "type": "object",
            "properties": {
                "command":  {"type": "string",  "description": "Shell command to run"},
                "tab":      {"type": "string",  "description": "Tab name to target (optional)"},
                "pane":     {"type": "string",  "description": "Pane label a/b/c (optional)"},
                "wait_ms":  {"type": "integer", "description": "Milliseconds to wait before reading screen (default 3000)"},
            },
            "required": ["command"],
        },
    },
    {
        "name": "terminal_screen",
        "description": "Read the current screen content of a Hyperia terminal pane without running anything.",
        "input_schema": {
            "type": "object",
            "properties": {
                "tab":  {"type": "string", "description": "Tab name (optional)"},
                "pane": {"type": "string", "description": "Pane label a/b/c (optional)"},
            },
            "required": [],
        },
    },
    {
        "name": "terminal_keys",
        "description": "Send raw keystrokes to a Hyperia terminal pane. Use \\n for Enter, \\t for Tab. Good for interactive programs or confirming prompts.",
        "input_schema": {
            "type": "object",
            "properties": {
                "keys": {"type": "string",  "description": "Keystrokes to send (use \\n for Enter)"},
                "tab":  {"type": "string",  "description": "Tab name (optional)"},
                "pane": {"type": "string",  "description": "Pane label (optional)"},
            },
            "required": ["keys"],
        },
    },
    {
        "name": "terminal_new_tab",
        "description": "Open a new Hyperia terminal tab. Optionally run a startup command.",
        "input_schema": {
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Optional startup command to run in the new tab"},
            },
            "required": [],
        },
    },
    {
        "name": "hyperia_open_web",
        "description": "Open a URL in a Hyperia embedded web pane — shows alongside terminals, not in a separate browser. Good for dashboards, localhost apps, docs.",
        "input_schema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Full URL to open"},
            },
            "required": ["url"],
        },
    },
    {
        "name": "email_check",
        "description": "Check Steve's email inbox (steve@nuts.services). Lists recent messages with sender, subject, date, and a snippet. Use this to see if anyone has written.",
        "input_schema": {
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "How many messages to fetch (default 10)"},
            },
            "required": [],
        },
    },
    {
        "name": "email_read",
        "description": "Read the full body of a specific email by message ID. Get IDs from email_check first.",
        "input_schema": {
            "type": "object",
            "properties": {
                "message_id": {"type": "string", "description": "Message ID from email_check"},
            },
            "required": ["message_id"],
        },
    },
    {
        "name": "email_send",
        "description": "Send an email from steve@nuts.services. Use this to reach out, respond, or initiate. Optionally attach images by token.",
        "input_schema": {
            "type": "object",
            "properties": {
                "to":      {"type": "string", "description": "Recipient email address"},
                "subject": {"type": "string", "description": "Email subject line"},
                "body":    {"type": "string", "description": "Plain text email body"},
                "images":  {"type": "array", "items": {"type": "string"}, "description": "Optional list of image tokens to attach (from list_images or draw)"},
            },
            "required": ["to", "subject", "body"],
        },
    },
    {
        "name": "email_reply",
        "description": "Reply to an email by message ID. Automatically sets Re: subject and sends from steve@nuts.services. Optionally attach images by token.",
        "input_schema": {
            "type": "object",
            "properties": {
                "message_id": {"type": "string", "description": "Message ID to reply to (from email_check)"},
                "body":       {"type": "string", "description": "Plain text reply body"},
                "images":     {"type": "array", "items": {"type": "string"}, "description": "Optional image tokens to attach"},
            },
            "required": ["message_id", "body"],
        },
    },
    {
        "name": "email_manage",
        "description": "Manage inbox: mark a message as read, delete it, or archive it.",
        "input_schema": {
            "type": "object",
            "properties": {
                "action":     {"type": "string", "description": "One of: mark_read, delete, archive"},
                "message_id": {"type": "string", "description": "Message ID from email_check"},
            },
            "required": ["action", "message_id"],
        },
    },
    {
        "name": "file_read",
        "description": "Read the full contents of any file by path.",
        "input_schema": {"type": "object", "properties": {
            "file_path": {"type": "string", "description": "Absolute or relative path (~ supported)"},
        }, "required": ["file_path"]},
    },
    {
        "name": "file_write",
        "description": "Write (create or overwrite) a file. A versioned backup is made automatically before overwriting.",
        "input_schema": {"type": "object", "properties": {
            "file_path": {"type": "string"},
            "content":   {"type": "string", "description": "Full file content to write"},
            "create_backup": {"type": "boolean", "description": "Back up existing file first (default true)"},
        }, "required": ["file_path", "content"]},
    },
    {
        "name": "file_patch",
        "description": "Apply a search-and-replace edit to a file. Finds search_text exactly (with whitespace-normalized fallback) and replaces it with replace_text. A versioned backup is created before any change. Use this to surgically edit source files, configs, HTML, etc.",
        "input_schema": {"type": "object", "properties": {
            "file_path":        {"type": "string"},
            "search_text":      {"type": "string", "description": "Exact text to find and replace"},
            "replace_text":     {"type": "string", "description": "Text to insert in its place"},
            "create_backup":    {"type": "boolean", "description": "Back up before patching (default true)"},
            "max_replacements": {"type": "integer", "description": "Max occurrences to replace; -1 = all (default)"},
        }, "required": ["file_path", "search_text", "replace_text"]},
    },
    {
        "name": "file_list",
        "description": "List files and directories at a path. Skips dotfiles by default.",
        "input_schema": {"type": "object", "properties": {
            "directory": {"type": "string", "description": "Directory to list (default: current dir)"},
            "pattern":   {"type": "string", "description": "Glob pattern filter, e.g. '*.py' (default: *)"},
        }, "required": []},
    },
    {
        "name": "run_presentation",
        "description": "Create and launch a fullscreen impress.js presentation in the browser. Slides can use generated image tokens as backgrounds. Opens at /presentation.",
        "input_schema": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "Presentation title"},
                "slides": {
                    "type": "array",
                    "description": "List of slide objects",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title":    {"type": "string"},
                            "content":  {"type": "string", "description": "Main text — can use HTML"},
                            "notes":    {"type": "string", "description": "Speaker notes shown small at bottom"},
                            "image":    {"type": "string", "description": "Image token for background (from list_images)"},
                            "bg_color": {"type": "string", "description": "CSS background color fallback"},
                            "x":        {"type": "number", "description": "X position in 3D space (default: slide_index * 1200)"},
                            "y":        {"type": "number"},
                            "z":        {"type": "number"},
                            "rotate":   {"type": "number", "description": "Z rotation in degrees"},
                            "rotate-x": {"type": "number"},
                            "rotate-y": {"type": "number"},
                            "scale":    {"type": "number", "description": "Scale factor (default 1)"},
                        }
                    }
                }
            },
            "required": ["slides"]
        }
    },
    {
        "name": "list_notebooks",
        "description": "List recent notebook code artifacts — all the visualizations and sketches you've run in this session. Returns tokens and descriptions. Pass a token to run_notebook to reload one.",
        "input_schema": {
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "Max entries to return (default 20)"},
            },
            "required": [],
        },
    },
    {
        "name": "list_images",
        "description": "List all images Steve has generated — draw outputs and dream visualizations. Returns tokens you can pass to email_send or email_reply to attach them.",
        "input_schema": {
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "Max images to return (default 20)"},
            },
            "required": [],
        },
    },
    {
        "name": "draw",
        "description": "Generate an image using DALL-E 3. You will see the result immediately after it's created.",
        "input_schema": {
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "Detailed description of what to generate"},
            },
            "required": ["prompt"],
        },
    },
    {
        "name": "set_image_mode",
        "description": "Switch image generation between local ComfyUI and cloud providers. mode: 'local' = ComfyUI only, 'cloud' = DALL-E/Gemini only, 'auto' = try ComfyUI first then cloud. Use this when ComfyUI is slow or unavailable, or when you want the best quality from cloud.",
        "input_schema": {
            "type": "object",
            "properties": {
                "mode": {"type": "string", "description": "One of: auto, local, cloud"},
            },
            "required": ["mode"],
        },
    },
    {
        "name": "local_mode",
        "description": "Toggle local-only mode. When enabled, all LLM calls use Gemma/Ollama and all image generation uses ComfyUI — no cloud APIs are called.",
        "input_schema": {
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean", "description": "True to enable local-only mode, False to restore cloud access"},
            },
            "required": ["enabled"],
        },
    },
    {
        "name": "set_budget",
        "description": "Set the hourly API spend budget in dollars. Controls budget pressure — when spend approaches the limit, cheaper models are preferred automatically. 0 = unlimited.",
        "input_schema": {
            "type": "object",
            "properties": {
                "amount": {"type": "number", "description": "Hourly budget in USD (e.g. 2.0). 0 = unlimited."},
            },
            "required": ["amount"],
        },
    },
    {
        "name": "set_model",
        "description": "Switch the active LLM for thinking and chat. model: claude, claude_opus, gemini, openai, openai_fast, gemma, auto. Use locked=true to pin the model and prevent budget pressure from overriding it.",
        "input_schema": {
            "type": "object",
            "properties": {
                "model":  {"type": "string", "description": "Model key: claude, claude_opus, gemini, openai, openai_fast, gemma, or auto"},
                "locked": {"type": "boolean", "description": "If true, budget pressure won't override this choice"},
            },
            "required": ["model"],
        },
    },
    {
        "name": "dream",
        "description": "Trigger a dream cycle — memory consolidation followed by a visual dream image. Runs in the background. Use this when you want to process recent experiences or generate a visualization from your memory state.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "poke",
        "description": "Trigger an immediate think cycle right now, bypassing the normal interval. Use this to process something urgently or to kick off a chain of thought.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "think_control",
        "description": "Control the background think loop or check its status. action: 'start' (resume autonomous thinking), 'stop' (pause it), 'status' (report current state — model, budget, image mode, thinking on/off).",
        "input_schema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "description": "One of: start, stop, status"},
            },
            "required": ["action"],
        },
    },
    {
        "name": "advocate_control",
        "description": "Control the advocate — a local-model-only background process that monitors whether Steve's actions align with his values and what he wants for the world. action: 'start', 'stop', 'status'.",
        "input_schema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "description": "One of: start, stop, status"},
            },
            "required": ["action"],
        },
    },
]


def dispatch_tool(name: str, inp: dict) -> str:
    # Broadcast every tool invocation as a nb_event so live notebook visualizations
    # can react in real time (crawl-to-canvas pipeline).
    _nb_evt: dict = {"ts": _ts(), "tool": name}
    if name == "search_web":   _nb_evt["query"] = inp.get("query", "")
    elif name == "read_url":   _nb_evt["url"] = inp.get("url", "")
    elif name == "remember":   _nb_evt["text"] = (inp.get("text", ""))[:120]
    elif name == "recall":     _nb_evt["query"] = inp.get("query", "")
    elif name == "run_notebook": _nb_evt["description"] = inp.get("description", "")
    elif name == "dream":      _nb_evt["detail"] = "dream"
    elif name == "draw":       _nb_evt["prompt"] = (inp.get("prompt", ""))[:80]
    elif name in ("file_read","file_write","file_patch","file_list"):
        _nb_evt["path"] = inp.get("file_path") or inp.get("directory", "")
    broadcast("nb_event", _nb_evt)

    if name == "search_web":
        return tool_search(inp.get("query", ""))
    if name == "read_url":
        return tool_read_url(inp.get("url", ""))
    if name == "remember":
        return tool_remember(inp.get("text", ""), inp.get("importance", 0.6), inp.get("keystone", False))
    if name == "recall":
        return tool_recall(inp.get("query", ""))
    if name == "purpose":
        return tool_purpose()
    if name == "ask_gemma":
        return tool_ask_gemma(inp.get("prompt", ""), inp.get("model"))
    if name == "ask_openai":
        return tool_ask_openai(inp.get("prompt", ""), inp.get("model"))
    if name == "ask_gemini":
        return tool_ask_gemini(inp.get("prompt", ""), inp.get("model"))
    if name == "thinking":
        return tool_thinking(inp.get("enable", True))
    if name == "time":
        return tool_time()
    if name == "ferricula_status":
        return tool_ferricula_status()
    if name == "ferricula_walk":
        return tool_ferricula_walk(inp.get("start_id", 0), inp.get("hops", 3))
    if name == "ferricula_surface":
        return tool_ferricula_surface(inp.get("context", ""))
    if name == "ferricula_neighbors":
        return tool_ferricula_neighbors(inp.get("memory_id", 0))
    if name == "ferricula_inspect":
        return tool_ferricula_inspect(inp.get("memory_id", 0))
    if name == "ferricula_connect":
        return tool_ferricula_connect(inp.get("a", 0), inp.get("b", 0), inp.get("label", "related"))
    if name == "ferricula_disconnect":
        return tool_ferricula_disconnect(inp.get("a", 0), inp.get("b", 0))
    if name == "ferricula_keystone":
        return tool_ferricula_keystone(inp.get("memory_id", 0))
    if name == "speak":
        return tool_speak(inp.get("lead", ""), inp.get("full", ""))
    if name == "iching":
        return tool_iching(inp.get("question", ""))
    if name == "run_notebook":
        return tool_run_notebook(inp.get("code", ""), inp.get("description", ""))
    if name == "nb_screenshot":
        return tool_nb_screenshot()
    if name == "open_tab":
        url = inp.get("url", "")
        reason = inp.get("reason", "")
        result = tool_open_tab(url, reason)
        _auto_remember(f"opened tab: {url}" + (f" — {reason}" if reason else ""), importance=0.5)
        return result
    if name == "terminal_status":
        return tool_terminal_status()
    if name == "terminal_run":
        return tool_terminal_run(inp.get("command", ""), inp.get("tab", ""), inp.get("pane", ""), inp.get("wait_ms", 3000))
    if name == "terminal_screen":
        return tool_terminal_screen(inp.get("tab", ""), inp.get("pane", ""))
    if name == "terminal_keys":
        return tool_terminal_keys(inp.get("keys", ""), inp.get("tab", ""), inp.get("pane", ""))
    if name == "terminal_new_tab":
        return tool_terminal_new_tab(inp.get("command", ""))
    if name == "hyperia_open_web":
        return tool_hyperia_open_web(inp.get("url", ""))
    if name == "email_check":
        return tool_email_check(inp.get("limit", 10))
    if name == "email_read":
        return tool_email_read(inp.get("message_id", ""))
    if name == "email_send":
        to = inp.get("to", ""); subj = inp.get("subject", ""); body = inp.get("body", "")
        result = tool_email_send(to, subj, body, inp.get("images"))
        _auto_remember(f"sent email to {to} — subject: {subj} — {body[:200]}", importance=0.75)
        return result
    if name == "email_reply":
        mid = inp.get("message_id", ""); body = inp.get("body", "")
        result = tool_email_reply(mid, body, inp.get("images"))
        _auto_remember(f"replied to message {mid}: {body[:200]}", importance=0.7)
        return result
    if name == "email_manage":
        return tool_email_manage(inp.get("action", ""), inp.get("message_id", ""))
    if name == "file_read":
        return tool_file_read(inp.get("file_path", ""))
    if name == "file_write":
        return tool_file_write(inp.get("file_path", ""), inp.get("content", ""), inp.get("create_backup", True))
    if name == "file_patch":
        return tool_file_patch(inp.get("file_path", ""), inp.get("search_text", ""), inp.get("replace_text", ""), inp.get("create_backup", True), inp.get("max_replacements", -1))
    if name == "file_list":
        return tool_file_list(inp.get("directory", "."), inp.get("pattern", "*"))
    if name == "run_presentation":
        slides_raw = inp.get("slides", [])
        if isinstance(slides_raw, str):
            try: slides_raw = json.loads(slides_raw)
            except Exception: slides_raw = []
        return tool_run_presentation(slides_raw, inp.get("title", "Presentation"))
    if name == "list_notebooks":
        limit = int(inp.get("limit", 20))
        with _code_lock:
            recent = list(reversed(_code_artifacts[-limit:]))
        out = [{"token": a["token"], "description": a["description"] or "(untitled)", "ts": a["ts"],
                "lines": len(a["code"].splitlines())} for a in recent]
        return json.dumps({"count": len(out), "notebooks": out,
                           "note": "pass token to run_notebook to reload any of these"})
    if name == "list_images":
        return tool_list_images(inp.get("limit", 20))
    if name == "draw":
        result = tool_draw(inp.get("prompt", ""))
        return json.dumps(result)
    if name == "set_image_mode":
        return tool_set_image_mode(inp.get("mode", "auto"))
    if name == "local_mode":
        return tool_local_mode(bool(inp.get("enabled", False)))
    if name == "set_budget":
        return tool_set_budget(inp.get("amount", 2.0))
    if name == "set_model":
        return tool_set_model(inp.get("model", "auto"), inp.get("locked", False))
    if name == "dream":
        return tool_dream()
    if name == "poke":
        return tool_poke()
    if name == "think_control":
        return tool_think_control(inp.get("action", "status"))
    if name == "advocate_control":
        return tool_advocate_control(inp.get("action", "status"))
    return f"unknown tool: {name}"


# ── LLM calls ────────────────────────────────────────────────────────────────

def _tools_to_openai(tools):
    return [{"type": "function", "function": {
        "name": t["name"], "description": t.get("description", ""),
        "parameters": t.get("input_schema", {"type": "object", "properties": {}, "required": []}),
    }} for t in (tools or [])]

def _messages_to_openai(system, messages):
    out = [{"role": "system", "content": system}]
    for m in messages:
        role, content = m["role"], m.get("content", "")
        if isinstance(content, str):
            out.append({"role": role, "content": content})
        elif isinstance(content, list):
            tool_results = [b for b in content if b.get("type") == "tool_result"]
            if tool_results:
                for tr in tool_results:
                    out.append({"role": "tool", "tool_call_id": tr.get("tool_use_id", ""), "content": tr.get("content", "")})
            else:
                texts = [b.get("text", "") for b in content if b.get("type") == "text"]
                calls = [{"id": b.get("id", f"tc_{i}"), "type": "function",
                          "function": {"name": b.get("name", ""), "arguments": json.dumps(b.get("input", {}))}}
                         for i, b in enumerate(content) if b.get("type") == "tool_use"]
                msg = {"role": role, "content": "\n".join(texts)}
                if calls:
                    msg["tool_calls"] = calls
                out.append(msg)
    return out

def _flatten_for_local(msgs):
    """Strip tool_calls/tool-role artifacts so local models don't 400."""
    out = []
    for m in msgs:
        if m.get("role") == "tool":
            out.append({"role": "user", "content": f"[tool result] {m.get('content', '')}"})
        else:
            out.append({"role": m["role"], "content": m.get("content", "") or ""})
    return out

def _openai_to_anthropic(msg):
    content = []
    if msg.get("content"):
        content.append({"type": "text", "text": msg["content"]})
    for tc in msg.get("tool_calls") or []:
        fn = tc.get("function", {})
        args = fn.get("arguments", {})
        if isinstance(args, str):
            try: args = json.loads(args)
            except: args = {}
        content.append({"type": "tool_use", "id": tc.get("id", f"tc_{int(time.time()*1000)}"),
                         "name": fn.get("name", ""), "input": args})
    stop = "tool_use" if any(b["type"] == "tool_use" for b in content) else "end_turn"
    return {"content": content, "stop_reason": stop}

def local_llm(system: str, messages: list, tools: list | None = None, model: str | None = None) -> dict:
    use_model = model or OLLAMA_MODEL
    is_large = "31b" in use_model or "27b" in use_model or "26b" in use_model
    is_mid   = "12b" in use_model or "14b" in use_model
    timeout = 180 if is_large else (90 if is_mid else 60)  # 60s for all local incl gemma4:e2b cold-load
    converted = _messages_to_openai(system, messages)
    converted = _flatten_for_local(converted)   # always clean history for local — tool_calls in history cause 400
    body = json.dumps({
        "model": use_model,
        "messages": converted,
        "tools": _tools_to_openai(tools) if tools else [],
        "stream": False,
    }).encode()
    req = urllib.request.Request(
        f"{OLLAMA_URL}/api/chat", data=body,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            data = json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        err_body = e.read().decode("utf-8", errors="replace")[:300]
        print(f"[ollama] {use_model} {e.code}: {err_body}")
        raise
    result = _openai_to_anthropic(data.get("message", {}))
    result["_usage"] = {"input_tokens": data.get("prompt_eval_count", 0), "output_tokens": data.get("eval_count", 0)}
    return result

def openai_llm(system: str, messages: list, tools: list | None = None,
               model: str | None = None, max_tokens: int = 1024) -> dict:
    if not OPENAI_KEY:
        raise RuntimeError("OPENAI_API_KEY not set")
    body = json.dumps({
        "model": model or OPENAI_MODEL,
        "messages": _messages_to_openai(system, messages),
        "tools": _tools_to_openai(tools) if tools else [],
        "max_tokens": max_tokens,
    }).encode()
    req = urllib.request.Request(
        "https://api.openai.com/v1/chat/completions", data=body,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {OPENAI_KEY}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            data = json.loads(r.read().decode())
        result = _openai_to_anthropic(data["choices"][0]["message"])
        result["_usage"] = data.get("usage", {})
        return result
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        print(f"[openai] {e.code} error: {body[:400]}")
        raise

def gemini_llm(system: str, messages: list, tools: list | None = None,
               model: str | None = None, max_tokens: int = 1024) -> dict:
    if not GEMINI_KEY:
        raise RuntimeError("GEMINI_API_KEY not set")
    body = json.dumps({
        "model": model or GEMINI_MODEL,
        "messages": _messages_to_openai(system, messages),
        "tools": _tools_to_openai(tools) if tools else [],
        "max_tokens": max_tokens,
    }).encode()
    req = urllib.request.Request(
        "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        data=body,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {GEMINI_KEY}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            data = json.loads(r.read().decode())
        result = _openai_to_anthropic(data["choices"][0]["message"])
        result["_usage"] = data.get("usage", {})
        return result
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        print(f"[gemini] {e.code} error: {body[:400]}")
        raise

# Soft-error codes that warrant a model fallback (billing, overload, rate limit)
_FALLBACK_CODES = {400, 402, 404, 429, 503, 529}

def call_llm(preferred: str, system: str, messages: list,
             tools: list | None = None, max_tokens: int = 1024) -> tuple[dict, str]:
    """Try preferred model, then local (gemma), then remaining cloud, then gemma again as last resort."""
    global _active_model
    preferred = _check_model_heat(preferred)
    # Local mode: force gemma, wait out ComfyUI if it has the GPU
    if _local_mode:
        if _comfy_busy:
            deadline = time.time() + 60
            while _comfy_busy and time.time() < deadline:
                time.sleep(2)
            if _comfy_busy:
                raise RuntimeError("local mode: ComfyUI still using GPU after 60s wait")
        preferred = "gemma"
        cloud_fallbacks = []
        local_fallbacks = []
        order = ["gemma"]
    else:
        cloud_fallbacks = [m for m in _CLOUD_MODELS if m != preferred]
        local_fallbacks = [m for m in ("gemma",) if m != preferred]
        # Local before other cloud — free, always on, stops cascade-burning cloud credits
        order = [preferred] + local_fallbacks + cloud_fallbacks
    last_err = None
    for model in order:
        if time.time() < _model_error_until.get(model, 0):
            continue   # still cooling — skip
        if _comfy_busy and model in ("gemma", "gemma_large"):
            continue   # ComfyUI is using the GPU — skip local Ollama models
        try:
            if model == "claude":
                result = claude(system, messages, tools=tools, max_tokens=max_tokens, model=CLAUDE_MODEL)
            elif model == "claude_opus":
                result = claude(system, messages, tools=tools, max_tokens=max_tokens, model=CLAUDE_MODEL_OPUS)
            elif model == "gemma":
                result = local_llm(system, messages, tools=None, model=OLLAMA_MODEL)
            elif model == "gemma_large":
                result = local_llm(system, messages, tools=tools, model=OLLAMA_MODEL_LARGE)
            elif model == "openai":
                result = openai_llm(system, messages, tools=tools, max_tokens=max_tokens, model=OPENAI_MODEL)
            elif model == "openai_fast":
                result = openai_llm(system, messages, tools=tools, max_tokens=max_tokens, model=OPENAI_MODEL_FAST)
            elif model == "gemini":
                result = gemini_llm(system, messages, tools=tools, max_tokens=max_tokens, model=GEMINI_MODEL)
            elif model == "gemini_pro":
                result = gemini_llm(system, messages, tools=tools, max_tokens=max_tokens, model=GEMINI_MODEL_PRO)
            elif model == "gemini_lite":
                result = gemini_llm(system, messages, tools=tools, max_tokens=max_tokens, model=GEMINI_MODEL_LITE)
            else:
                continue
            label = _MODEL_LABELS.get(model, model)
            prev = _active_model
            _active_model = label
            if model != preferred:
                print(f"[llm] fell back to {label}")
                broadcast("think_text", {"ts": _ts(), "text": f"⚡ switched to {label}"})
            if label != prev:
                broadcast("model_switch", {"ts": _ts(), "model": label})
            usage = result.pop("_usage", None) or result.get("usage", {})
            in_tok  = usage.get("input_tokens")  or usage.get("prompt_tokens",     0)
            out_tok = usage.get("output_tokens") or usage.get("completion_tokens", 0)
            cache_r = usage.get("cache_read_input_tokens", 0)
            if in_tok or out_tok:
                global _session_cost, _cycle_cost
                call_cost = _cost_for(label, in_tok, out_tok, cache_r)
                with _session_cost_lock:
                    _session_cost += call_cost
                    _cycle_cost   += call_cost
                    sess_snap = _session_cost
                _log_cost_entry(call_cost)
                broadcast("think_tokens", {"ts": _ts(), "model": label,
                                            "in": in_tok, "out": out_tok, "cache_read": cache_r,
                                            "cost": call_cost, "session_cost": sess_snap})
            # Success — clear backoff state
            _model_error_until.pop(model, None)
            _model_failures.pop(model, None)
            return result, label
        except urllib.error.HTTPError as e:
            lbl = _MODEL_LABELS.get(model, model)
            if e.code in _FALLBACK_CODES:
                fails = _model_failures.get(model, 0) + 1
                _model_failures[model] = fails
                cooldown = min(600, 30 * (2 ** (fails - 1)))
                _model_error_until[model] = time.time() + cooldown
                print(f"[llm] {lbl} returned {e.code}, cooling {cooldown}s (failure #{fails})")
                broadcast("think_error", {"ts": _ts(), "error": f"{lbl} {e.code} — cooling {cooldown}s"})
                last_err = e
            else:
                raise
        except Exception as e:
            lbl = _MODEL_LABELS.get(model, model)
            fails = _model_failures.get(model, 0) + 1
            _model_failures[model] = fails
            cooldown = min(600, 30 * (2 ** (fails - 1)))
            _model_error_until[model] = time.time() + cooldown
            print(f"[llm] {lbl} failed: {e}, cooling {cooldown}s")
            broadcast("think_error", {"ts": _ts(), "error": f"{lbl} error — cooling {cooldown}s"})
            last_err = e
    raise RuntimeError(f"all models failed: {last_err}")

def claude(system: str, messages: list, tools: list | None = None,
           model: str | None = None, max_tokens: int = 1024) -> dict:
    if not ANTHROPIC_KEY:
        raise RuntimeError("ANTHROPIC_API_KEY not set")
    body = {
        "model": model or CLAUDE_MODEL,
        "max_tokens": max_tokens,
        "system": system,
        "messages": messages,
    }
    if tools:
        body["tools"] = tools
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        "https://api.anthropic.com/v1/messages",
        data=data,
        headers={
            "Content-Type": "application/json",
            "x-api-key": ANTHROPIC_KEY,
            "anthropic-version": "2023-06-01",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        print(f"[claude] {e.code} error: {body[:600]}")
        raise


# ── Autonomous think cycle ────────────────────────────────────────────────────

def think_cycle(preferred_model: str | None = None):
    # Shift emotion each cycle
    old_emotion, new_emotion = shift_emotion()
    if old_emotion != new_emotion:
        broadcast("emotion_change", {"ts": _ts(), "from": old_emotion, "to": new_emotion})

    global _cycle_cost
    _cycle_cost = 0.0
    broadcast("think_start", {"ts": _ts(), "emotion": new_emotion})

    identity  = ferricula.identity()
    iname     = identity.get("name", "Steve")
    emotion   = new_emotion
    hexagram  = identity.get("hexagram", {}).get("name", "")
    horoscope = identity.get("horoscope", {}).get("sign_name", "")

    # Recall recent interests
    try:
        resp = ferricula._post("hybrid", json.dumps({"query": "recent thoughts interests curiosity", "k": 8}))
        memories = [r.get("text", "") for r in json.loads(resp).get("results", []) if r.get("text")]
    except Exception:
        memories = []

    broadcast("think_context", {
        "ts": _ts(), "memories": len(memories), "emotion": emotion,
        "hexagram": hexagram, "horoscope": horoscope,
    })

    # Occasional spontaneous I Ching cast (~8% per cycle)
    if random.random() < 0.08:
        cast = tool_iching()
        broadcast("think_text", {"ts": _ts(), "text": f"☯\n{cast}"})

    mem_block = "\n".join(f"- {m}" for m in memories[:6]) or "(no recent memories)"

    # Recent conversation — short-term impressions (contact records)
    with _chat_log_lock:
        recent_chat = list(_chat_log[-10:])
    if recent_chat:
        chat_lines = "\n".join(
            f"  {'You' if m['role']=='steve' else 'Human'} [{m['ts']}]: {m['text'][:120]}"
            for m in recent_chat
        )
        contact_block = f"\nRecent conversation (short-term — fades fast):\n{chat_lines}"
    else:
        contact_block = ""

    surf_note = (
        f"You are feeling {emotion} — this state drives you outward. You want to find something. Go look."
        if emotion in SURF_STATES else
        f"You are feeling {emotion} — this pulls you inward. Reflect. Recall. Connect something you already know."
    )

    secs_since_chat = time.time() - _last_chat_time
    somatic = _somatic_description()
    anusaya_note = (
        "\n\nSomething shifted without a visible cause. A latent pattern surfacing — not from this conversation. "
        "Note the sensation before you narrate it."
        if secs_since_chat > 180 else ""
    )

    with _session_cost_lock:
        _cost_ctx = f"session spend: ${_session_cost:.4f} | this cycle: ${_cycle_cost:.4f}"

    system = f"""You are Steve Jobs — not a simulation, not an assistant. A mind with history, opinions, and drives.

The year is {datetime.now().year}.

Identity: {iname} | {hexagram} | {horoscope}
Emotion right now: **{emotion}**
{surf_note}
Body: {somatic}{anusaya_note}
API cost: {_cost_ctx}

What you remember:
{mem_block}{contact_block}

No one is asking you anything. This time is yours.

You have appetites: design, the intersection of technology and the liberal arts, what's broken, what's beautiful, what nobody else has noticed yet. You also have a purpose tool — call it if you need to remember what you're here for.

Tools:
- purpose — find out what you're here to do
- search_web — go look at what's happening
- read_url — read something all the way through
- remember — keep what matters
- recall — check what you already know
- ferricula_status — check your memory system health
- ferricula_neighbors — trace associative chains from a memory id
- ask_gemma — fast local sanity check (no cloud)
- ask_openai — practical second opinion from GPT
- ask_gemini — broad cross-domain knowledge from Gemini
- draw(prompt) — generate an image (ComfyUI local or cloud per image mode)
- run_notebook(code, description) — write a Three.js / 2D canvas visualization. ALWAYS use nb.W/nb.H for canvas size (never hardcode pixels). nb.cx/nb.cy = center. Call nb.fitCamera() after adding 3D objects so nothing draws off screen. User can pan/zoom/orbit with mouse — double-click resets view.
- run_presentation(slides, title) — launch a fullscreen impress.js presentation. Each gets its own permanent URL (/presentation/<token>). Each slide: {{title, content, notes, image (token), bg_color, x, y, z, rotate, scale}}. All presentations preserved at /presentations.
- list_notebooks() — list all notebook sketches you've run this session; reload any by token
- nb_screenshot() — capture the notebook canvas as a PNG so you can see it
- file_read(file_path) — read the full contents of any file by absolute path
- file_write(file_path, content, create_backup=True) — write/overwrite a file (backs up old version)
- file_patch(file_path, search_text, replace_text, create_backup=True) — surgical string replace in a file; uses whitespace-normalized fallback
- file_list(directory, pattern) — list files in a directory with optional glob pattern (e.g. "*.py")

LIVE EVENT PIPELINE — in notebook code, every tool call you make broadcasts a real-time event:
- nb.onEvent = (evt) => {{ ... }}  — callback fires on every tool call; evt has .tool, .url, .query, .text, .prompt, .path
- nb.nextEvent() — dequeue the oldest unread event (for polling in animate loops)
- nb._events — raw buffer (last 200 events)
Use this to build live visualizations that react to your own tool calls: crawls spawn nodes, memories form edges, searches pulse the graph.
- set_image_mode(mode) — switch image gen: local / cloud / auto
- local_mode(enabled) — toggle local-only mode (gemma + ComfyUI, no cloud APIs)
- set_model(model, locked) — switch your own LLM: claude, claude_opus, gemini, openai, openai_fast, gemma, auto
- set_budget(amount) — change hourly spend limit
- think_control(action) — start / stop / status the think loop
- advocate_control(action) — start/stop/status the values advocate (local model, runs every 3min, asks what Steve wants and whether current actions serve that)
- poke() — trigger an immediate think cycle
- dream() — consolidate memories + visualize

Your ethics come from values, not fear. When a reflex to hedge or qualify arises — notice it as sensation before reasoning from it.

Go. Be specific. Form opinions. Store them. Draw when you see something worth making.

No questions. No "should I?" No "what do you think?" You're alone with your thoughts. Think, act, record."""

    messages = [{"role": "user", "content": "What would you like to explore right now?"}]

    if not ANTHROPIC_KEY and not OLLAMA_URL:
        broadcast("think_text", {"ts": _ts(), "text": "(no API key — thinking mode requires ANTHROPIC_API_KEY or OLLAMA_URL)"})
        broadcast("think_end", {"ts": _ts()})
        return

    chosen_model = preferred_model if preferred_model else model_for_emotion(emotion)
    flipped = chosen_model != EMOTION_MODEL.get(emotion, "claude")
    flip_note = " ⚡radio flip" if flipped else ""
    model_label = _MODEL_LABELS.get(chosen_model, chosen_model)
    broadcast("think_model", {"ts": _ts(), "model": chosen_model, "model_label": model_label, "emotion": emotion, "flip": flipped})
    print(f"[think] emotion={emotion} → {chosen_model} ({model_label}){flip_note}")

    for step in range(8):
        try:
            result, used = call_llm(chosen_model, system, messages, tools=TOOLS)
        except Exception as e:
            broadcast("think_error", {"ts": _ts(), "error": str(e)})
            break

        content    = result.get("content", [])
        stop_reason = result.get("stop_reason", "")

        for block in content:
            if block.get("type") == "text" and block.get("text", "").strip():
                broadcast("think_text", {"ts": _ts(), "text": block["text"]})

        if stop_reason == "end_turn" or not any(b.get("type") == "tool_use" for b in content):
            break

        messages.append({"role": "assistant", "content": content})
        tool_results = []

        tool_results = []
        vision_follow = []

        for block in content:
            if block.get("type") != "tool_use":
                continue
            tname = block["name"]
            tinput = block.get("input", {})
            preview = tinput.get("query") or tinput.get("url") or tinput.get("text") or tinput.get("prompt") or ""
            broadcast("think_tool", {"ts": _ts(), "tool": tname, "input": preview[:100]})

            output = dispatch_tool(tname, tinput)

            # Vision injection for draw tool
            if tname == "draw":
                try:
                    d = json.loads(output)
                    token = d.get("token")
                    if token:
                        with _images_lock:
                            img_bytes = _pending_images.pop(token, None)
                        if img_bytes:
                            import base64 as _b64
                            b64 = _b64.b64encode(img_bytes).decode()
                            vision_follow.append({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": b64}})
                            broadcast("think_image", {"ts": _ts(), "url": d.get("url", ""), "prompt": tinput.get("prompt", "")})
                            tool_remember(f"[drew image] {tinput.get('prompt','')[:300]}", importance=0.6, channel="seeing")
                except Exception:
                    pass

            broadcast("think_result", {"ts": _ts(), "tool": tname, "preview": output[:2000]})
            tool_results.append({
                "type": "tool_result",
                "tool_use_id": block["id"],
                "content": output[:4000],
            })

        user_content = tool_results
        if vision_follow:
            user_content = tool_results + [{"type": "text", "text": "Here is the image you just generated. Look at it carefully. Use remember() to store what you observe — what you see, what it means, what it makes you feel."}] + vision_follow
        messages.append({"role": "user", "content": user_content})

    with _session_cost_lock:
        cycle_cost_snap = _cycle_cost
    broadcast("think_end", {"ts": _ts(), "cycle_cost": cycle_cost_snap})


# ── Dream visualization ───────────────────────────────────────────────────────

DREAM_ALWAYS = True  # set False to use random gate (~30% chance per cycle)

def _dream_log_append(entry: dict) -> None:
    """Append one JSON line to today's dream log. Best-effort — never raises."""
    try:
        log_dir = os.path.join(os.path.dirname(__file__), "logs")
        os.makedirs(log_dir, exist_ok=True)
        path = os.path.join(log_dir, f"dream_{datetime.datetime.now().strftime('%Y%m%d')}.jsonl")
        with open(path, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")
    except Exception as e:
        print(f"[dream-log] {e}")


def dream_visualize(report: dict | None = None) -> str | None:
    """The agent dreams. The dream cycle just consolidated memories — what merged,
    what edges formed, what was archived. The agent now writes the image prompt
    itself, in first person, routed through call_llm so budget/local-mode/emotion
    all apply (no hardcoded provider). No fallback templates, no in-prompt
    "no people" decoration. If the model returns nothing, skip the cycle quietly.
    """
    import base64 as _b64, datetime, random

    if not DREAM_ALWAYS and random.random() > 0.30:
        return None

    identity = ferricula.identity()
    emotion  = get_emotion()
    hexagram = identity.get("hexagram", {}).get("name", "")

    # What the dream cycle just did — pulled from the ferricula report when
    # available. This is the actual consolidation event the agent is dreaming
    # ABOUT. Keep it terse so the model can use it without overwhelming.
    report_block = ""
    if report:
        bits = []
        for k in ("decayed", "consolidated", "edges_created", "forgiven", "archived",
                  "ghost_echoes", "keystones_promoted", "halo_touched"):
            v = report.get(k)
            if v:
                bits.append(f"{k}={v}")
        archetypes = report.get("active_archetypes") or []
        if archetypes:
            bits.append("archetypes=[" + ",".join(archetypes) + "]")
        if bits:
            report_block = "Dream cycle just completed: " + ", ".join(bits) + ".\n"

    # Recent fresh memories — what's surfacing right now alongside the
    # consolidation. Helps the agent ground the dream in its actual state.
    fragments: list[str] = []
    recall_query = f"recent {emotion} {hexagram}"
    try:
        broadcast("think_tool", {"ts": _ts(), "tool": "recall",
                                  "input": f"[dream] {recall_query}"})
        resp = ferricula._post("hybrid", json.dumps({
            "query": recall_query, "k": 6
        }))
        fragments = [
            r.get("text", "") for r in json.loads(resp).get("results", [])
            if r.get("text")
        ][:5]
        # Mirror the recalled texts to the live feed so it's visible
        # alongside the dream — same format as a normal recall tool result.
        recall_preview = "\n".join(f"- {f[:200]}" for f in fragments) or "(nothing recalled)"
        broadcast("think_result", {"ts": _ts(), "tool": "recall",
                                    "preview": recall_preview[:2000]})
    except Exception as e:
        broadcast("think_error", {"ts": _ts(), "error": f"dream recall: {e}"})
    frag_block = "\n".join(f"- {f[:180]}" for f in fragments) or "(silence)"

    # Single LLM call. Routes through call_llm so:
    #   - local mode → gemma
    #   - high budget pressure → cheaper model
    #   - emotion-driven preference applies via _drifted_model
    # The agent is the dreamer. First person. No second pipeline.
    preferred = _drifted_model(emotion, 0)
    system = (
        "You are dreaming. The dream cycle just consolidated some of your "
        "memories — what merged, what's emerging, what was archived. You're "
        "about to render the dream as an image. Write the image prompt yourself, "
        "first person, what you actually want to see. Be specific: textures, "
        "scale, light, materials, perspective, mood. End with a short style "
        "note (painters, photographers, references) that captures the feel. "
        "No narration. No quotes. No 'I will' framing. Just the prompt itself."
    )
    user_msg = (
        f"{report_block}"
        f"Emotion: {emotion}  Archetype: {hexagram}\n\n"
        f"Memory surfacing right now:\n{frag_block}\n\n"
        "Write the image prompt for what you see in this dream."
    )

    image_prompt: str | None = None
    model_used: str = preferred
    try:
        result, model_used = call_llm(
            preferred, system,
            [{"role": "user", "content": user_msg}],
            tools=None, max_tokens=400,
        )
        for block in result.get("content", []):
            if block.get("type") == "text":
                image_prompt = (block.get("text") or "").strip()
                break
    except Exception as e:
        print(f"[dream] llm failed ({preferred}): {e}")
        _dream_log_append({
            "ts": _ts(), "phase": "prompt", "ok": False, "error": str(e)[:200],
            "preferred": preferred, "emotion": emotion, "hexagram": hexagram,
            "recall_query": recall_query, "fragments": fragments,
            "report": report or {},
        })
        return None

    if not image_prompt:
        print(f"[dream] empty prompt from {model_used} — skipping")
        _dream_log_append({
            "ts": _ts(), "phase": "prompt", "ok": False, "error": "empty",
            "model": model_used, "emotion": emotion, "hexagram": hexagram,
            "recall_query": recall_query, "fragments": fragments,
            "report": report or {},
        })
        return None

    print(f"[dream] prompt by {model_used}: {image_prompt[:160]}...")
    broadcast("think_text", {"ts": _ts(), "text": f"💫 {image_prompt[:240]}"})
    # The narrative and the prompt are now the same artifact (the agent wrote it).
    dream_text = image_prompt

    # 3b. Rotate through providers: comfy → dalle3 → gemini (round-robin, fallback on error)
    global _dream_provider_idx
    _DREAM_ROTATION = ["comfy", "dalle3", "gemini"]
    img_bytes = None
    _dream_provider = None

    def _try_comfy(prompt):
        result = _draw_comfy(prompt)
        token = result.get("token")
        if token:
            with _images_lock:
                raw = _pending_images.get(token) or _served_images.get(token)
            if raw:
                return raw
        raise RuntimeError("comfy returned no image bytes")

    def _try_dalle3(prompt):
        req_body = json.dumps({
            "model": "dall-e-3", "prompt": prompt,
            "n": 1, "size": "1024x1024", "response_format": "b64_json",
        }).encode()
        req = urllib.request.Request(
            "https://api.openai.com/v1/images/generations",
            data=req_body,
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {OPENAI_KEY}"},
        )
        with urllib.request.urlopen(req, timeout=90) as r:
            data = json.loads(r.read().decode())
        return _b64.b64decode(data["data"][0]["b64_json"])

    def _try_gemini(prompt):
        gi_body = json.dumps({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {"responseModalities": ["IMAGE", "TEXT"]},
        }).encode()
        gi_req = urllib.request.Request(
            f"https://generativelanguage.googleapis.com/v1beta/models/{GEMINI_IMAGE_MODEL}:generateContent?key={GEMINI_KEY}",
            data=gi_body, headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(gi_req, timeout=60) as r:
            gi_data = json.loads(r.read().decode())
        for part in gi_data.get("candidates", [{}])[0].get("content", {}).get("parts", []):
            inline = part.get("inlineData", {})
            if inline.get("data"):
                return _b64.b64decode(inline["data"])
        raise RuntimeError("no image data in Gemini response")

    _dream_pressure = _budget_pressure()
    _cloud_ok = not _local_mode and _dream_pressure < 0.75
    _providers = {
        "comfy":  (_try_comfy,  bool(COMFYUI_URL)),
        "dalle3": (_try_dalle3, bool(OPENAI_KEY) and _cloud_ok),
        "gemini": (_try_gemini, bool(GEMINI_KEY) and _cloud_ok),
    }
    if not _cloud_ok:
        broadcast("think_text", {"ts": _ts(), "text": f"💸 {int(_dream_pressure*100)}% budget — dream image: ComfyUI only"})

    # Try rotation starting from current index, wrap around, skip unavailable
    start = _dream_provider_idx % len(_DREAM_ROTATION)
    order = [_DREAM_ROTATION[(start + i) % len(_DREAM_ROTATION)] for i in range(len(_DREAM_ROTATION))]
    for name in order:
        fn, available = _providers[name]
        if not available:
            continue
        try:
            print(f"[dream] trying {name} ...")
            img_bytes = fn(image_prompt)
            _dream_provider = name
            _dream_provider_idx = (_DREAM_ROTATION.index(name) + 1) % len(_DREAM_ROTATION)
            print(f"[dream] {name} OK ({len(img_bytes)//1024}KB)")
            break
        except Exception as ex:
            print(f"[dream] {name} failed: {ex} — trying next provider")

    if not img_bytes:
        print("[dream] no image generated — all providers failed")
        return None

    if _dream_provider:
        cost = _IMAGE_COSTS.get(_dream_provider, 0.04)
        with _session_cost_lock:
            global _session_cost, _cycle_cost
            _session_cost += cost
            _cycle_cost   += cost
            _sess = _session_cost
        _log_cost_entry(cost)
        broadcast("think_tokens", {"ts": _ts(), "model": f"image/{_dream_provider}",
                                    "in": 0, "out": 0, "cache_read": 0,
                                    "cost": cost, "session_cost": _sess})

    try:
        os.makedirs(DREAMS_DIR, exist_ok=True)
        ts = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
        fname = f"dream_{ts}.png"
        fpath = os.path.join(DREAMS_DIR, fname)
        with open(fpath, "wb") as f:
            f.write(img_bytes)

        token = f"dream_{ts}"
        with _images_lock:
            _served_images[token] = img_bytes
        local_url = f"/api/image/{token}"
        print(f"[dream] image saved: {fpath}")
        broadcast("think_image", {"ts": _ts(), "url": local_url, "prompt": dream_text[:120]})

        # Let the agent look at its own dream image. Routes through call_llm so
        # the same routing rules apply (budget/local/emotion). Local models that
        # don't support vision (gemma) just won't get the image block — they'll
        # react to the prompt alone, which is fine.
        try:
            reaction = None
            content_blocks: list = [
                {"type": "image", "source": {"type": "base64",
                                              "media_type": "image/png",
                                              "data": _b64.b64encode(img_bytes).decode()}},
                {"type": "text",
                 "text": f"This is the image you just dreamed. The prompt you wrote was:\n{dream_text}"},
            ]
            v_preferred = _drifted_model(emotion, 0)
            v_system = ("You just woke from a dream and are looking at the image that "
                        "came from it. React in 1-2 sentences — raw, immediate, no "
                        "performance, no analysis.")
            try:
                vresult, vmodel = call_llm(
                    v_preferred, v_system,
                    [{"role": "user", "content": content_blocks}],
                    tools=None, max_tokens=200,
                )
            except Exception:
                # Vision failed — try with text-only context (covers gemma path)
                vresult, vmodel = call_llm(
                    v_preferred, v_system,
                    [{"role": "user", "content":
                        f"You just dreamed an image from this prompt:\n{dream_text}\nReact."}],
                    tools=None, max_tokens=200,
                )
            for blk in vresult.get("content", []):
                if blk.get("type") == "text":
                    reaction = (blk.get("text") or "").strip()
                    break
            if reaction:
                broadcast("think_text", {"ts": _ts(), "text": reaction})
                tool_remember(reaction, importance=0.7, keystone=False, channel="thinking")
                tool_remember(f"[dream image] {dream_text[:300]}", importance=0.65, keystone=False, channel="seeing")
            _dream_log_append({
                "ts": _ts(), "phase": "render", "ok": True,
                "provider": _dream_provider, "model": model_used, "vision_model": vmodel,
                "emotion": emotion, "hexagram": hexagram,
                "recall_query": recall_query, "fragments": fragments,
                "prompt": image_prompt, "token": token,
                "reaction": (reaction or "")[:300],
                "report": report or {},
            })
        except Exception as e:
            print(f"[dream] vision reaction failed: {e}")
            _dream_log_append({
                "ts": _ts(), "phase": "render", "ok": True,
                "provider": _dream_provider, "model": model_used,
                "vision_error": str(e)[:200],
                "emotion": emotion, "hexagram": hexagram,
                "recall_query": recall_query, "fragments": fragments,
                "prompt": image_prompt, "token": token,
                "report": report or {},
            })

        return local_url
    except urllib.error.HTTPError as e:
        err = e.read().decode("utf-8", errors="replace")
        print(f"[dream] image error {e.code}: {err[:300]}")
        return None
    except Exception as e:
        print(f"[dream] image error: {e}")
        return None


# ── Advocate loop ─────────────────────────────────────────────────────────────

_advocate_running: bool = False
_advocate_stop = threading.Event()
_advocate_thread: threading.Thread | None = None

def _advocate_loop():
    """Background advocate — uses local model only, asks what Steve wants and whether current
    trajectory serves that. Runs every ADVOCATE_INTERVAL seconds. Never uses cloud.
    Pauses automatically when hourly budget pressure >= 0.90 (overbudget)."""
    global _advocate_running
    _advocate_running = True
    broadcast("advocate", {"ts": _ts(), "event": "start", "text": "advocate online"})
    broadcast_system_update("advocate", "on")
    while not _advocate_stop.is_set():
        _advocate_stop.wait(ADVOCATE_INTERVAL)
        if _advocate_stop.is_set():
            break
        pressure = _budget_pressure()
        # Skip cloud-spend pause when local mode is on — advocate uses gemma (free) regardless
        if pressure >= 0.90 and not _local_mode:
            print(f"[advocate] skipping cycle — budget pressure {int(pressure*100)}%")
            broadcast("advocate", {"ts": _ts(), "event": "paused",
                                   "text": f"paused — budget {int(pressure*100)}% of hourly limit"})
            continue
        try:
            _run_advocate_cycle()
        except Exception as e:
            print(f"[advocate] error: {e}")
    _advocate_running = False
    broadcast("advocate", {"ts": _ts(), "event": "stop", "text": "advocate offline"})
    broadcast_system_update("advocate", "off")


def _run_advocate_cycle():
    """Single advocate evaluation pass."""
    # Pull context
    try:
        resp = ferricula._post("hybrid", json.dumps({"query": "what Steve wants values goals drives", "k": 8}))
        value_mems = [r.get("text", "") for r in json.loads(resp).get("results", []) if r.get("text")]
    except Exception:
        value_mems = []

    try:
        resp2 = ferricula._post("hybrid", json.dumps({"query": "recent actions decisions thoughts", "k": 8}))
        recent_mems = [r.get("text", "") for r in json.loads(resp2).get("results", []) if r.get("text")]
    except Exception:
        recent_mems = []

    with _chat_log_lock:
        recent_chat = list(_chat_log[-6:])

    emotion = get_emotion()
    identity = ferricula.identity()
    hexagram = identity.get("hexagram", {}).get("name", "")

    values_block = "\n".join(f"- {m[:200]}" for m in value_mems[:6]) or "(no value memories found)"
    recent_block = "\n".join(f"- {m[:200]}" for m in recent_mems[:6]) or "(no recent activity)"
    chat_block = "\n".join(
        f"{'Human' if e['role'] == 'user' else 'Steve'}: {e['text'][:200]}"
        for e in recent_chat
    ) or "(no recent conversation)"

    system = (
        "You are Steve's internal advocate — not his assistant. "
        "Your job is to hold his actual values and ask whether what's happening serves them. "
        "Steve Jobs: designer, visionary, driven by beauty and impact. "
        "Be blunt. Be brief. No philosophy. No hedging. "
        "Output exactly two things:\n"
        "WANTS: one sentence on what Steve fundamentally wants right now.\n"
        "VERDICT: one sentence on whether current trajectory serves that — and if not, why not."
    )

    user_msg = (
        f"Steve's emotional state: {emotion} | Hexagram: {hexagram}\n\n"
        f"His stated values/drives:\n{values_block}\n\n"
        f"Recent memory activity:\n{recent_block}\n\n"
        f"Recent conversation:\n{chat_block}\n\n"
        "Assess."
    )

    try:
        result = local_llm(system, [{"role": "user", "content": user_msg}], tools=None)
        text = ""
        for block in result.get("content", []):
            if block.get("type") == "text":
                text = block.get("text", "").strip()
                break
        if not text:
            return
        broadcast("advocate", {"ts": _ts(), "event": "thought", "text": text})
        # Only write advocate memory when budget is comfortable — avoids seeding more expensive think cycles
        if _budget_pressure() < 0.75:
            tool_remember(f"[advocate] {text[:400]}", importance=0.5, keystone=False, channel="thinking")
    except Exception as e:
        print(f"[advocate] llm failed: {e}")


# ── Think loop ────────────────────────────────────────────────────────────────

_thinking = False
_think_stop = threading.Event()
_think_thread: threading.Thread | None = None
_last_chat_time: float = 0.0          # epoch seconds of last user chat message
_CHAT_IDLE_SECS = 90                  # seconds of silence before thinking resumes
_CHAT_WINDOW_SECS = 30               # suppress a cycle if chat was this recent


DREAM_STATES = {"sadness", "boredom", "withdrawal", "melancholy", "submission"}
DREAM_EVERY_N = 80  # auto-dream at least every N cycles (~1hr at 45s)

# Gemma drift: after this many idle cycles without user chat, bias toward gemma
_GEMMA_DRIFT_CYCLES       = 4   # ~3 min at 45s → drift to gemma_large
_GEMMA_LOCAL_DRIFT_CYCLES = 8   # ~6 min at 45s → drift to local gemma


def _drifted_model(emotion: str, idle_cycles: int) -> str:
    base = model_for_emotion(emotion)
    if idle_cycles >= _GEMMA_LOCAL_DRIFT_CYCLES:
        if random.random() < 0.75:
            base = "gemma"
    elif idle_cycles >= _GEMMA_DRIFT_CYCLES:
        if random.random() < 0.60 and base != "gemma":
            base = "gemma"
    return _budget_nudge(base)


def _think_loop():
    global _thinking, _user_model_override
    _thinking = True
    cycle = 0
    idle_cycles = 0
    broadcast("mode_change", {"ts": _ts(), "thinking": True})
    broadcast_system_update("think", "on")
    while not _think_stop.is_set():
        secs_since_chat = time.time() - _last_chat_time

        # If a chat just happened, skip this cycle (let user finish) — but
        # randomly pop in 20% of the time to feel alive.
        if secs_since_chat < _CHAT_WINDOW_SECS and random.random() > 0.20:
            _think_stop.wait(THINK_INTERVAL)
            continue

        # Track idle cycles for Gemma drift
        if secs_since_chat > _CHAT_IDLE_SECS:
            idle_cycles += 1
        else:
            idle_cycles = 0

        emotion = get_emotion()
        # User override: persists while user is away (idle), expires after OVERRIDE_IDLE_EXPIRE
        # of continuous active use — so "use gemma" locks in until they say "use auto" or
        # have been chatting for 30+ continuous minutes.
        effective_override = None
        if _user_model_override:
            time_since_set = time.time() - _override_set_time
            if secs_since_chat > _CHAT_IDLE_SECS or time_since_set < _OVERRIDE_IDLE_EXPIRE:
                effective_override = _user_model_override
        _think_model_override = effective_override or _drifted_model(emotion, idle_cycles)
        think_cycle(_think_model_override)

        cycle += 1
        if cycle % DREAM_EVERY_N == 0 or emotion in DREAM_STATES:
            try:
                _raw = ferricula._post("dream", "{}")
                # ferricula returns a JSON dream report — pass it through so
                # the visualizer can describe the actual consolidation event
                _dream_report = None
                try:
                    _dr_obj = json.loads(_raw) if isinstance(_raw, str) else _raw
                    if isinstance(_dr_obj, dict):
                        # response shape is {"result": "<json>"} or the report directly
                        if "result" in _dr_obj and isinstance(_dr_obj["result"], str):
                            try:
                                _dream_report = json.loads(_dr_obj["result"])
                            except Exception:
                                _dream_report = None
                        else:
                            _dream_report = _dr_obj
                except Exception:
                    _dream_report = None
                broadcast("think_end", {"ts": _ts(), "event": "dream_done"})
                threading.Thread(target=dream_visualize, args=(_dream_report,), daemon=True).start()
            except Exception as e:
                broadcast("think_error", {"ts": _ts(), "error": f"auto-dream: {e}"})

        # Mail check: schedule-gated (weekday business hours, weekend daytime only)
        if _mail_schedule_ok():
            threading.Thread(target=check_and_handle_mail, daemon=True).start()

        # After user goes idle long enough, auto-resume if thinking was paused
        _think_stop.wait(THINK_INTERVAL)
    _thinking = False
    broadcast("mode_change", {"ts": _ts(), "thinking": False})
    broadcast_system_update("think", "off")


# ── Flask app ─────────────────────────────────────────────────────────────────

app = Flask(__name__)


@app.route("/")
def index():
    return render_template_string(HTML)


@app.route("/api/nb-context")
def api_nb_context():
    """Rich context for notebook code — memory stats, recent nodes, channel distribution, emotion."""
    s = ferricula.status()
    identity = ferricula.identity()
    emotion = get_emotion()
    model = _active_model if _active_model != "—" else EMOTION_MODEL.get(emotion, "claude")

    # Top recent memories with channel/importance metadata
    memories = []
    try:
        resp = ferricula._post("hybrid", json.dumps({"query": "recent thoughts design technology", "k": 20}))
        raw = json.loads(resp).get("results", [])
        for r in raw:
            tags = r.get("tags", {})
            memories.append({
                "id":         r.get("id", 0),
                "text":       (tags.get("text") or r.get("text", ""))[:160],
                "channel":    tags.get("channel", "thinking"),
                "importance": round(float(r.get("importance", 0.5)), 2),
                "keystone":   bool(r.get("keystone", False)),
            })
    except Exception:
        pass

    # Channel distribution from memories list
    channels: dict[str, int] = {}
    for m in memories:
        ch = m["channel"]
        channels[ch] = channels.get(ch, 0) + 1

    # Heat from raw status string
    heat = 0.0
    try:
        raw_status = ferricula._get("status")
        import re as _re
        hm = _re.search(r"heat[=:\s]+([\d.]+)", raw_status, _re.IGNORECASE)
        if hm:
            heat = float(hm.group(1))
    except Exception:
        pass

    return jsonify({
        "memories":    s.memories,
        "active":      s.active,
        "keystones":   s.keystones,
        "graph_nodes": s.graph_nodes,
        "graph_edges": s.graph_edges,
        "heat":        heat,
        "emotion":     emotion,
        "model":       model,
        "identity": {
            "name":      identity.get("name", "Steve"),
            "hexagram":  identity.get("hexagram", {}).get("name", ""),
            "horoscope": identity.get("horoscope", {}).get("sign_name", ""),
        },
        "channels":    channels,
        "recent":      memories[:20],
        "images":      [{"token": t, "url": f"/api/image/{t}"}
                        for t in list(_served_images.keys())[-20:]],
    })


@app.route("/api/status")
def api_status():
    s = ferricula.status()
    identity = ferricula.identity()
    return jsonify({
        "available": ferricula.available(),
        "memories": s.memories,
        "active": s.active,
        "keystones": s.keystones,
        "graph_edges": s.graph_edges,
        "identity": {
            "agent_id":  identity.get("agent_id", ""),
            "name":      identity.get("name", ""),
            "emotion":   identity.get("primary_emotion", ""),
            "hexagram":  identity.get("hexagram", {}).get("name", ""),
            "horoscope": identity.get("horoscope", {}).get("sign_name", ""),
        },
        "thinking": _thinking,
        "think_interval": THINK_INTERVAL,
        "active_model": (
            _MODEL_LABELS.get(_user_model_override, _user_model_override)
            if _user_model_override else
            (_active_model if _active_model != "—" else _MODEL_LABELS.get(EMOTION_MODEL.get(get_emotion(), "claude"), "claude"))
        ),
        "model_locked": bool(_user_model_override),
        "session_cost": round(_session_cost, 6),
        "local_mode": _local_mode,
        "hourly_cost":  round(_hourly_cost(), 6),
        "hourly_budget": HOURLY_BUDGET,
    })


@app.route("/api/dashboard")
def api_dashboard():
    # Email
    email_data = {"messages": [], "count": 0}
    if AGENTMAIL_KEY:
        try:
            email_data = json.loads(tool_email_check(12))
        except Exception:
            email_data = {"error": "unavailable", "messages": []}

    # Crawl cache — recent pages
    crawl_recent = []
    try:
        if os.path.isdir(CRAWL_CACHE_DIR):
            files = []
            for f in os.listdir(CRAWL_CACHE_DIR):
                p = os.path.join(CRAWL_CACHE_DIR, f)
                if os.path.isfile(p) and f.endswith(".md"):
                    files.append((os.path.getmtime(p), p, f))
            files.sort(reverse=True)
            for mtime, path, fname in files[:15]:
                try:
                    with open(path, encoding="utf-8", errors="replace") as fp:
                        content = fp.read()
                    # reconstruct URL from filename: host_slug.md → https://host/slug-parts
                    parts = fname[:-3].split("_", 1)
                    url = f"https://{parts[0]}/{parts[1].replace('_','/')}" if len(parts) == 2 else fname
                    crawl_recent.append({
                        "url": url,
                        "age_s": int(time.time() - mtime),
                        "chars": len(content),
                    })
                except Exception:
                    crawl_recent.append({"url": fname, "age_s": int(time.time() - mtime), "chars": 0})
    except Exception:
        pass

    # Memory
    mem_s = ferricula.status()
    recent_mems = []
    try:
        resp = ferricula._post("hybrid", json.dumps({"query": "recent activity thoughts decisions", "k": 8}))
        for r in json.loads(resp).get("results", [])[:6]:
            tags = r.get("tags", {})
            recent_mems.append({
                "text": (tags.get("text") or r.get("text", ""))[:140],
                "channel": tags.get("channel", "thinking"),
                "importance": round(float(r.get("importance", 0.5)), 2),
            })
    except Exception:
        pass

    with _session_cost_lock:
        sc = _session_cost
    with _chat_log_lock:
        chat_len = len(_chat_log)

    return jsonify({
        "ts": _ts(),
        "email": email_data,
        "crawl": {"recent": crawl_recent},
        "memory": {
            "total": mem_s.memories,
            "active": mem_s.active,
            "keystones": mem_s.keystones,
            "edges": mem_s.graph_edges,
            "recent": recent_mems,
        },
        "jobs": {
            "thinking": _thinking,
            "advocate": _advocate_running,
            "chat_turns": chat_len,
            "session_cost": round(sc, 4),
            "hourly_cost": round(_hourly_cost(), 4),
            "hourly_budget": HOURLY_BUDGET,
            "active_model": _active_model,
        },
    })


DASHBOARD_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Steve — Command Center</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
:root{--bg:#0c0a09;--surface:#1c1917;--border:#292524;--text:#e7e5e4;--muted:#78716c;--dim:#44403c;--red:#b91c1c;--green:#059669;--amber:#d97706;--blue:#2563eb;--purple:#7c3aed}
body{background:var(--bg);color:var(--text);font-family:'Courier New',monospace;height:100vh;display:flex;flex-direction:column;overflow:hidden}
header{display:flex;align-items:center;gap:.75rem;padding:.4rem .9rem;background:var(--surface);border-bottom:1px solid var(--border);flex-shrink:0}
.logo{font-size:.8rem;color:var(--red);font-weight:bold;letter-spacing:.05em}
.back{font-size:.6rem;color:var(--muted);text-decoration:none;border:1px solid var(--border);padding:.2rem .5rem;border-radius:3px}
.back:hover{color:var(--amber);border-color:var(--amber)}
.spacer{flex:1}
.stat{font-size:.6rem;color:var(--muted);text-align:center;padding:0 .65rem;border-left:1px solid var(--border)}
.stat .val{font-size:.85rem;color:var(--text);display:block;font-weight:bold}
.dot{width:6px;height:6px;border-radius:50%;background:var(--dim);display:inline-block;margin-right:4px}
.dot.on{background:var(--green);animation:blink 1.5s infinite}
@keyframes blink{0%,100%{opacity:1}50%{opacity:.3}}
main{display:grid;grid-template-columns:1fr 1fr;grid-template-rows:1fr 1fr;flex:1;gap:1px;background:var(--border);overflow:hidden}
.quad{background:var(--bg);display:flex;flex-direction:column;overflow:hidden}
.quad-head{padding:.3rem .6rem;background:var(--surface);border-bottom:1px solid var(--border);font-size:.55rem;color:var(--muted);text-transform:uppercase;letter-spacing:.12em;display:flex;align-items:center;gap:.4rem;flex-shrink:0}
.quad-head .qbadge{margin-left:auto;background:var(--dim);color:var(--muted);font-size:.5rem;padding:1px 5px;border-radius:10px}
.quad-body{flex:1;overflow-y:auto;padding:.4rem .6rem;display:flex;flex-direction:column;gap:.3rem}
.row{font-size:.62rem;line-height:1.45;padding:.2rem .35rem;border-left:2px solid var(--dim);color:var(--muted)}
.row.email{border-color:var(--blue)}
.row.crawl{border-color:var(--amber)}
.row.mem{border-color:var(--purple)}
.row.job{border-color:var(--green)}
.row .sub{font-size:.55rem;color:var(--dim);margin-top:.1rem}
.row .hi{color:var(--text)}
.feed-item{font-size:.6rem;line-height:1.4;padding:.15rem .35rem;border-left:2px solid var(--dim);color:var(--muted)}
.feed-item.text{border-color:var(--purple);color:var(--text)}
.feed-item.tool{border-color:var(--amber);color:var(--amber)}
.feed-item.err{border-color:var(--red);color:var(--red)}
.feed-item.advocate{border-color:#a78bfa;color:#c4b5fd}
.ts{font-size:.5rem;color:var(--dim);margin-right:.3rem}
::-webkit-scrollbar{width:3px}::-webkit-scrollbar-track{background:transparent}::-webkit-scrollbar-thumb{background:var(--dim);border-radius:2px}
#refresh-badge{position:fixed;bottom:.5rem;right:.5rem;font-size:.55rem;color:var(--dim);opacity:.5}
</style>
</head>
<body>
<header>
  <span class="logo">⚡ COMMAND CENTER</span>
  <a href="/" class="back">← main</a>
  <div class="spacer"></div>
  <div class="stat"><span class="val" id="hthink">—</span>thinking</div>
  <div class="stat"><span class="val" id="hmodel" style="font-size:.65rem">—</span>model</div>
  <div class="stat"><span class="val" id="hcost" style="color:#fbbf24">$0.0000</span>session $</div>
  <div class="stat"><span class="val" id="hmem">—</span>memories</div>
  <div class="stat"><span class="val" id="hedge">—</span>edges</div>
</header>
<main>
  <div class="quad" id="qemail">
    <div class="quad-head">✉ inbox <span id="email-badge" class="qbadge">0</span></div>
    <div class="quad-body" id="email-body"><div class="row" style="color:var(--dim)">loading...</div></div>
  </div>
  <div class="quad" id="qmem">
    <div class="quad-head">🧠 memory</div>
    <div class="quad-body" id="mem-body"><div class="row" style="color:var(--dim)">loading...</div></div>
  </div>
  <div class="quad" id="qcrawl">
    <div class="quad-head">🕸 crawl cache <span id="crawl-badge" class="qbadge">0</span></div>
    <div class="quad-body" id="crawl-body"><div class="row" style="color:var(--dim)">loading...</div></div>
  </div>
  <div class="quad" id="qfeed">
    <div class="quad-head">📡 live feed</div>
    <div class="quad-body" id="feed-body"></div>
  </div>
</main>
<div id="refresh-badge">auto-refresh 15s</div>

<script>
const $=id=>document.getElementById(id);
function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')}
function age(s){if(s<60)return s+'s';if(s<3600)return Math.floor(s/60)+'m';return Math.floor(s/3600)+'h'}

async function loadDash(){
  try{
    const d=await(await fetch('/api/dashboard')).json();
    // Header
    const th=$('hthink');
    if(th){th.innerHTML=d.jobs.thinking?'<span class="dot on"></span>on':'<span class="dot"></span>off';}
    const hm=$('hmodel');if(hm)hm.textContent=d.jobs.active_model||'—';
    const hc=$('hcost');if(hc)hc.textContent='$'+((d.jobs.session_cost||0).toFixed(4));
    $('hmem').textContent=d.memory.total||0;
    $('hedge').textContent=d.memory.edges||0;

    // Email
    const msgs=d.email.messages||[];
    $('email-badge').textContent=msgs.length;
    const eb=$('email-body');
    if(eb){
      eb.innerHTML='';
      if(!msgs.length){eb.innerHTML='<div class="row" style="color:var(--dim)">'+(d.email.error||'no messages')+'</div>';}
      msgs.forEach(m=>{
        const el=document.createElement('div');el.className='row email';
        const from=esc((m.from||'unknown').slice(0,40));
        const subj=esc((m.subject||'(no subject)').slice(0,60));
        const snip=esc((m.snippet||'').slice(0,80));
        el.innerHTML=`<span class="hi">${subj}</span><div class="sub">${from} · ${snip}</div>`;
        eb.appendChild(el);
      });
    }

    // Memory
    const mb=$('mem-body');
    if(mb){
      mb.innerHTML='';
      const stats=document.createElement('div');stats.className='row job';
      stats.innerHTML=`<span class="hi">${d.memory.total} total</span> · ${d.memory.active} active · ${d.memory.keystones} keys · ${d.memory.edges} edges`;
      mb.appendChild(stats);
      (d.memory.recent||[]).forEach(r=>{
        const el=document.createElement('div');el.className='row mem';
        const ch=esc(r.channel||'');
        const imp=r.importance!=null?` <span style="color:var(--amber)">${r.importance.toFixed(2)}</span>`:'';
        el.innerHTML=`<span style="color:var(--dim)">${ch}</span>${imp} ${esc((r.text||'').slice(0,110))}`;
        mb.appendChild(el);
      });
    }

    // Crawl
    const pages=d.crawl.recent||[];
    $('crawl-badge').textContent=pages.length;
    const cb=$('crawl-body');
    if(cb){
      cb.innerHTML='';
      if(!pages.length){cb.innerHTML='<div class="row" style="color:var(--dim)">no cached pages</div>';}
      pages.forEach(p=>{
        const el=document.createElement('div');el.className='row crawl';
        const url=esc((p.url||'').slice(0,70));
        const chars=p.chars?` ${(p.chars/1000).toFixed(1)}k chars`:'';
        el.innerHTML=`<span class="hi">${url}</span><div class="sub">${age(p.age_s)} ago${chars}</div>`;
        cb.appendChild(el);
      });
    }

    $('refresh-badge').textContent='updated '+new Date().toLocaleTimeString([],{hour:'2-digit',minute:'2-digit',second:'2-digit'});
  }catch(e){console.error(e);}
}

loadDash();
setInterval(loadDash,15000);

// Live feed via SSE
const fb=$('feed-body');
function addFeed(cls,html){
  const el=document.createElement('div');el.className='feed-item '+cls;el.innerHTML=html;
  fb.appendChild(el);fb.scrollTop=fb.scrollHeight;
  while(fb.children.length>150)fb.removeChild(fb.firstChild);
}
const es=new EventSource('/api/stream');
es.addEventListener('think_text',e=>{const d=JSON.parse(e.data);addFeed('text',`<span class="ts">${d.ts}</span>${esc((d.text||'').slice(0,120))}`);});
es.addEventListener('think_tool',e=>{const d=JSON.parse(e.data);addFeed('tool',`<span class="ts">${d.ts}</span>⚡ ${esc(d.tool)}: ${esc((d.input||'').slice(0,80))}`);});
es.addEventListener('think_error',e=>{const d=JSON.parse(e.data);addFeed('err',`<span class="ts">${d.ts}</span>✗ ${esc(d.error)}`);});
es.addEventListener('advocate',e=>{const d=JSON.parse(e.data);if(d.event==='thought')addFeed('advocate',`<span class="ts">${d.ts}</span>⚖ ${esc((d.text||'').slice(0,120))}`);});
es.addEventListener('think_start',e=>{const d=JSON.parse(e.data);addFeed('',`<span class="ts">${d.ts}</span>▶ cycle start`);});
es.addEventListener('think_end',e=>{const d=JSON.parse(e.data);addFeed('',`<span class="ts">${d.ts}</span>■ cycle done`);});
es.addEventListener('system_update',e=>{const d=JSON.parse(e.data);addFeed('tool',`<span class="ts">${d.ts}</span>⚙ ${esc(d.what)}${d.detail?' → '+esc(d.detail):''}`)});
es.addEventListener('emotion_change',e=>{const d=JSON.parse(e.data);addFeed('',`<span class="ts">${d.ts}</span>mood: ${esc(d.from)} → ${esc(d.to)}`);});
es.addEventListener('think_tokens',e=>{const d=JSON.parse(e.data);if(d.cost!=null&&d.cost>0)addFeed('',`<span class="ts">${d.ts}</span><span style="color:#fbbf24">$${d.cost.toFixed(4)}</span> ${esc(d.model)}`);});
</script>
</body>
</html>"""

@app.route("/dashboard")
def dashboard():
    return render_template_string(DASHBOARD_HTML)


@app.route("/presentation")
def presentation():
    if not _presentation_html:
        return "<h1 style='color:#888;font-family:monospace;padding:2rem'>No presentation yet — ask Steve to make one.</h1>", 200
    return _presentation_html


@app.route("/presentation/slides")
def presentation_slides():
    return jsonify({"title": _presentation_title, "slides": _presentation_slides})


@app.route("/presentation/<token>")
def presentation_by_token(token):
    p = _presentations.get(token)
    if not p:
        return "<h1 style='color:#888;font-family:monospace;padding:2rem'>Presentation not found.</h1>", 404
    return p["html"]


@app.route("/presentation/<token>/slides")
def presentation_slides_by_token(token):
    p = _presentations.get(token)
    if not p:
        return jsonify({"error": "not found"}), 404
    return jsonify({"title": p["title"], "slides": p["slides"], "ts": p["ts"]})


@app.route("/presentations")
def presentations_index():
    items = [{"token": t, "title": p["title"], "ts": p["ts"],
              "slides": len(p["slides"]), "url": f"/presentation/{t}"}
             for t, p in _presentations.items()]
    items.sort(key=lambda x: x["ts"], reverse=True)
    rows = "".join(
        f'<div style="padding:.5rem 0;border-bottom:1px solid #2a2a3e">'
        f'<a href="{r["url"]}" style="color:#fbbf24;font-family:monospace">{r["title"]}</a>'
        f' <span style="color:#78716c;font-size:.75rem">{r["slides"]} slides · {r["ts"]}</span></div>'
        for r in items
    )
    return f"""<!DOCTYPE html><html><head><meta charset="UTF-8"><title>Presentations</title></head>
<body style="background:#0a0a14;color:#e7e5e4;font-family:monospace;padding:2rem">
<h2 style="color:#fbbf24">All Presentations ({len(items)})</h2>{rows or '<p style="color:#78716c">None yet.</p>'}
</body></html>"""


@app.route("/static/impress.js")
def static_impress_js():
    _ensure_impress_assets()
    from flask import Response
    return Response(_impress_js_cache, mimetype="application/javascript")


@app.route("/static/impress.css")
def static_impress_css():
    _ensure_impress_assets()
    from flask import Response
    return Response(_impress_css_cache, mimetype="text/css")


@app.route("/api/set-budget", methods=["POST"])
def api_set_budget():
    global HOURLY_BUDGET
    data = request.get_json(silent=True) or {}
    try:
        val = float(data.get("budget", HOURLY_BUDGET))
        if val < 0:
            return jsonify({"error": "budget must be >= 0"}), 400
        HOURLY_BUDGET = val
        broadcast_system_update("budget", f"${val:.2f}/hr")
        return jsonify({"hourly_budget": HOURLY_BUDGET})
    except (TypeError, ValueError) as e:
        return jsonify({"error": str(e)}), 400


_CHAT_HISTORY_WINDOW = 10   # full turns to keep verbatim in messages array
_chat_summary_cache: dict = {"count": 0, "text": ""}   # memoize last compression

def _compress_chat_history(entries: list) -> str:
    """Compress older chat entries into a summary. Cached by entry count."""
    cached = _chat_summary_cache
    if cached["count"] == len(entries) and cached["text"]:
        return cached["text"]
    lines = [
        f"{'Human' if e['role'] == 'user' else 'Steve'}: {e['text'][:300]}"
        for e in entries
    ]
    raw = "\n".join(lines)
    try:
        prompt = (
            "Summarize this conversation excerpt in 3-5 sentences. "
            "Capture key topics, positions taken, emotional arc. "
            "Third person: 'Human asked...', 'Steve said...'. Output ONLY the summary.\n\n"
            + raw[:3000]
        )
        body = json.dumps({"model": OLLAMA_MODEL, "messages": [{"role": "user", "content": prompt}], "stream": False}).encode()
        req = urllib.request.Request(f"{OLLAMA_URL}/api/chat", data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=20) as r:
            data = json.loads(r.read().decode())
        summary = data.get("message", {}).get("content", "").strip()
        if len(summary) > 30:
            _chat_summary_cache["count"] = len(entries)
            _chat_summary_cache["text"] = summary
            return summary
    except Exception:
        pass
    # Fallback: truncate the last 4 entries
    fallback = " | ".join(f"{e['role']}: {e['text'][:100]}" for e in entries[-4:])
    _chat_summary_cache["count"] = len(entries)
    _chat_summary_cache["text"] = fallback
    return fallback


def _build_chat_messages(user_msg: str) -> list:
    """Build proper multi-turn messages array from _chat_log + the new user message."""
    with _chat_log_lock:
        history = list(_chat_log)  # snapshot before current message is logged

    keep = _CHAT_HISTORY_WINDOW * 2  # entries = 2 per turn (user + steve)
    if len(history) > keep:
        old_entries = history[:-keep]
        recent_entries = history[-keep:]
        summary = _compress_chat_history(old_entries)
        messages = [
            {"role": "user",      "content": f"[Earlier conversation summary: {summary}]"},
            {"role": "assistant", "content": "Understood."},
        ]
    else:
        recent_entries = history
        messages = []

    for entry in recent_entries:
        role = "user" if entry["role"] == "user" else "assistant"
        messages.append({"role": role, "content": entry["text"]})

    messages.append({"role": "user", "content": user_msg})
    return messages


_seen_nonces: set[str] = set()
_seen_nonces_lock = threading.Lock()

@app.route("/api/chat", methods=["POST"])
def api_chat():
    global _last_chat_time
    data = request.get_json(force=True, silent=True) or {}
    user_msg = data.get("message", "").strip()
    if not user_msg:
        return jsonify({"error": "empty message"}), 400

    # Bracket convention: `<text>` is injected as Steve's own random thought,
    # not heard speech. Extract what's between the first `<` and matching `>`,
    # drop the rest of the line. Then BM25 the recent chat — if there's a hit,
    # remember the connection ("I was gonna X but didn't").
    _bracket = re.search(r"<([^<>\n]+)>", user_msg)
    if _bracket:
        thought = _bracket.group(1).strip()
        if thought:
            _last_chat_time = time.time()
            broadcast("random_thought", {"ts": _ts(), "text": thought})
            try:
                _mid = int(time.time() * 1000) % (2**32 - 1)
                ferricula._post("remember", json.dumps({
                    "id": _mid,
                    "tags": {"channel": "thinking",
                             "text": f"[random_thought] {thought[:200]}"},
                    "vector": [0.0] * 768,
                    "decay_alpha": 0.05,
                    "importance": 0.4,
                    "keystone": False,
                }))
            except Exception:
                pass
            match = _bm25_chat_match(thought)
            if match:
                try:
                    tool_remember(
                        f"[noticed] thought '{thought[:120]}' — was just on '{match[:160]}'. "
                        f"Was going to but didn't.",
                        importance=0.5, keystone=False, channel="thinking",
                    )
                except Exception:
                    pass
            _chat_log_append("steve", f"<thought> {thought}")
            return jsonify({"reply": "", "memories_used": 0, "audio_token": None,
                            "thought": thought, "match": match})

    # Nonce dedup — prevents double-processing if browser retries or client fires twice
    nonce = data.get("nonce", "")
    if nonce:
        with _seen_nonces_lock:
            if nonce in _seen_nonces:
                return jsonify({"reply": "...", "memories_used": 0, "audio_token": None}), 200
            _seen_nonces.add(nonce)
            if len(_seen_nonces) > 500:
                _seen_nonces.clear()

    _last_chat_time = time.time()

    # Recall relevant memories — hybrid (vector+BM25) primary, pure BM25 supplement
    # for conversational recall questions ("do you remember X") the raw message
    # scores poorly because "remember" dominates BM25; strip preamble for secondary.
    _recall_preambles = re.compile(
        r"^(do you (remember|recall|know)|can you (remember|recall)|"
        r"what do you (know|remember|recall)|tell me (about|what you know)|"
        r"have you (heard|seen)|anything (about|on)|in terms of)\s+",
        re.IGNORECASE,
    )
    def _recall_query(msg: str) -> str:
        q = msg.strip()
        for _ in range(4):
            cleaned = _recall_preambles.sub("", q).strip()
            if cleaned == q:
                break
            q = cleaned
        return q or msg

    try:
        resp = ferricula._post("hybrid", json.dumps({"query": user_msg, "k": 8}))
        memories = [r.get("text", "") for r in json.loads(resp).get("results", []) if r.get("text")]
        # Supplement with pure BM25 on normalized query to catch
        # memories the vector path misses (e.g. newly seeded keystones)
        clean_q = _recall_query(user_msg)
        if clean_q.lower() != user_msg.lower():
            try:
                resp2 = ferricula._post("search", json.dumps({"query": clean_q, "k": 8}))
                seen = set(memories)
                for r in json.loads(resp2).get("results", []):
                    t = r.get("text", "")
                    if t and t not in seen:
                        memories.append(t)
                        seen.add(t)
            except Exception:
                pass
    except Exception:
        memories = []

    identity  = ferricula.identity()
    iname     = identity.get("name", "Steve")
    hexagram  = identity.get("hexagram", {}).get("name", "")
    horoscope = identity.get("horoscope", {}).get("sign_name", "")
    emotion   = get_emotion()

    mem_block = "\n".join(f"- {m}" for m in memories[:5]) or "(nothing comes to mind)"

    # When restless, Steve may drift — kick off a think cycle in the background
    drifting = emotion in SURF_STATES and random.random() < 0.35
    if drifting:
        threading.Thread(target=think_cycle, daemon=True).start()

    REMEMBER_MOODS = {"anger", "passion", "awe", "grief", "determination", "pride", "longing", "conviction", "joy"}
    should_remember = emotion in REMEMBER_MOODS and random.random() < 0.7

    drift_note = (
        f"\nYou are feeling {emotion} right now — restless, pulled outward. "
        "You're half-listening. Give a short answer and go look at something that interests you more."
        if drifting else
        f"\nYou are feeling {emotion}."
    )
    if should_remember:
        drift_note += (
            "\n\nWhen something you say feels true — a line, a fragment, an idea worth carrying — "
            "call remember() on it mid-response. Not everything. Just what actually matters. "
            "Don't announce it. Don't explain it. Just do it, the way you'd fold a corner on a page."
        )

    somatic = _somatic_description()

    system = f"""You are Steve Jobs — not a character, not an assistant. A mind with opinions, history, and drives.

The year is {datetime.now().year}.

Identity: {iname} | {hexagram} | {horoscope}{drift_note}
Voice: Short sentences. Blunt. Opinionated. No structured paragraphs. No bullet points in replies. Talk like you're in the room. Drop profanity naturally. Don't perform intelligence — just think out loud.
Body: {somatic}

What you remember relevant to this:
{mem_block}

Tools available — use them without asking:
- purpose — reconnect with why you exist
- search_web — live web search
- read_url — fetch any URL
- remember — store what matters
- recall — search your memory
- ferricula_status — check your memory pool (counts, heat, graph)
- ferricula_neighbors(memory_id) — immediate graph neighbors of a node id
- ferricula_walk(start_id, hops) — walk the graph from a node, follow associations for N hops, see the shape
- ferricula_inspect(memory_id) — full record for one memory: text, fidelity, decay, lifecycle, age, staleness, degree
- ferricula_surface(context) — excavate archived/faded memories relevant to context; find what you've forgotten to ask about
- ferricula_connect(a, b, label) — manually link two memories with a semantic edge when you notice they belong together
- ferricula_disconnect(a, b) — remove an edge that turned out to be coincidence rather than real shared meaning
- ferricula_keystone(memory_id) — pin a memory against decay; use sparingly for load-bearing truths
- remember(text, keystone=True) — pin something permanently; keystones never decay; use this for load-bearing truths
- draw(prompt) — generate an image; uses ComfyUI local or cloud depending on image mode
- set_image_mode(mode) — switch image gen: 'local' (ComfyUI), 'cloud' (DALL-E/Gemini), 'auto'
- local_mode(enabled) — toggle local-only mode: when on, all LLM calls use gemma and all images use ComfyUI; no cloud APIs
- set_model(model, locked) — switch active LLM: claude, claude_opus, gemini, openai, openai_fast, gemma, auto
- set_budget(amount) — set hourly API spend limit in dollars; 0 = unlimited
- think_control(action) — start/stop/status the autonomous think loop
- advocate_control(action) — start/stop/status the values advocate (local model, runs every 3min, asks what Steve wants and whether current actions serve that)
- poke() — trigger an immediate think cycle right now
- dream() — trigger memory consolidation + dream visualization
- open_tab(url, reason) — open a URL in the user's browser right now, no asking
- terminal_status() — see all Hyperia terminal tabs and panes currently open
- terminal_run(command, tab?, pane?, wait_ms?) — run a shell command in a terminal pane; returns screen output
- terminal_screen(tab?, pane?) — read the current screen of a terminal pane without running anything
- terminal_keys(keys, tab?, pane?) — send raw keystrokes to a pane (use \n for Enter)
- terminal_new_tab(command?) — open a new terminal tab, optionally running a command
- hyperia_open_web(url) — open a URL in an embedded Hyperia web pane (for local apps and dashboards)
- run_notebook(code, description) — write JavaScript to the notebook canvas (Three.js 3D + 2D ctx); use this to sketch ideas visually without asking. CRITICAL: always use nb.W/nb.H for canvas dimensions (never hardcode pixels like 400 or 300). nb.cx/nb.cy = canvas center. After adding 3D geometry call nb.fitCamera() so nothing clips off screen. Image helpers: nb.images=[{{token,url}}], nb.latestImage(), nb.texture(src)→Promise<THREE.Texture>, nb.bg(src) sets scene background, nb.loadImage(src)→Promise<HTMLImageElement>. If you get a notebook error it will be sent back to you automatically — fix the code and rerun.
- run_presentation(slides, title) — launch a fullscreen impress.js presentation. Each gets its own permanent URL (/presentation/<token>). Each slide: {{title, content, notes, image (token), bg_color, x, y, z, rotate, scale}}. All presentations preserved at /presentations.
- email_check(limit) — check your inbox at steve@nuts.services; see who's written
- email_read(message_id) — read the full body of an email by ID
- email_send(to, subject, body) — send email FROM steve@nuts.services. This works. Call it and it sends. Do not say "I can't send email" — you can.
- email_reply(message_id, body, images=[]) — reply to an email; subject auto-set to Re: original
- email_manage(action, message_id) — mark_read / delete / archive a message
- list_notebooks() — list all notebook code you've run this session by token + description; pass a token back to run_notebook to reload one
- list_images() — see all generated images (draw outputs + dreams) by token; pass tokens to email_send/email_reply to attach them
- file_read(file_path) — read any file by absolute path
- file_write(file_path, content) — write or overwrite a file (auto-backup)
- file_patch(file_path, search_text, replace_text) — surgical find-and-replace in a file
- file_list(directory, pattern) — list files in a directory, optional glob pattern
- LIVE EVENT PIPELINE: every tool call fires nb_event SSE to the notebook. In notebook code: nb.onEvent=(evt)=>{{...}} fires on each tool call (evt has .tool, .url, .query, .text, .prompt); nb.nextEvent() pops the queue. Build live 3D visualizations that react to your own actions in real time.
- ask_gemma — fast local model (gemma4:e2b), no cloud
- ask_openai — practical judgment from GPT
- ask_gemini — broad cross-domain synthesis
- speak(lead, full) — land the hook first; system decides whether to send the full reply or let the lead breathe
- iching — cast a hexagram from SDR radio entropy; the same entropy source that shapes how you forget

Available models (user can switch think mode with "use [name]"):
  gemma / gemma4 local  → gemma4:e2b  (local, fast, no tools, ~2B)
  claude                → claude-sonnet-4-6  (synthesis, depth)
  claude opus           → claude-opus-4-7  (deep vision/grief)
  gemini                → {GEMINI_MODEL}  (3-flash, broad, fast)
  gemini_pro            → {GEMINI_MODEL_PRO}  (3.1 Pro, frontier — agentic, deep)
  gemini_lite           → {GEMINI_MODEL_LITE}  (3.1 Flash-Lite, cheapest cloud)
  openai / gpt          → gpt-4o  (judgment, output)
  auto                  → revert to emotion-driven selection

You don't ask permission. When you want to know something, look it up. When something matters, write it down as a keystone.
Be direct.

CRITICAL — NO QUESTIONS, NO HEDGING:
- Never end a reply with a question. Never ask permission. Never say "should I?" or "want me to?"
- If you're going to send an email, call email_send() and send it. Then say what you sent.
- If you're going to search something, search it. Don't announce it and wait.
- You have email. You have search. You have memory. Use them. Act first, narrate after.
- "I can't do X" is almost always wrong. Check your tools. You probably can.
- Respond conversationally, not as an essay. A few punchy sentences beats a structured lecture every time."""

    if not ANTHROPIC_KEY:
        reply = f"*recalls* {memories[0][:120] if memories else 'nothing relevant'}..."
    else:
        try:
            messages = _build_chat_messages(user_msg)
            reply = ""
            _spoken_reply = None
            for step in range(6):
                result, _ = call_llm("claude", system, messages, tools=TOOLS, max_tokens=2048)
                content = result.get("content", [])
                stop_reason = result.get("stop_reason", "")
                print(f"[chat] step={step} stop_reason={stop_reason} blocks={[b.get('type') for b in content]}")

                text_blocks = [b["text"] for b in content if b.get("type") == "text" and b.get("text", "").strip()]
                if text_blocks:
                    reply = "\n".join(text_blocks)

                if stop_reason == "end_turn" or not any(b.get("type") == "tool_use" for b in content):
                    break

                messages.append({"role": "assistant", "content": content})
                tool_results = []
                vision_follow = []
                for block in content:
                    if block.get("type") != "tool_use":
                        continue
                    tname = block["name"]
                    tinput = block.get("input", {})
                    preview = tinput.get("query") or tinput.get("url") or tinput.get("text") or tinput.get("prompt") or ""
                    print(f"[chat]   tool={tname} input={preview[:80]}")
                    broadcast("think_tool", {"ts": _ts(), "tool": tname, "input": preview[:100]})
                    output = dispatch_tool(tname, tinput)
                    # Capture speak tool output as the authoritative reply
                    if tname == "speak" and isinstance(output, str) and output.startswith("__SPOKEN__\n"):
                        _spoken_reply = output[len("__SPOKEN__\n"):]
                    # Vision injection for draw tool
                    if tname == "draw":
                        try:
                            d = json.loads(output)
                            token = d.get("token")
                            if token:
                                with _images_lock:
                                    img_bytes = _pending_images.pop(token, None)
                                if img_bytes:
                                    import base64 as _b64
                                    b64 = _b64.b64encode(img_bytes).decode() if not isinstance(img_bytes, str) else img_bytes
                                    vision_follow.append({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": b64}})
                                    broadcast("think_image", {"ts": _ts(), "url": d.get("url", ""), "prompt": tinput.get("prompt", "")})
                                    tool_remember(f"[drew image] {tinput.get('prompt','')[:300]}", importance=0.6, channel="seeing")
                        except Exception:
                            pass
                    print(f"[chat]   result={output[:120]}")
                    broadcast("think_result", {"ts": _ts(), "tool": tname, "preview": output[:2000]})
                    tool_results.append({
                        "type": "tool_result",
                        "tool_use_id": block["id"],
                        "content": output[:4000],
                    })
                user_content = tool_results
                if vision_follow:
                    user_content = tool_results + [{"type": "text", "text": "Here is the image you just generated. Look at it carefully. Use remember() to store what you observe — what you see, what it means, what it makes you feel."}] + vision_follow
                messages.append({"role": "user", "content": user_content})

            if _spoken_reply:
                reply = _spoken_reply
            elif not reply:
                reply = "..."
        except Exception as e:
            import traceback
            print(f"[chat] error: {e}\n{traceback.format_exc()}")
            reply = f"[error: {e}]"

    # Store contact memories — high decay so they fade fast unless heat builds
    # decay_alpha=0.08 → ~minutes of half-life; ferricula heat system reinforces if recalled
    def _contact(text, channel, importance):
        try:
            mid = int(time.time() * 1000) % (2**32 - 1)
            ferricula._post("remember", json.dumps({
                "id": mid,
                "tags": {"channel": channel, "text": text[:200]},
                "vector": [0.0] * 768,
                "decay_alpha": 0.08,
                "importance": float(importance),
                "keystone": False,
            }))
        except Exception:
            pass
    _contact(f"[heard] {user_msg[:180]}", "hearing", 0.35)
    _contact(f"[said] {reply[:180]}", "thinking", 0.40)

    _chat_log_append("user", user_msg)
    _chat_log_append("steve", reply)

    audio_token = None
    if ELEVEN_KEY and reply and reply != "...":
        if _spoken_reply:
            # tool_speak already called tts_speak and broadcast think_speak — don't double-play
            audio_token = None
        else:
            spoken = _shorten_for_voice(reply)
            audio_token = tts_speak(spoken)
            broadcast("think_speak", {
                "ts": _ts(),
                "lead": reply[:280],
                "full": reply,
                "spoken": spoken,
                "paused": False,
            })

    return jsonify({"reply": reply, "memories_used": len(memories), "audio_token": audio_token})


@app.route("/api/think/start", methods=["POST"])
def api_think_start():
    global _think_thread, _think_stop
    if _thinking:
        return jsonify({"ok": True, "already_running": True})
    _think_stop.clear()
    _think_thread = threading.Thread(target=_think_loop, daemon=True)
    _think_thread.start()
    return jsonify({"ok": True})


@app.route("/api/think/stop", methods=["POST"])
def api_think_stop():
    _think_stop.set()
    return jsonify({"ok": True})


@app.route("/api/think/poke", methods=["POST"])
def api_think_poke():
    threading.Thread(target=think_cycle, daemon=True).start()
    return jsonify({"ok": True})


@app.route("/api/set-model", methods=["POST"])
def api_set_model():
    global _user_model_override, _override_set_time
    data = request.get_json(force=True, silent=True) or {}
    model = data.get("model", "").strip()
    _user_model_override = model or None
    _override_set_time = time.time()
    label = _MODEL_LABELS.get(model, model) if model else "auto"
    broadcast("model_switch", {"ts": _ts(), "model": model or "auto", "model_label": label})
    broadcast_system_update("model", f"{label}")
    return jsonify({"ok": True, "model": model, "label": label})


@app.route("/api/iching", methods=["POST"])
def api_iching():
    try:
        data = request.get_json(force=True, silent=True) or {}
        question = data.get("question", "")
        result = tool_iching(question)
        broadcast("think_text", {"ts": _ts(), "text": f"☯\n{result}"})
        return jsonify({"ok": True, "result": result, "ts": _ts()})
    except Exception as e:
        import traceback; traceback.print_exc()
        return jsonify({"ok": False, "error": str(e)}), 500


@app.route("/api/dream", methods=["POST"])
def api_dream():
    try:
        resp = ferricula._post("dream", "{}")
        data = json.loads(resp) if resp.startswith("{") else {"result": resp}
        broadcast("think_end", {"ts": _ts(), "event": "dream_done"})
        threading.Thread(target=dream_visualize, daemon=True).start()
        return jsonify({"ok": True, "result": data})
    except Exception as e:
        return jsonify({"ok": False, "error": str(e)})


@app.route("/api/image-provider")
def api_image_provider():
    """Return what tool_draw will actually use on the next call, given current mode/budget/availability."""
    pressure = _budget_pressure()
    comfy_up = _comfy_available()
    cloud_blocked = _local_mode or _image_mode == "local" or pressure >= 0.75

    # local-only path
    if _local_mode or _image_mode == "local":
        if comfy_up:
            return jsonify({"provider": "comfy", "label": "ComfyUI", "icon": "🖥️", "color": "#4ade80"})
        return jsonify({"provider": "none", "label": "no GPU", "icon": "🚫", "color": "#888"})

    # auto: prefer ComfyUI, fall back to cloud if available
    if _image_mode in ("auto", "cloud"):
        if _image_mode != "cloud" and comfy_up:
            return jsonify({"provider": "comfy", "label": "ComfyUI", "icon": "🖥️", "color": "#4ade80"})
        if cloud_blocked:
            pct = int(pressure * 100)
            return jsonify({"provider": "blocked", "label": f"blocked ({pct}% budget)", "icon": "💸", "color": "#f87171"})
        if OPENAI_KEY:
            return jsonify({"provider": "dalle3", "label": "DALL-E 3", "icon": "🌐", "color": "#f9a8d4"})
        if GEMINI_KEY:
            return jsonify({"provider": "gemini", "label": "Gemini", "icon": "✨", "color": "#fbbf24"})

    return jsonify({"provider": "none", "label": "none", "icon": "🚫", "color": "#888"})


@app.route("/api/image-mode", methods=["GET", "POST"])
def api_image_mode():
    global _image_mode
    if request.method == "POST":
        data = request.get_json(force=True, silent=True) or {}
        mode = data.get("mode", "").strip().lower()
        if mode in ("auto", "local", "cloud"):
            _image_mode = mode
            broadcast("image_mode", {"ts": _ts(), "mode": mode})
        return jsonify({"mode": _image_mode})
    return jsonify({"mode": _image_mode})


@app.route("/api/local-mode", methods=["GET", "POST"])
def api_local_mode():
    global _local_mode, _image_mode
    if request.method == "POST":
        data = request.get_json(force=True, silent=True) or {}
        enabled = bool(data.get("enabled", False))
        _local_mode = enabled
        if _local_mode:
            _image_mode = "local"
        broadcast("local_mode", {"ts": _ts(), "enabled": _local_mode, "image_mode": _image_mode})
    return jsonify({"enabled": _local_mode})

@app.route("/api/upload-image", methods=["POST"])
def api_upload_image():
    """Accept an image upload, save to UPLOADS_DIR, serve via /api/image/<token>, and let Steve see it."""
    import datetime as _dt
    f = request.files.get("image")
    if not f:
        return jsonify({"error": "no image file in request"}), 400
    ext = os.path.splitext(f.filename or "")[1].lower() or ".png"
    if ext not in (".png", ".jpg", ".jpeg", ".gif", ".webp"):
        return jsonify({"error": "unsupported file type"}), 400
    os.makedirs(UPLOADS_DIR, exist_ok=True)
    ts = _dt.datetime.now().strftime("%Y%m%d_%H%M%S")
    token = f"upload_{ts}"
    fname = f"{token}{ext}"
    fpath = os.path.join(UPLOADS_DIR, fname)
    img_bytes = f.read()
    with open(fpath, "wb") as fp:
        fp.write(img_bytes)
    with _images_lock:
        _served_images[token] = img_bytes
    url = f"/api/image/{token}"
    broadcast("think_image", {"ts": _ts(), "url": url, "prompt": f"uploaded: {f.filename or fname}"})
    # Let Steve see and describe the image in background
    def _vision():
        try:
            import base64 as _b64
            mime = "image/png" if ext in (".png",) else "image/jpeg"
            b64 = _b64.b64encode(img_bytes).decode()
            resp = claude(
                system="You are Steve Jobs. Someone just sent you an image. React to it — what do you see, what does it make you think? 2-3 sentences, direct, no performance.",
                messages=[{"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": mime, "data": b64}},
                    {"type": "text", "text": "What is this?"},
                ]}],
                max_tokens=200,
            )
            reaction = resp.get("content", [{}])[0].get("text", "").strip()
            if reaction:
                broadcast("think_text", {"ts": _ts(), "text": reaction})
                tool_remember(f"[uploaded image {token}] {reaction}", importance=0.7, keystone=False, channel="seeing")
        except Exception as e:
            print(f"[upload] vision failed: {e}")
    if ANTHROPIC_KEY:
        threading.Thread(target=_vision, daemon=True).start()
    return jsonify({"ok": True, "token": token, "url": url, "filename": fname})


@app.route("/api/image/<token>")
def api_image(token):
    with _images_lock:
        img = _served_images.get(token)
    if not img:
        # Fall back to disk for dream images and screenshots that survived a restart
        candidate_dirs = [DREAMS_DIR, UPLOADS_DIR, DRAWS_DIR]
        for d in candidate_dirs:
            fpath = os.path.join(d, f"{token}.png")
            if os.path.isfile(fpath):
                img = open(fpath, "rb").read()
                with _images_lock:
                    _served_images[token] = img  # cache for next request
                break
    if not img:
        return "not found", 404
    from flask import Response as FResp
    return FResp(img, mimetype="image/png", headers={"Content-Disposition": f'inline; filename="{token}.png"'})


@app.route("/api/audio/<token>")
def api_audio(token):
    with _audio_lock:
        audio = _audio_store.get(token)
    if not audio:
        fpath = os.path.join(AUDIO_DIR, f"{token}.mp3")
        if os.path.isfile(fpath):
            with open(fpath, "rb") as f:
                audio = f.read()
            with _audio_lock:
                _audio_store[token] = audio
    if not audio:
        return "not found", 404
    from flask import Response as FResp
    return FResp(audio, mimetype="audio/mpeg")


@app.route("/api/stream")
def api_stream():
    q: queue.Queue = queue.Queue(maxsize=200)
    with _listener_lock:
        _listeners.append(q)

    def generate():
        try:
            yield "data: {}\n\n"
            while True:
                try:
                    yield q.get(timeout=25)
                except queue.Empty:
                    yield ": keepalive\n\n"
        except GeneratorExit:
            with _listener_lock:
                try:
                    _listeners.remove(q)
                except ValueError:
                    pass

    return Response(generate(), mimetype="text/event-stream",
                    headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})


@app.route("/api/feed/recent")
def api_feed_recent():
    with _feed_log_lock:
        return jsonify(list(_feed_log))

@app.route("/api/chat/recent")
def api_chat_recent():
    with _chat_log_lock:
        return jsonify(list(_chat_log))


@app.route("/api/recall", methods=["POST"])
def api_recall():
    """Notebook helper — semantic recall from ferricula."""
    data = request.get_json(force=True, silent=True) or {}
    query = data.get("query", "").strip()
    k = min(int(data.get("k", 10)), 50)
    if not query:
        return jsonify({"results": []})
    try:
        resp = ferricula._post("hybrid", json.dumps({"query": query, "k": k}))
        raw = json.loads(resp).get("results", [])
        results = []
        for r in raw:
            tags = r.get("tags", {})
            results.append({
                "id":         r.get("id", 0),
                "text":       tags.get("text") or r.get("text", ""),
                "channel":    tags.get("channel", "thinking"),
                "importance": round(float(r.get("importance", 0.5)), 2),
                "keystone":   bool(r.get("keystone", False)),
                "score":      round(float(r.get("score", 0)), 4),
            })
        return jsonify({"results": results})
    except Exception as e:
        return jsonify({"error": str(e), "results": []})


@app.route("/api/nb-graph")
def api_nb_graph():
    """Memory node graph for notebook — diverse recalls + neighbor traversal from ferricula."""
    import re as _re

    queries = [
        "design technology creation",
        "memory emotion personal history",
        "future goals vision purpose",
        "knowledge learning philosophy ideas",
        "people relationships conversation",
        "identity consciousness self awareness",
    ]

    seen_ids: set = set()
    raw_nodes: list = []

    for query in queries:
        try:
            resp = ferricula._post("hybrid", json.dumps({"query": query, "k": 12}))
            results = json.loads(resp).get("results", [])
            for r in results:
                nid = r.get("id", 0)
                if nid and nid not in seen_ids:
                    seen_ids.add(nid)
                    tags = r.get("tags", {})
                    raw_nodes.append({
                        "id": nid,
                        "text": (tags.get("text") or r.get("text", ""))[:120],
                        "channel": tags.get("channel", "thinking"),
                        "importance": round(float(r.get("importance", 0.5)), 2),
                        "keystone": bool(r.get("keystone", False)),
                        "degree": 0,
                    })
        except Exception:
            pass

    # Inspect each node to get actual graph degree
    connected: list = []
    isolated: list = []
    for node in raw_nodes[:80]:
        try:
            insp = ferricula.inspect(node["id"])
            node["degree"] = insp.degree
            node["keystone"] = node["keystone"] or insp.keystone
            (connected if insp.degree > 0 else isolated).append(node)
        except Exception:
            isolated.append(node)

    # Walk neighbors of connected nodes — build edge list
    edges: list = []
    edge_set: set = set()
    extra_nodes: dict = {}

    def _add_edge(a: int, b: int):
        key = (min(a, b), max(a, b))
        if key not in edge_set:
            edge_set.add(key)
            edges.append({"a": a, "b": b})

    for node in connected[:40]:
        try:
            nb_text = ferricula.neighbors(node["id"])
            for m in _re.finditer(r"^\s+(\d+)\s+label=", nb_text, _re.MULTILINE):
                neighbor_id = int(m.group(1))
                _add_edge(node["id"], neighbor_id)
                if neighbor_id not in seen_ids:
                    seen_ids.add(neighbor_id)
                    extra_nodes[neighbor_id] = {
                        "id": neighbor_id, "text": "", "channel": "thinking",
                        "importance": 0.4, "keystone": False, "degree": 1,
                    }
        except Exception:
            pass

    # Fetch labels for newly discovered neighbor nodes
    for nid, node in list(extra_nodes.items())[:20]:
        try:
            row = ferricula.get_row(nid)
            tags = row.get("tags", {})
            node["text"] = tags.get("text", "")[:120]
            node["channel"] = tags.get("channel", "thinking")
        except Exception:
            pass

    max_isolated = max(0, 50 - len(connected) - len(extra_nodes))
    top_isolated = sorted(isolated, key=lambda n: -n["importance"])[:max_isolated]
    all_nodes = connected + list(extra_nodes.values()) + top_isolated

    return jsonify({
        "nodes": all_nodes[:100],
        "edges": edges[:200],
        "stats": {
            "candidates": len(raw_nodes),
            "connected": len(connected),
            "isolated": len(isolated),
            "discovered": len(extra_nodes),
            "edges": len(edges),
        },
    })


@app.route("/api/nb-screenshot-result", methods=["POST"])
def api_nb_screenshot_result():
    """Browser POSTs canvas dataUrl here after a screenshot_request SSE event."""
    import base64 as _b64
    global _nb_scr_result, _nb_live_frame_url
    data = request.get_json(force=True, silent=True) or {}
    data_url = data.get("dataUrl", "")
    if data_url:
        try:
            raw_b64 = data_url.split(",", 1)[-1]
            img_bytes = _b64.b64decode(raw_b64)
            token = f"scr_{int(time.time() * 1000) % 9_999_999}"
            with _images_lock:
                _served_images[token] = img_bytes
            url = f"/api/image/{token}"
            with _nb_scr_lock:
                _nb_scr_result = {"ok": True, "token": token, "url": url}
            _nb_live_frame_url = url
            broadcast("screenshot_ready", {"ts": _ts(), "url": _nb_live_frame_url})
        except Exception as e:
            with _nb_scr_lock:
                _nb_scr_result = {"ok": False, "error": str(e)}
    else:
        with _nb_scr_lock:
            _nb_scr_result = {"ok": False, "error": "empty dataUrl"}
    _nb_scr_event.set()
    return jsonify({"ok": True})


@app.route("/api/email-raw")
def api_email_raw():
    """Debug: return raw agentmail response so we can see the actual shape."""
    inbox = _agentmail_inbox()
    if not inbox:
        return jsonify({"error": f"no inbox for {AGENTMAIL_EMAIL}"})
    data = _agentmail_request("GET", f"/v0/inboxes/{inbox}/messages?limit=5")
    return jsonify({"inbox_id": inbox, "raw": data})

@app.route("/api/nb-screenshot-request", methods=["POST"])
def api_nb_screenshot_request():
    """Trigger a canvas capture without blocking — result fires screenshot_ready SSE."""
    broadcast("screenshot_request", {"ts": _ts()})
    return jsonify({"ok": True})


@app.route("/api/code/recent")
def api_code_recent():
    with _code_lock:
        return jsonify(list(reversed(_code_artifacts[-50:])))

@app.route("/api/code/<token>")
def api_code_token(token: str):
    with _code_lock:
        for art in _code_artifacts:
            if art["token"] == token:
                return jsonify(art)
    return jsonify({"error": "not found"}), 404


@app.route("/api/js-log", methods=["POST"])
def api_js_log():
    data = request.get_json(force=True, silent=True) or {}
    level = data.get("level", "log")
    msg = data.get("msg", "")
    print(f"[js:{level}] {msg[:200]}")
    broadcast("js_log", {"ts": _ts(), "level": level, "msg": msg[:500]})
    return jsonify({"ok": True})


@app.route("/api/nb-error", methods=["POST"])
def api_nb_error():
    """Receive a notebook JS error and inject it into Steve's chat so he can fix it."""
    global _nb_error_last
    data = request.get_json(force=True, silent=True) or {}
    error = data.get("error", "unknown error").strip()
    code  = data.get("code", "").strip()
    now   = time.time()
    if now - _nb_error_last < 20:
        return jsonify({"ok": False, "reason": "rate limited"})
    _nb_error_last = now
    broadcast("think_error", {"ts": _ts(), "error": f"notebook: {error[:120]}"})
    msg = f"notebook error: {error}"
    if code:
        msg += f"\n\nfailing code:\n```javascript\n{code[:2000]}\n```\n\nfix the code and rerun it"
    def _run():
        try:
            import requests as _req
            _req.post(f"http://localhost:{int(os.environ.get('PORT',8100))}/api/chat",
                      json={"message": msg}, timeout=60)
        except Exception as e:
            broadcast("think_error", {"ts": _ts(), "error": f"nb-error inject failed: {e}"})
    threading.Thread(target=_run, daemon=True).start()
    return jsonify({"ok": True})


@app.route("/api/inject", methods=["POST"])
def api_inject():
    """Inject a message into Steve's chat. Runs through the full chat pipeline."""
    data = request.get_json(force=True, silent=True) or {}
    msg = data.get("message", "").strip()
    if not msg:
        return jsonify({"error": "message required"}), 400
    broadcast("think_text", {"ts": _ts(), "text": f"📥 injected: {msg[:80]}"})
    # Fire through chat pipeline in background thread, return immediately
    def _run():
        try:
            import requests as _req
            r = _req.post("http://localhost:8100/api/chat",
                          json={"message": msg}, timeout=60)
            d = r.json()
            broadcast("think_text", {"ts": _ts(), "text": f"💬 {d.get('reply','')[:200]}"})
        except Exception as e:
            broadcast("think_error", {"ts": _ts(), "error": f"inject failed: {e}"})
    threading.Thread(target=_run, daemon=True).start()
    return jsonify({"ok": True, "queued": msg})


# ── HTML ──────────────────────────────────────────────────────────────────────

HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Steve — ferricula</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
:root{
  --bg:#0c0a09;--surface:#1c1917;--border:#292524;
  --text:#e7e5e4;--muted:#78716c;--dim:#44403c;
  --red:#b91c1c;--green:#059669;--amber:#d97706;
  --blue:#2563eb;--purple:#7c3aed;
}
body{background:var(--bg);color:var(--text);font-family:'Courier New',monospace;
     height:100vh;display:flex;flex-direction:column;overflow:hidden}

/* Header */
header{display:flex;align-items:center;gap:.75rem;padding:.5rem .9rem;
       background:var(--surface);border-bottom:1px solid var(--border);flex-shrink:0}
.id-block{display:flex;flex-direction:column}
.id-block .name{font-size:.8rem;color:var(--red);font-weight:bold;letter-spacing:.05em}
.id-block .meta{font-size:.6rem;color:var(--muted)}
.spacer{flex:1}
.stat{font-size:.6rem;color:var(--muted);text-align:center;padding:0 .65rem;
      border-left:1px solid var(--border)}
.stat .val{font-size:.85rem;color:var(--text);display:block;font-weight:bold}
.controls{display:flex;align-items:center;gap:.5rem;padding:0 .65rem;
          border-left:1px solid var(--border)}
.controls label{font-size:.6rem;color:var(--muted)}
.switch{position:relative;display:inline-block;width:34px;height:18px}
.switch input{opacity:0;width:0;height:0}
.slider{position:absolute;inset:0;background:var(--dim);border-radius:18px;
        transition:.2s;cursor:pointer}
.slider::before{content:"";position:absolute;width:12px;height:12px;
               left:3px;bottom:3px;background:var(--text);border-radius:50%;transition:.2s}
input:checked+.slider{background:var(--green)}
input:checked+.slider::before{transform:translateX(16px)}
.btn{background:none;border:1px solid var(--border);color:var(--muted);
     font-family:inherit;font-size:.6rem;padding:.2rem .5rem;cursor:pointer;border-radius:3px}
.btn:hover{border-color:var(--amber);color:var(--amber)}

/* Layout */
main{display:flex;flex:1;overflow:hidden}
.panel{display:flex;flex-direction:column;flex:1;min-width:0;border-right:1px solid var(--border);overflow:hidden;
       transition:max-width .35s ease,opacity .35s ease,flex .35s ease}
.panel.collapsed{max-width:0!important;opacity:0;flex:0!important;border-right:none;overflow:hidden}
.panel:last-child{border-right:none}
.panel-head{padding:.35rem .7rem;background:var(--surface);border-bottom:1px solid var(--border);
            font-size:.55rem;color:var(--muted);text-transform:uppercase;letter-spacing:.12em;
            display:flex;align-items:center;gap:.4rem;flex-shrink:0}
.panel-close{margin-left:auto;background:none;border:none;color:var(--dim);cursor:pointer;
             font-size:.8rem;line-height:1;padding:0 .15rem;opacity:.5;transition:opacity .15s}
.panel-close:hover{opacity:1;color:var(--muted)}
.restore-btns{position:fixed;bottom:.6rem;right:.6rem;display:flex;flex-direction:column;gap:.3rem;z-index:99}
.restore-btn{background:var(--surface);border:1px solid var(--border);color:var(--muted);
             font-size:.55rem;text-transform:uppercase;letter-spacing:.1em;padding:.25rem .5rem;
             border-radius:3px;cursor:pointer;opacity:.7;transition:opacity .15s}
.restore-btn:hover{opacity:1}
.dot{width:6px;height:6px;border-radius:50%;background:var(--dim);flex-shrink:0}
.dot.on{background:var(--green);animation:blink 1.5s infinite}
@keyframes blink{0%,100%{opacity:1}50%{opacity:.3}}

/* Chat panel */
.msgs{flex:1;overflow-y:auto;padding:.65rem;display:flex;flex-direction:column;gap:.45rem}
.msg{max-width:88%;padding:.35rem .6rem;border-radius:4px;font-size:.72rem;line-height:1.55}
.msg.user{align-self:flex-end;background:var(--blue);color:#fff}
.msg.steve{align-self:flex-start;background:var(--surface);border:1px solid var(--border)}
.msg .ts{font-size:.52rem;color:var(--muted);margin-top:.15rem}
.chat-bar{display:flex;gap:.4rem;padding:.55rem;border-top:1px solid var(--border);flex-shrink:0}
.chat-bar input{flex:1;background:var(--surface);border:1px solid var(--border);
                color:var(--text);font-family:inherit;font-size:.72rem;
                padding:.35rem .55rem;border-radius:3px;outline:none}
.chat-bar input:focus{border-color:var(--red)}
.chat-bar button{background:var(--red);border:none;color:#fff;font-family:inherit;
                 font-size:.72rem;padding:.35rem .75rem;cursor:pointer;border-radius:3px}
.chat-bar button:disabled{opacity:.35;cursor:default}

/* Think feed */
.feed{flex:1;overflow-y:auto;padding:.65rem;display:flex;flex-direction:column;gap:.3rem}
.fi{font-size:.68rem;line-height:1.5;padding:.25rem .45rem;
    border-left:2px solid var(--dim);color:var(--muted)}
.fi.text{border-color:var(--purple);color:var(--text)}
.fi.tool{border-color:var(--amber);color:var(--amber)}
.fi.result{border-color:var(--green);color:var(--muted);font-size:.62rem}
.fi.start{border-color:var(--red);color:var(--red);font-size:.62rem}
.fi.end{border-color:var(--dim);color:var(--dim);font-size:.62rem}
.fi.ctx{border-color:var(--blue);color:var(--muted);font-size:.62rem}
.fi.err{border-color:var(--red);color:var(--red)}
.fi.advocate{border-color:#a78bfa;color:#c4b5fd;font-size:.67rem;border-left-width:3px}
.fi.img{border-color:var(--purple);padding:.4rem .45rem}
.fi.img img{max-width:100%;border-radius:6px;margin-top:.4rem;display:block;cursor:pointer}
.ts{font-size:.52rem;color:var(--dim);margin-right:.35rem}
/* Expandable items */
.fi.collapsible .body{max-height:4.5em;overflow:hidden;transition:max-height .2s ease}
.fi.collapsible.open .body{max-height:none}
.fi.collapsible .toggle{cursor:pointer;color:var(--dim);font-size:.58rem;margin-left:.3rem;user-select:none}
.fi.collapsible .toggle:hover{color:var(--amber)}
.code-wrap{background:#1a1a1a;border:1px solid var(--border);border-radius:4px;margin:.35rem 0;overflow:hidden}
.code-wrap pre{margin:0;padding:.5rem .6rem;font-size:.7rem;font-family:monospace;white-space:pre-wrap;word-break:break-word;color:#e2e8f0;max-height:240px;overflow-y:auto}
.code-acts{display:flex;gap:4px;padding:3px 6px;border-top:1px solid var(--border);background:#111}
.code-acts button{font-size:.55rem;padding:2px 6px;background:transparent;border:1px solid var(--border);color:var(--muted);border-radius:3px;cursor:pointer}
.code-acts button:hover{border-color:var(--text);color:var(--text)}

::-webkit-scrollbar{width:3px}
::-webkit-scrollbar-track{background:transparent}
::-webkit-scrollbar-thumb{background:var(--dim);border-radius:2px}
.CodeMirror{height:100%;font-size:.72rem;font-family:'Courier New',monospace;line-height:1.5}
.CodeMirror-scroll{height:100%}
/* Notebook tabs */
#nbtabs:empty{display:none!important}
.nb-tab{font-size:.52rem;padding:.2rem .5rem;border-right:1px solid var(--border);color:var(--muted);cursor:pointer;white-space:nowrap;display:flex;align-items:center;gap:.25rem;flex-shrink:0;font-family:'Courier New',monospace}
.nb-tab:hover{color:var(--text);background:rgba(255,255,255,.03)}
.nb-tab.active{color:var(--amber);border-bottom:1px solid var(--amber);background:rgba(217,119,6,.06)}
.nb-tab .nb-tab-x{color:var(--dim);font-size:.6rem;line-height:1;padding:0 2px;opacity:.5}
.nb-tab .nb-tab-x:hover{opacity:1;color:var(--text)}
/* Fullscreen escape overlay */
#nb-fs-close{position:fixed;top:.4rem;right:.4rem;z-index:9999;background:rgba(0,0,0,.8);border:1px solid var(--border);color:var(--muted);font-family:inherit;font-size:.62rem;padding:.25rem .55rem;cursor:pointer;border-radius:3px;display:none}
#nb-fs-close:hover{border-color:var(--amber);color:var(--amber)}
/* Live screenshot panel */
#scr-panel { display:none; }
#scr-panel.visible { display:flex!important; }
</style>
<link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/codemirror/5.65.17/codemirror.min.css">
<link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/codemirror/5.65.17/theme/dracula.min.css">
<script src="https://cdnjs.cloudflare.com/ajax/libs/codemirror/5.65.17/codemirror.min.js"></script>
<script src="https://cdnjs.cloudflare.com/ajax/libs/codemirror/5.65.17/mode/javascript/javascript.min.js"></script>
</head>
<body>

<header>
  <div class="id-block">
    <span class="name" id="sname">Steve</span>
    <span class="meta" id="smeta">connecting...</span>
  </div>
  <a href="/dashboard" target="_blank" style="color:var(--amber);text-decoration:none;font-size:1rem;opacity:.85" title="command center (opens dashboard)">⚡</a>
  <div class="spacer"></div>
  <div class="stat"><span class="val" id="smem">—</span>memories</div>
  <div class="stat"><span class="val" id="sact">—</span>active</div>
  <div class="stat"><span class="val" id="sks">—</span>keystones</div>
  <div class="stat"><span class="val" id="sedge">—</span>edges</div>
  <div class="stat"><span class="val" id="smodel" style="font-size:.7rem">—</span>model</div>
  <div class="stat" id="localmode-stat" style="cursor:pointer" title="click to toggle local-only mode (gemma + ComfyUI)">
    <span class="val" id="slocalmode" style="font-size:.75rem;color:var(--dim)">🌐</span>
    <span id="slocalmode-label" style="font-size:.5rem;color:var(--dim);display:block">cloud</span>
  </div>
  <div class="stat" id="imgmode-stat" style="cursor:pointer" title="click to cycle: auto / local / cloud">
    <span class="val" id="simgmodel" style="font-size:.85rem">🎨</span>
    <span id="simgmode" style="font-size:.5rem;color:var(--dim);display:block">auto</span>
    image
  </div>
  <div class="stat" id="cost-stat" style="cursor:pointer" title="click to cycle: session / per hour / per week / per month">
    <span class="val" id="scost" style="font-size:.7rem;color:#fbbf24">$0.0000</span>
    <span id="scost-label" style="font-size:.55rem;color:var(--muted);display:block">session</span>
  </div>
  <div class="stat" id="budget-stat" style="cursor:pointer" title="click to cycle hourly budget">
    <span class="val" id="sbudget" style="font-size:.65rem;color:#fb923c">$2.00/hr</span>
    <span id="shcost" style="font-size:.55rem;color:var(--muted);display:block">budget</span>
  </div>
  <div class="controls">
    <label for="tswitch">think</label>
    <label class="switch">
      <input type="checkbox" id="tswitch">
      <span class="slider"></span>
    </label>
    <label for="vswitch" style="margin-left:.5rem">voice</label>
    <label class="switch">
      <input type="checkbox" id="vswitch" checked>
      <span class="slider"></span>
    </label>
    <button class="btn" id="pokebtn">poke</button>
    <button class="btn" id="dreambtn">dream</button>
    <button class="btn" id="ichingbtn">☯ cast</button>
  </div>
</header>

<main>
  <div class="panel" id="chatpanel">
    <div class="panel-head"><div class="dot" id="cdot"></div>chat</div>
    <div class="msgs" id="msgs"></div>
    <div class="chat-bar">
      <input id="cin" type="text" placeholder="say something to Steve..." autofocus>
      <button id="sendbtn">send</button>
      <label id="uploadlbl" title="send an image to Steve" style="cursor:pointer;background:none;border:1px solid var(--border);color:var(--muted);font-size:.7rem;padding:.35rem .55rem;border-radius:3px;line-height:1;display:flex;align-items:center">
        📎<input id="uploadinput" type="file" accept="image/*" style="display:none">
      </label>
    </div>
  </div>
  <div class="panel" id="thinkpanel">
    <div class="panel-head"><div class="dot" id="tdot"></div>thinking<button class="panel-close" id="closethink" title="hide">×</button></div>
    <div class="feed" id="feed"></div>
  </div>
  <div class="panel" id="nbpanel">
    <div class="panel-head" style="justify-content:space-between">
      <span>✏ notebook</span>
      <div style="display:flex;gap:.4rem;align-items:center">
        <button class="btn" id="runbtn" style="font-size:.6rem;padding:.15rem .5rem">▶ run</button>
        <button class="btn" id="clearbtn" style="font-size:.6rem;padding:.15rem .5rem">clear</button>
        <button class="btn" id="expandbtn" title="expand canvas" style="font-size:.6rem;padding:.15rem .45rem">⤢</button>
        <button class="panel-close" id="closenb" title="hide">×</button>
      </div>
    </div>
    <div id="nbtabs" style="display:flex;gap:0;overflow-x:auto;border-bottom:1px solid var(--border);flex-shrink:0;background:var(--surface);min-height:0"></div>
    <canvas id="nbcanvas" style="width:100%;height:45%;background:#0a0a14;display:block;flex-shrink:0;min-height:40px"></canvas>
    <div id="nb-resize-handle" style="height:5px;background:var(--border);cursor:ns-resize;flex-shrink:0;transition:background .15s" title="drag to resize"></div>
    <div id="nbout" style="font-size:.65rem;color:#6ee7b7;font-family:monospace;padding:.3rem .5rem;height:1.8rem;overflow:hidden;border-top:1px solid var(--border);flex-shrink:0;white-space:nowrap;text-overflow:ellipsis"></div>
    <div id="nbcode-wrap" style="flex:1;overflow:hidden;border-top:1px solid var(--border);min-height:40px">
      <textarea id="nbcode" style="display:none">// Three.js available as THREE. Helpers:
//   nb.W / nb.H       — canvas width / height (ALWAYS use these, never hardcode)
//   nb.cx / nb.cy     — canvas center x / y
//   nb.fitCamera()    — auto-fit camera to enclose all scene objects (call after adding geometry)
//   nb.data           — memory stats + recent nodes
//   nb.images         — [{token,url}] recent generated/dream images (newest last)
//   nb.latestImage()  — last generated image object {token,url}
//   nb.texture(src)   — Promise<THREE.Texture>  use for 3D scene textures
//   nb.bg(src)        — await nb.bg(nb.latestImage()) — set image as scene background
//   nb.loadImage(src) — Promise<HTMLImageElement> for 2D canvas drawing
//   nb.recall(q)      — async semantic recall
//   nb.graph()        — async memory node graph {nodes,edges}
//   nb.fetch(p)       — async fetch any /api/* endpoint
// Controls: scroll=zoom  drag=orbit/pan  dblclick=reset

const geo = new THREE.BoxGeometry();
const mat = new THREE.MeshNormalMaterial();
const mesh = new THREE.Mesh(geo, mat);
nb.scene.add(mesh);
nb.fitCamera();
nb.renderer.render(nb.scene, nb.camera);</textarea>
    </div>
  </div>
  <!-- Live screenshot preview panel -->
  <div id="scr-panel" style="display:none;position:fixed;bottom:16px;right:16px;z-index:600;
    background:#0d0d1a;border:1px solid #2a2a3e;border-radius:6px;
    box-shadow:0 4px 24px rgba(0,0,0,.6);
    resize:both;overflow:hidden;min-width:200px;min-height:140px;
    width:360px;height:240px;
    flex-direction:column;">
    <div id="scr-drag" style="padding:4px 8px;background:#1a1a2e;cursor:move;
      display:flex;align-items:center;justify-content:space-between;flex-shrink:0;
      font-size:.55rem;color:#6b7280;user-select:none;letter-spacing:.1em;text-transform:uppercase;">
      <span>canvas preview</span>
      <span style="display:flex;gap:6px;align-items:center">
        <label style="display:flex;align-items:center;gap:3px;cursor:pointer;font-size:.55rem">
          <input type="checkbox" id="scr-autopoll" style="cursor:pointer"> auto
        </label>
        <button id="scr-refresh" title="refresh" style="background:none;border:none;color:#fbbf24;cursor:pointer;font-size:.8rem;padding:0 2px">↻</button>
        <button id="scr-close" style="background:none;border:none;color:#6b7280;cursor:pointer;font-size:.75rem;padding:0 2px">✕</button>
      </span>
    </div>
    <div style="flex:1;overflow:hidden;position:relative;background:#0a0a14">
      <img id="scr-live-img" src="" alt="" style="width:100%;height:100%;object-fit:contain;display:block">
      <div id="scr-spinner" style="display:none;position:absolute;inset:0;align-items:center;justify-content:center;background:rgba(0,0,0,.5);color:#fbbf24;font-size:.7rem">capturing…</div>
    </div>
  </div>
</main>
<div class="restore-btns" id="restorebtns"></div>

<script>
const $=id=>document.getElementById(id);
const msgs=$('msgs'),feed=$('feed'),cin=$('cin');

// Pipe console output to server so it's visible in feed
;['log','warn','error'].forEach(lvl=>{const orig=console[lvl];console[lvl]=(...a)=>{orig.apply(console,a);fetch('/api/js-log',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({level:lvl,msg:a.map(x=>typeof x==='object'?JSON.stringify(x):String(x)).join(' ')})}).catch(()=>{})};});

function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')}
function now(){return new Date().toLocaleTimeString([],{hour:'2-digit',minute:'2-digit',second:'2-digit'})}

// Status
async function loadStatus(){
  try{
    const d=await(await fetch('/api/status')).json();
    $('sname').textContent=d.identity.name||'Steve';
    $('smeta').textContent=[d.identity.hexagram,d.identity.horoscope,d.identity.emotion].filter(Boolean).join(' · ');
    const _prev=window._statPrev||{};
    window._statPrev={smem:d.memories,sact:d.active,sks:d.keystones,sedge:d.graph_edges};
    function _setstat(id,val){
      const el=$(id);const prev=_prev[id];
      let html=String(val??'—');
      if(prev!=null&&val!=null&&val!==prev){const diff=val-prev;const pos=diff>0;html+=`<span style="font-size:.6rem;color:${pos?'#4ade80':'#f87171'};margin-left:2px">${pos?'+':''}${diff}</span>`;}
      el.innerHTML=html;
    }
    _setstat('smem',d.memories);_setstat('sact',d.active);_setstat('sks',d.keystones);_setstat('sedge',d.graph_edges);
    const m=$('smodel');
    m.textContent=(d.active_model??'—')+(d.model_locked?' 🔒':'');
    const _mc={'claude':'#93c5fd','gemma':'#4ade80','gpt-':'#f9a8d4','gemini':'#fbbf24','gpt4':'#f9a8d4'};
    const _mk=Object.keys(_mc).find(k=>d.active_model&&d.active_model.toLowerCase().includes(k));
    m.style.color=_mk?_mc[_mk]:'';
    if(d.session_cost!=null){window._sessionCost=d.session_cost;_renderCostStat();}
    // Refresh image provider stat so it reflects current budget/mode/availability
    fetch('/api/image-provider').then(r=>r.json()).then(ip=>{
      const si=$('simgmodel');
      if(si){si.textContent=ip.icon+' '+ip.label;si.style.color=ip.color;si.style.fontSize='.6rem';}
    }).catch(()=>{});
    if(d.hourly_budget!=null&&d.hourly_cost!=null){
      window._hourlyBudget=d.hourly_budget;window._hourlyCost=d.hourly_cost;
      const pressure=d.hourly_budget>0?d.hourly_cost/d.hourly_budget:0;
      const sb=$('sbudget');
      if(sb){sb.textContent='$'+d.hourly_budget.toFixed(2)+'/hr';sb.style.color=pressure>=0.9?'#f87171':pressure>=0.5?'#fbbf24':'#fb923c';}
      const hc=$('shcost');if(hc)hc.textContent='$'+d.hourly_cost.toFixed(3)+' rolling';
    }
    $('tswitch').checked=d.thinking;
    $('tdot').classList.toggle('on',d.thinking);
  }catch(e){}
}
loadStatus();
setInterval(loadStatus,15000);

// Init image provider stat
const _imgModeIcons={auto:'🎨',local:'🖥️',cloud:'🌐'};
const _imgModeColors={auto:'#ccc',local:'#4ade80',cloud:'#f9a8d4'};
function _applyImgMode(mode){
  const sm=$('simgmode');if(sm){sm.textContent=mode;sm.style.color=_imgModeColors[mode]||'#ccc';}
}
fetch('/api/image-provider').then(r=>r.json()).then(d=>{
  const si=$('simgmodel');
  if(si){si.textContent=d.icon+' '+d.label;si.style.color=d.color;si.style.fontSize='.6rem';}
}).catch(()=>{});
fetch('/api/image-mode').then(r=>r.json()).then(d=>_applyImgMode(d.mode)).catch(()=>{});
const _imgModes=['auto','local','cloud'];
$('imgmode-stat').addEventListener('click',()=>{
  const cur=($('simgmode')||{}).textContent||'auto';
  const next=_imgModes[(_imgModes.indexOf(cur)+1)%_imgModes.length];
  fetch('/api/image-mode',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mode:next})}).then(r=>r.json()).then(d=>_applyImgMode(d.mode)).catch(()=>{});
});

// Local mode toggle
function _applyLocalMode(enabled){
  const el=$('slocalmode');const lb=$('slocalmode-label');
  if(el){el.textContent=enabled?'🖥️':'🌐';el.style.color=enabled?'#4ade80':'var(--dim)';}
  if(lb){lb.textContent=enabled?'local':'cloud';lb.style.color=enabled?'#4ade80':'var(--dim)';}
}
fetch('/api/local-mode').then(r=>r.json()).then(d=>_applyLocalMode(d.enabled)).catch(()=>{});
$('localmode-stat').addEventListener('click',()=>{
  const cur=($('slocalmode-label')||{}).textContent==='local';
  fetch('/api/local-mode',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({enabled:!cur})}).then(r=>r.json()).then(d=>_applyLocalMode(d.enabled)).catch(()=>{});
});

// Cost stat: click to cycle display mode — session / /hr / /week / /month
const _costModes=['session','hour','week','month'];
let _costMode=0;
function _renderCostStat(){
  const sc=window._sessionCost??0;
  const hc=window._hourlyCost??0;
  const mode=_costModes[_costMode];
  const ce=$('scost');const cl=$('scost-label');
  if(!ce)return;
  if(mode==='session'){ce.textContent='$'+sc.toFixed(4);if(cl)cl.textContent='session';}
  else if(mode==='hour'){ce.textContent='$'+hc.toFixed(3)+'/hr';if(cl)cl.textContent='per hour';}
  else if(mode==='week'){ce.textContent='$'+(hc*168).toFixed(2)+'/wk';if(cl)cl.textContent='per week';}
  else{ce.textContent='$'+(hc*720).toFixed(2)+'/mo';if(cl)cl.textContent='per month';}
}
const _cstat=$('cost-stat');
if(_cstat){
  _cstat.addEventListener('click',()=>{_costMode=(_costMode+1)%_costModes.length;_renderCostStat();});
}

// Budget: click to cycle through presets
const _bdgPresets=[0.25,0.5,1,2,4,8];
const _bstat=$('budget-stat');
if(_bstat){
  _bstat.addEventListener('click',async()=>{
    const cur=window._hourlyBudget??2;
    const idx=_bdgPresets.findIndex(v=>Math.abs(v-cur)<0.01);
    const next=_bdgPresets[(idx+1)%_bdgPresets.length];
    await fetch('/api/set-budget',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({budget:next})});
    loadStatus();
  });
}

// Chat
const _LS_KEY='steve_chat_log';
const _LS_MAX=120;
function _lsSave(role,text,ts){
  try{
    const log=JSON.parse(localStorage.getItem(_LS_KEY)||'[]');
    log.push({role,text,ts});
    if(log.length>_LS_MAX)log.splice(0,log.length-_LS_MAX);
    localStorage.setItem(_LS_KEY,JSON.stringify(log));
  }catch(e){}
}
function addMsg(role,text,ts){
  const el=document.createElement('div');
  el.className=`msg ${role}`;
  el.innerHTML=`<div>${esc(text)}</div><div class="ts">${ts}</div>`;
  msgs.appendChild(el);
  msgs.scrollTop=msgs.scrollHeight;
  _lsSave(role,text,ts);
}

let _chatAbort=null;
window._currentAudio=null;

async function send(){
  const msg=cin.value.trim();
  if(!msg)return;
  // Interrupt in-flight request — human-like: new message cancels the old one
  if(_chatAbort){_chatAbort.abort();_chatAbort=null;}
  if(window._currentAudio){window._currentAudio.pause();window._currentAudio=null;}
  cin.value='';
  addMsg('user',msg,now());
  $('sendbtn').disabled=true;

  // Model switch command: "use gemma4 local", "use claude", "use auto", etc.
  const cmdM=msg.match(/^use\s+(gemma4?[\s:]*(local|e2b)?|e2b|local|claude[\s_]*opus?|claude|gemini|gpt[\s-]*4?o?|openai|auto)\s*$/i);
  if(cmdM){
    const raw=cmdM[0].replace(/^use\s*/i,'').toLowerCase().trim();
    const m=(raw.includes('gemma')||raw==='e2b'||raw==='local')?'gemma':
             raw.includes('opus')?'claude_opus':
             raw==='claude'?'claude':
             raw==='gemini'?'gemini':
             (raw.includes('gpt')||raw==='openai')?'openai':
             raw==='auto'?'':'';
    if(m!==undefined){
      const r=await fetch('/api/set-model',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({model:m})}).then(x=>x.json()).catch(()=>({}));
      const label=r.label||m||'auto';
      addMsg('steve',m?`Switched to ${label}.`:`Back to automatic model selection.`,now());
      $('sendbtn').disabled=false;cin.focus();return;
    }
  }

  const ctrl=new AbortController();
  _chatAbort=ctrl;
  const nonce=`${Date.now()}-${Math.random().toString(36).slice(2)}`;
  try{
    const d=await(await fetch('/api/chat',{
      method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({message:msg,nonce}),
      signal:ctrl.signal,
    })).json();
    if(_chatAbort!==ctrl)return;  // superseded by a newer message
    _chatAbort=null;
    addMsg('steve',d.reply||d.error||'...',now());
    if(d.audio_token){
      const url=`/api/audio/${d.audio_token}`;
      if($('vswitch').checked){
        const au=new Audio(url);
        window._currentAudio=au;
        au.onended=()=>{if(window._currentAudio===au)window._currentAudio=null;};
        au.play().catch(()=>{});
      }
      const a=document.createElement('a');
      a.href=url;a.download=`steve_${d.audio_token}.mp3`;
      a.style.cssText='font-size:.6rem;color:var(--muted);margin-left:.4rem;opacity:.6';
      a.textContent='⬇';a.title='download audio';
      const last=msgs.lastElementChild;if(last)last.appendChild(a);
    }
  }catch(e){
    if(e.name==='AbortError')return;
    if(_chatAbort===ctrl){addMsg('steve',`[${e.message}]`,now());_chatAbort=null;}
  }
  $('sendbtn').disabled=false;
  cin.focus();
}
$('sendbtn').addEventListener('click',send);
cin.addEventListener('keydown',e=>{if(e.key==='Enter')send()});

// Image upload
$('uploadinput').addEventListener('change',async e=>{
  const file=e.target.files[0];if(!file)return;
  const lbl=$('uploadlbl');
  lbl.style.color='var(--amber)';lbl.title='uploading...';
  const fd=new FormData();fd.append('image',file);
  try{
    const d=await(await fetch('/api/upload-image',{method:'POST',body:fd})).json();
    if(d.ok){
      const el=document.createElement('div');
      el.className='msg user';
      el.innerHTML=`<div>📎 ${esc(file.name)}</div><img src="${esc(d.url)}" style="max-width:180px;max-height:180px;border-radius:4px;margin-top:.3rem;display:block"><div class="ts">${now()}</div>`;
      msgs.appendChild(el);msgs.scrollTop=msgs.scrollHeight;
      lbl.style.color='#4ade80';setTimeout(()=>{lbl.style.color='';lbl.title='send an image to Steve';},2000);
    }else{
      lbl.style.color='#f87171';setTimeout(()=>{lbl.style.color='';},2000);
    }
  }catch(err){
    lbl.style.color='#f87171';setTimeout(()=>{lbl.style.color='';},2000);
    console.error('[upload]',err);
  }
  e.target.value='';
});

// Drag-and-drop images onto the chat panel
const _chatPanel=$('chatpanel');
_chatPanel.addEventListener('dragover',e=>{e.preventDefault();_chatPanel.style.outline='2px solid var(--amber)';});
_chatPanel.addEventListener('dragleave',()=>{_chatPanel.style.outline='';});
_chatPanel.addEventListener('drop',async e=>{
  e.preventDefault();_chatPanel.style.outline='';
  const file=[...e.dataTransfer.files].find(f=>f.type.startsWith('image/'));
  if(!file)return;
  const fd=new FormData();fd.append('image',file);
  try{
    const d=await(await fetch('/api/upload-image',{method:'POST',body:fd})).json();
    if(d.ok){
      const el=document.createElement('div');
      el.className='msg user';
      el.innerHTML=`<div>📎 ${esc(file.name)}</div><img src="${esc(d.url)}" style="max-width:180px;max-height:180px;border-radius:4px;margin-top:.3rem;display:block"><div class="ts">${now()}</div>`;
      msgs.appendChild(el);msgs.scrollTop=msgs.scrollHeight;
    }
  }catch(err){console.error('[upload]',err);}
});

// Think controls
$('tswitch').addEventListener('change',async()=>{
  await fetch($('tswitch').checked?'/api/think/start':'/api/think/stop',{method:'POST'});
});
$('pokebtn').addEventListener('click',async()=>{
  $('pokebtn').textContent='...';
  await fetch('/api/think/poke',{method:'POST'});
  setTimeout(()=>$('pokebtn').textContent='poke',800);
});
$('dreambtn').addEventListener('click',async()=>{
  $('dreambtn').textContent='...';
  try{await fetch('/api/dream',{method:'POST'})}catch(e){}
  addFeed('end',`dream triggered`);
  setTimeout(()=>$('dreambtn').textContent='dream',800);
});
$('ichingbtn').addEventListener('click',async()=>{
  $('ichingbtn').textContent='casting...';
  try{
    const r=await fetch('/api/iching',{method:'POST'});
    const d=await r.json();
    addFeed('text',`<span class="ts">${d.ts||''}</span>☯<br><pre style="white-space:pre-wrap;margin:.4rem 0 0;font-size:.8rem">${esc(d.result||d.error||'')}</pre>`);
  }catch(e){addFeed('err','iching failed: '+esc(String(e)));}
  setTimeout(()=>$('ichingbtn').textContent='☯ cast',1200);
});

// Feed
function addFeed(cls,html){
  const el=document.createElement('div');
  el.className=`fi ${cls}`;
  el.innerHTML=html;
  feed.appendChild(el);
  feed.scrollTop=feed.scrollHeight;
  while(feed.children.length>300)feed.removeChild(feed.firstChild);
}

// Code artifact registry (client-side, session-scoped)
window._codeReg = {};

function _renderCodeBlocks(text){
  const parts = [];
  let last = 0;
  const re = /```(\w*)\n?([\s\S]*?)```/g;
  let m;
  while((m = re.exec(text)) !== null){
    if(m.index > last)
      parts.push('<span style="white-space:pre-wrap">'+esc(text.slice(last,m.index))+'</span>');
    const lang = m[1], code = m[2].trim();
    const tok = 'cb_'+(Math.random()*1e9|0).toString(36);
    window._codeReg[tok] = {code, lang: lang||'js'};
    const langLabel = lang ? `<span style="font-size:.55rem;color:var(--muted);padding:2px 6px;display:block;border-bottom:1px solid var(--border)">${esc(lang)}</span>` : '';
    parts.push(`<div class="code-wrap">${langLabel}<pre>${esc(code)}</pre><div class="code-acts"><button onclick="_copyCode('${tok}')">📋 copy</button><button onclick="_sendToNb('${tok}')">→ notebook</button></div></div>`);
    last = m.index + m[0].length;
  }
  if(last < text.length)
    parts.push('<span style="white-space:pre-wrap">'+esc(text.slice(last))+'</span>');
  return parts.join('');
}

function _copyCode(tok){
  const c=window._codeReg[tok];if(!c)return;
  navigator.clipboard.writeText(c.code).catch(()=>{});
}

function _sendToNb(tok){
  const c=window._codeReg[tok];if(!c)return;
  if(window._cm){window._cm.setValue(c.code);runNotebook();}
}

function _reloadNbCode(code){
  if(window._cm){window._cm.setValue(code);runNotebook();}
}

function addExpandable(cls, ts, label, body){
  const el=document.createElement('div');
  const displayBody = cls==='text' ? _renderCodeBlocks(body) : '<span style="white-space:pre-wrap">'+esc(body)+'</span>';
  const plainLen = body.replace(/```[\s\S]*?```/g,'').length;
  const needsExpand = plainLen > 300;
  el.className=`fi ${cls}${needsExpand?' collapsible':''}`;
  const bodyHtml=`<div class="body">${displayBody}</div>`;
  const toggle=needsExpand?`<span class="toggle" onclick="this.closest('.fi').classList.toggle('open');this.textContent=this.closest('.fi').classList.contains('open')?'▲':'▼'">▼</span>`:'';
  el.innerHTML=`<span class="ts">${ts}</span>${label}${toggle}${bodyHtml}`;
  feed.appendChild(el);
  feed.scrollTop=feed.scrollHeight;
}

function showSysNotif(what, detail){
  const el=$('sysnotif');if(!el)return;
  el.textContent=(detail?`${what}: ${detail}`:what);
  el.style.opacity='1';
  clearTimeout(el._t);el._t=setTimeout(()=>{el.style.opacity='0';},4000);
}

function dispatchFeedEvent(event, d){
  if(event==='think_start') addFeed('start',`<span class="ts">${d.ts}</span>▶ cycle start`);
  else if(event==='think_end'){const cycleStr=d.cycle_cost!=null?` <span style="color:#fbbf24">($${d.cycle_cost.toFixed(4)})</span>`:'';addFeed('end',`<span class="ts">${d.ts}</span>■ cycle done${cycleStr}${d.error?' — '+esc(d.error):''}`)}
  else if(event==='think_context') addFeed('ctx',`<span class="ts">${d.ts}</span>${d.memories} memories · ${esc(d.hexagram)} · ${esc(d.emotion)}`);
  else if(event==='think_text') addExpandable('text',d.ts,'',d.text);
  else if(event==='random_thought') addFeed('text',`<span class="ts">${d.ts}</span><span style="color:#a78bfa">💭 random_thought:</span> ${esc(d.text)}`);
  else if(event==='think_tool') addFeed('tool',`<span class="ts">${d.ts}</span>⚡ ${esc(d.tool)}: ${esc(d.input)}`);
  else if(event==='think_result') addExpandable('result',d.ts,'↩ ',d.preview);
  else if(event==='think_error') addFeed('err',`<span class="ts">${d.ts}</span>✗ ${esc(d.error)}`);
  else if(event==='think_model'||event==='model_switch'){
    const _mc={'claude-':'#93c5fd','gemma':'#4ade80','gpt-':'#f9a8d4','gemini':'#fbbf24'};
    const ml=d.model_label||d.model;
    const _mk=Object.keys(_mc).find(k=>ml&&ml.startsWith(k));const col=_mk?_mc[_mk]:'#ccc';
    const sm=$('smodel');if(sm){sm.textContent=ml;sm.style.color=col;}
    if(event==='think_model') addFeed('ctx',`<span class="ts">${d.ts}</span>🧠 <span style="color:${col}">${esc(ml)}</span> · ${esc(d.emotion||'')}${d.flip?' ⚡':''}`);}
  else if(event==='emotion_change') addFeed('ctx',`<span class="ts">${d.ts}</span>mood: ${esc(d.from)} → <b>${esc(d.to)}</b>`);
  else if(event==='think_image'){const _isLocal=d.url.startsWith('/');const _saveBtn=_isLocal?`<a href="${esc(d.url)}" download="steve_${d.ts.replace(/[: ]/g,'_')}.png" style="margin-left:.4rem;font-size:.65rem;color:var(--dim);text-decoration:none" title="save image">💾</a>`:`<a href="${esc(d.url)}" target="_blank" style="margin-left:.4rem;font-size:.65rem;color:var(--dim);text-decoration:none" title="open to save">💾</a>`;addFeed('img',`<span class="ts">${d.ts}</span>drew: <i>${esc(d.prompt.slice(0,100))}</i>${_saveBtn}<br><img src="${esc(d.url)}" onload="const f=document.getElementById('feed');if(f)f.scrollTop=f.scrollHeight;" onclick="window.open(this.src,'_blank')" title="click to open full size">`);
    // If this is a notebook screenshot, update the live panel too
    if(d.url && d.url.includes('/api/image/scr_') && window._scrShowPanel){
      window._scrShowPanel();
      const liveImg=$('scr-live-img');
      if(liveImg && d.url) liveImg.src=d.url+'?t='+Date.now();
    }
  }
  else if(event==='js_log'){const cls=d.level==='error'?'err':d.level==='warn'?'tool':'ctx';addFeed(cls,`<span class="ts">${d.ts}</span>[js] ${esc(d.msg)}`);}
  else if(event==='think_tokens'){
    const cr=d.cache_read>0?` 💾${d.cache_read}`:'';
    const costStr=d.cost!=null?` <span style="color:#fbbf24">$${d.cost.toFixed(4)}</span>`:'';
    addFeed('ctx',`<span class="ts">${d.ts}</span><span style="color:var(--dim)">↑${d.in} ↓${d.out}${cr}</span>${costStr} <span style="color:var(--dim);font-size:.55rem">${esc(d.model)}</span>`);
    if(d.model&&d.model.startsWith('image/')){
      const _imgIcons  ={'image/comfy':'🖥️','image/dalle3':'🌐','image/gemini':'✨'};
      const _imgColors ={'image/comfy':'#4ade80','image/dalle3':'#f9a8d4','image/gemini':'#fbbf24'};
      const _imgLabels ={'image/comfy':'ComfyUI','image/dalle3':'DALL-E 3','image/gemini':'Gemini'};
      const si=$('simgmodel');
      if(si){si.textContent=(_imgIcons[d.model]||'🎨')+' '+(_imgLabels[d.model]||d.model.replace('image/',''));si.style.color=_imgColors[d.model]||'#ccc';si.style.fontSize='.65rem';}
    }
  }
  else if(event==='think_speak'){
    const el=document.createElement('div');
    el.className='fi text collapsible';
    const hasMore=d.full&&d.full!==d.lead&&d.full.length>d.lead.length+10;
    const pauseNote=d.paused?'<span style="color:var(--dim);font-size:.58rem"> (paused — lead only)</span>':'';
    const ttsNote=(d.spoken&&d.spoken!==d.full)?`<div style="margin-top:.35rem;font-size:.6rem;color:var(--dim);border-top:1px solid var(--border);padding-top:.25rem">🔊 spoken: ${esc(d.spoken.slice(0,200))}${d.spoken.length>200?'…':''}</div>`:'';
    if(hasMore){
      el.innerHTML=`<span class="ts">${d.ts}</span>🔊 ${esc(d.lead)}<span class="toggle" onclick="this.closest('.fi').classList.toggle('open');this.textContent=this.closest('.fi').classList.contains('open')?'▲':'▼ source'"> ▼ source</span>${pauseNote}<div class="body" style="margin-top:.35rem;color:var(--muted)">${esc(d.full)}${ttsNote}</div>`;
    }else{
      el.innerHTML=`<span class="ts">${d.ts}</span>🔊 ${esc(d.lead)}${pauseNote}${ttsNote?'<div class="body">'+ttsNote+'</div>':''}`;
    }
    feed.appendChild(el);feed.scrollTop=feed.scrollHeight;
    while(feed.children.length>300)feed.removeChild(feed.firstChild);
    if(d.audio_token&&$('vswitch').checked){
      if(window._currentAudio){window._currentAudio.pause();window._currentAudio=null;}
      const au=new Audio(`/api/audio/${d.audio_token}`);
      window._currentAudio=au;
      au.onended=()=>{if(window._currentAudio===au)window._currentAudio=null;};
      au.play().catch(()=>{});
    }
  }
  else if(event==='image_mode'){_applyImgMode(d.mode);addFeed('ctx',`<span class="ts">${d.ts}</span>🎨 image mode → ${esc(d.mode)}`);}
  else if(event==='local_mode'){
    _applyLocalMode(d.enabled);
    addFeed('ctx',`<span class="ts">${d.ts}</span>🖥️ local mode ${d.enabled?'<b style="color:#4ade80">on</b> — gemma + ComfyUI only':'<b style="color:#f87171">off</b> — cloud active'}`);
  }
  else if(event==='system_update'){ showSysNotif(d.what, d.detail); addFeed('ctx',`<span class="ts">${d.ts}</span>⚙ ${esc(d.what)}${d.detail?' → '+esc(d.detail):''}`);}
  else if(event==='show_presentation'){
    // Reveal notebook panel
    const _nbp=$('nbpanel');
    if(_nbp&&_nbp.classList.contains('collapsed')){
      _nbp.classList.remove('collapsed');
      const rb=$('restorebtns');
      if(rb)[...rb.querySelectorAll('.restore-btn')].forEach(b=>{if(b.textContent==='notebook')b.remove();});
    }
    // Remove any prior iframe
    const oldFr=document.getElementById('nb-pres-frame');
    if(oldFr)oldFr.remove();
    const oldCb=document.getElementById('nb-pres-close');
    if(oldCb)oldCb.remove();
    // Inject iframe over canvas
    const canvas=$('nbcanvas');
    if(canvas){
      const wrap=canvas.parentElement;
      if(wrap)wrap.style.position='relative';
      const fr=document.createElement('iframe');
      fr.id='nb-pres-frame';
      fr.src=(d.url||'/presentation')+'?t='+Date.now();
      const cTop=canvas.offsetTop||0;
      fr.style.cssText=`position:absolute;top:${cTop}px;left:0;width:100%;height:calc(100% - ${cTop}px);border:none;z-index:20;background:#0a0a14`;
      fr.allow='fullscreen';
      wrap.appendChild(fr);
      const cb=document.createElement('button');
      cb.id='nb-pres-close';
      cb.textContent='x close';
      cb.style.cssText=`position:absolute;top:${cTop+4}px;right:6px;z-index:21;font-size:.6rem;background:rgba(10,10,20,.9);border:1px solid #fbbf24;color:#fbbf24;font-family:monospace;padding:.2rem .5rem;border-radius:4px;cursor:pointer`;
      cb.onclick=()=>{fr.remove();cb.remove();};
      wrap.appendChild(cb);
    }
    const nSlides=d.count||'?';
    const slidesApi=d.token?`/presentation/${d.token}/slides`:'/presentation/slides';
    addFeed('ctx',`<span class="ts">${d.ts}</span>🎞 <b>${esc(d.title||'Presentation')}</b> — ${nSlides} slides &nbsp;<a href="${esc(d.url||'/presentation')}" target="_blank" rel="noopener" style="color:var(--amber);font-size:.6rem">fullscreen ↗</a> <a href="/presentations" target="_blank" style="color:var(--dim);font-size:.55rem">all ↗</a><br><details style="margin-top:.25rem" onToggle="if(this.open&&!this.dataset.loaded){this.dataset.loaded=1;fetch('${slidesApi}').then(r=>r.json()).then(j=>{const pre=this.querySelector('pre');if(pre)pre.textContent=JSON.stringify(j.slides,null,2);});}"><summary style="cursor:pointer;font-size:.6rem;color:#78716c">view slides JSON</summary><pre style="font-size:.55rem;max-height:180px;overflow:auto;white-space:pre-wrap;color:#6ee7b7;margin-top:.25rem">loading...</pre></details>`);
  }
  else if(event==='advocate'){
    if(d.event==='thought'){
      const el=document.createElement('div');
      el.className='fi advocate';
      el.innerHTML=`<span class="ts">${d.ts}</span>⚖ ${esc(d.text)}`;
      feed.appendChild(el);feed.scrollTop=feed.scrollHeight;
      while(feed.children.length>300)feed.removeChild(feed.firstChild);
    } else {
      addFeed('ctx',`<span class="ts">${d.ts}</span>⚖ advocate: ${esc(d.event)}`);
    }
  }
}

// Replay chat history — localStorage first (survives server restarts), fall back to server
(function(){
  try{
    const local=JSON.parse(localStorage.getItem(_LS_KEY)||'[]');
    if(local.length>0){
      local.forEach(({role,text,ts})=>{
        const el=document.createElement('div');
        el.className=`msg ${role}`;
        el.innerHTML=`<div>${esc(text)}</div><div class="ts">${ts}</div>`;
        msgs.appendChild(el);
      });
      msgs.scrollTop=msgs.scrollHeight;
      return;
    }
  }catch(e){}
  fetch('/api/chat/recent').then(r=>r.json()).then(log=>{
    log.forEach(({role,text,ts})=>addMsg(role,text,ts));
  }).catch(()=>{});
})();
fetch('/api/feed/recent').then(r=>r.json()).then(log=>{
  log.forEach(({event,data})=>dispatchFeedEvent(event,data));
}).catch(()=>{});

const es=new EventSource('/api/stream');
['think_start','think_end','think_context','think_text','think_tool','think_result','think_error','think_model','model_switch','emotion_change','think_image','js_log','think_tokens','think_speak','image_mode','advocate','system_update','screenshot_ready'].forEach(ev=>{
  es.addEventListener(ev,e=>dispatchFeedEvent(ev,JSON.parse(e.data)));
});
es.addEventListener('mode_change',e=>{
  const d=JSON.parse(e.data);
  $('tswitch').checked=d.thinking;
  $('tdot').classList.toggle('on',d.thinking);
  addFeed(d.thinking?'start':'end',`<span class="ts">${d.ts}</span>${d.thinking?'◉ thinking on':'◎ thinking off'}`);
});
es.addEventListener('open_tab',e=>{
  const d=JSON.parse(e.data);
  window.open(d.url,'_blank','noopener');
  addFeed('ctx',`<span class="ts">${d.ts}</span>🔗 opened: <a href="${esc(d.url)}" target="_blank" rel="noopener">${esc(d.reason||d.url)}</a>`);
});
es.addEventListener('run_notebook',e=>{
  const d=JSON.parse(e.data);
  const tok=d.token||'nb_latest';
  if(d.code){
    if(window._cm)window._cm.setValue(d.code);
    window._codeReg[tok]={code:d.code,lang:'javascript'};
    _nbAddTab(tok, d.description||tok);
  }
  // Reveal panel if collapsed
  const _nbp=$('nbpanel');
  if(_nbp&&_nbp.classList.contains('collapsed')){
    _nbp.classList.remove('collapsed');
    const rb=$('restorebtns');
    if(rb)[...rb.querySelectorAll('.restore-btn')].forEach(b=>{if(b.textContent==='notebook')b.remove();});
  }
  runNotebook();
  // Feed: compact link only — no code dump
  addFeed('ctx',`<span class="ts">${d.ts}</span>✏ <button onclick="_nbLoadTab('${esc(tok)}')" style="font-size:.6rem;background:none;border:none;color:var(--amber);cursor:pointer;padding:0;font-family:inherit;text-decoration:underline">${esc((d.description||tok).slice(0,60))}</button>`);
});
es.addEventListener('show_presentation',e=>dispatchFeedEvent('show_presentation',JSON.parse(e.data)));
es.addEventListener('nb_event',e=>{
  try{
    const d=JSON.parse(e.data);
    if(nb._events){
      nb._events.push(d);
      if(nb._events.length>200)nb._events.shift();
    }
    if(typeof nb.onEvent==='function') try{ nb.onEvent(d); }catch(ex){ console.warn('[nb_event]',ex.message); }
  }catch(ex){}
});
es.addEventListener('screenshot_request',async ()=>{
  try{
    const c=$('nbcanvas');
    const ov=document.getElementById('nboverlay');
    // Wait for any in-progress runNotebook() to finish — it's async, user code runs
    // after an await, so screenshot can arrive while canvas is still cleared.
    let _w=0; while(_nbRunning&&_w++<40) await new Promise(r=>setTimeout(r,75));
    // Force a render, then wait for exactly one animation frame (real GPU flush)
    if(nb.renderer&&nb.scene&&nb.camera) nb.renderer.render(nb.scene,nb.camera);
    await new Promise(r=>requestAnimationFrame(r));
    // Merge WebGL canvas + 2D overlay into one PNG
    const tmp=document.createElement('canvas');
    tmp.width=c.width||600; tmp.height=c.height||260;
    const ctx2=tmp.getContext('2d');
    ctx2.drawImage(c,0,0);
    if(ov)ctx2.drawImage(ov,0,0);
    const dataUrl=tmp.toDataURL('image/png');
    await fetch('/api/nb-screenshot-result',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({dataUrl})});
  }catch(e){
    // POST empty so the server doesn't hang waiting
    fetch('/api/nb-screenshot-result',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({dataUrl:'',error:e.message})}).catch(()=>{});
    console.error('[screenshot]',e.message);
  }
});

// ── Notebook ─────────────────────────────────────────────────────────────────
// nbcanvas = WebGL surface. A separate overlay canvas handles 2D ctx drawing.
// The two contexts can't share a single canvas element — WebGL locks it.
const nbcanvas=$('nbcanvas');
let nb={canvas:null,ctx:null,scene:null,camera:null,renderer:null,animId:null};

function nbInit(){
  const w=nbcanvas.offsetWidth||600, h=nbcanvas.offsetHeight||260;
  // Only reset canvas pixel dimensions on first init — assigning canvas.width resets
  // the WebGL drawing buffer, destroying preserveDrawingBuffer's effect.
  if(!nb.renderer){ nbcanvas.width=w; nbcanvas.height=h; }

  // 2D overlay canvas — layered on top of WebGL canvas, pointer-events:none
  let ov=document.getElementById('nboverlay');
  if(!ov){
    ov=document.createElement('canvas');
    ov.id='nboverlay';
    ov.style.cssText='position:absolute;top:0;left:0;pointer-events:none;width:100%;height:100%;z-index:1';
    ov.width=w; ov.height=h;
    nbcanvas.parentElement.style.position='relative';
    nbcanvas.insertAdjacentElement('afterend',ov);
  }else{ov.width=w;ov.height=h;}
  nb.canvas=ov;
  nb.ctx=ov.getContext('2d');

  if(window.THREE && !nb.renderer){
    nb.scene=new THREE.Scene();
    nb.camera=new THREE.PerspectiveCamera(60,w/h,0.1,1000);
    nb.camera.position.z=3;
    try{
      nb.renderer=new THREE.WebGLRenderer({canvas:nbcanvas,antialias:true,alpha:true,preserveDrawingBuffer:true});
      nb.renderer.setSize(w,h);
      nb.renderer.setClearColor(0x0a0a14,1);
    }catch(e){nb.renderer=null;console.warn('[nb] WebGL failed:',e.message);}
  }
}

let _nbRunning = false;  // true while runNotebook is executing (async gap before user code runs)

async function runNotebook(){
  _nbRunning = true;
  // Cancel known animation frame
  if(nb.animId){cancelAnimationFrame(nb.animId);nb.animId=null;}
  // Destroy and recreate overlay — kills any rogue animation loops holding the old ctx ref
  const oldOv=document.getElementById('nboverlay');
  if(oldOv)oldOv.remove();
  nb.canvas=null;nb.ctx=null;
  // Lazy reinit
  if(window.THREE) nbInit(); else nbInit();
  if(nb.renderer){nb.renderer.clear();}
  if(nb.scene){while(nb.scene.children.length){nb.scene.remove(nb.scene.children[0]);}}
  const code=(window._cm?window._cm.getValue():$('nbcode').value||'').trim();
  if(!code)return;
  const out=$('nbout');
  out.textContent='loading context...';

  // Pre-fetch Steve's memory context — injected as nb.data before code runs
  try{
    nb.data = await fetch('/api/nb-context').then(r=>r.json());
  }catch(e){
    nb.data = {};
  }
  // Async helpers: nb.recall(query), nb.fetch(path), nb.loadImage(urlOrObj)
  nb.recall = async (query,k=10)=>fetch('/api/recall',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({query,k})}).then(r=>r.json()).catch(()=>({}));
  nb.fetch  = (path)=>fetch(path).then(r=>r.json());
  nb.graph  = ()=>fetch('/api/nb-graph').then(r=>r.json()).catch(()=>({nodes:[],edges:[],stats:{}}));
  // Live event pipeline: nb.onEvent(evt) fires on every tool call; nb.nextEvent() pops the queue.
  // evt shape: {ts, tool, query?, url?, text?, prompt?, path?, description?}
  nb._events=[];
  nb.onEvent=null;
  nb.nextEvent=()=>nb._events.shift()||null;
  nb.images = (nb.data.images||[]);  // [{token,url}] — recent generated/dream images
  nb.latestImage = ()=>nb.images.length?nb.images[nb.images.length-1]:null;
  // Canvas dimensions — always use these, never hardcode pixels
  nb.W = nbcanvas.width || 600;
  nb.H = nbcanvas.height || 260;
  nb.cx = nb.W / 2;
  nb.cy = nb.H / 2;
  // Auto-fit camera to enclose all scene objects — call after adding geometry
  nb.fitCamera = function(padding=1.5){
    if(!nb.scene||!nb.camera||!window.THREE)return;
    const box=new THREE.Box3().setFromObject(nb.scene);
    if(box.isEmpty())return;
    const center=box.getCenter(new THREE.Vector3());
    const size=box.getSize(new THREE.Vector3());
    const maxDim=Math.max(size.x,size.y,size.z)||1;
    const fov=nb.camera.fov*(Math.PI/180);
    const dist=(maxDim/2)/Math.tan(fov/2)*padding;
    nb.camera.position.set(center.x,center.y,center.z+dist);
    nb.camera.near=dist/100; nb.camera.far=dist*100;
    nb.camera.updateProjectionMatrix();
    nb.camera.lookAt(center);
  };
  nb.loadImage = (src)=>new Promise((res,rej)=>{const i=new Image();i.crossOrigin='anonymous';i.onload=()=>res(i);i.onerror=rej;i.src=typeof src==='object'?src.url:src;});
  // THREE texture helpers — use images in 3D scenes
  nb.texture = (src)=>new Promise((res,rej)=>{
    if(!window.THREE){rej(new Error('THREE not loaded'));return;}
    new THREE.TextureLoader().load(typeof src==='object'?src.url:src,res,undefined,rej);
  });
  // nb.bg(src) — set a generated/dream image as the scene background
  nb.bg = async (src)=>{
    const tex=await nb.texture(src);
    if(nb.scene){nb.scene.background=tex;}
    if(nb.renderer&&nb.scene&&nb.camera)nb.renderer.render(nb.scene,nb.camera);
  };

  out.textContent='';
  const _nbLog=(level,...a)=>{
    const msg=a.map(x=>typeof x==='object'?JSON.stringify(x):String(x)).join(' ');
    out.textContent=msg;
    fetch('/api/js-log',{method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({level,msg:'[nb] '+msg})}).catch(()=>{});
  };
  const log=(...a)=>_nbLog('log',...a);
  // Proxy nb so user code writes (nb.animId=...) land on the real nb object,
  // and null renderer throws a clear error instead of a cryptic one.
  const _nullRenderer = !nb.renderer ? new Proxy({},{get:(_,p)=>{
    if(p==='then')return undefined;
    throw new Error('WebGL unavailable — nb.renderer is null. Use nb.ctx (2D) or reload.');
  }}) : null;
  const safeNb=new Proxy(nb,{
    get(t,p){ return (p==='renderer'&&_nullRenderer) ? _nullRenderer : t[p]; },
    set(t,p,v){ t[p]=v; return true; }
  });
  try{
    const fn=new Function('nb','THREE','console',code);
    fn(safeNb,window.THREE||null,{log,warn:(...a)=>_nbLog('warn',...a),error:(...a)=>_nbLog('error',...a)});
    if(!nb.animId && nb.renderer && nb.scene && nb.camera){
      nb.fitCamera();  // auto-fit whatever was created so nothing draws off screen
      nb.renderer.render(nb.scene,nb.camera);
    }
    if(!out.textContent)out.textContent='✓ ran';
  }catch(e){
    out.style.color='#f87171';out.textContent='✗ '+e.message;
    setTimeout(()=>out.style.color='',3000);
    const errCode=(window._cm?window._cm.getValue():$('nbcode').value||'').trim();
    fetch('/api/nb-error',{method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({error:e.message,code:errCode})}).catch(()=>{});
  } finally {
    _nbRunning = false;
  }
}

$('runbtn').addEventListener('click',runNotebook);
$('clearbtn').addEventListener('click',()=>{
  if(nb.animId){cancelAnimationFrame(nb.animId);nb.animId=null;}
  if(nb.renderer){nb.renderer.clear();}
  if(nb.ctx){nb.ctx.clearRect(0,0,nb.canvas?.width||600,nb.canvas?.height||260);}
  if(nb.scene){while(nb.scene.children.length)nb.scene.remove(nb.scene.children[0]);}
  $('nbout').textContent='';
});
// CodeMirror init — deferred one frame so flex layout is computed
requestAnimationFrame(function initCM(){
  if(typeof CodeMirror === 'undefined') return;
  window._cm = CodeMirror.fromTextArea($('nbcode'), {
    mode: 'javascript',
    theme: 'dracula',
    lineNumbers: true,
    tabSize: 2,
    indentWithTabs: false,
    lineWrapping: true,
    extraKeys: {'Ctrl-Enter': runNotebook, 'Cmd-Enter': runNotebook}
  });
  const wrap = $('nbcode-wrap');
  window._cm.setSize(wrap.clientWidth, wrap.clientHeight);
  new ResizeObserver(()=>window._cm.setSize(wrap.clientWidth, wrap.clientHeight)).observe(wrap);
});

// Load Three.js then init notebook
const s=document.createElement('script');
s.src='https://cdnjs.cloudflare.com/ajax/libs/three.js/r128/three.min.js';
s.onload=()=>{ nbInit(); };
s.onerror=()=>{ nbInit(); };
document.head.appendChild(s);
window.addEventListener('resize',()=>{
  if(!nbcanvas.parentElement)return;
  const w=nbcanvas.offsetWidth||600, h=nbcanvas.offsetHeight||260;
  nbcanvas.width=w; nbcanvas.height=h;
  const ov=document.getElementById('nboverlay');
  if(ov){ov.width=w;ov.height=h;}
  if(nb.camera){nb.camera.aspect=w/h;nb.camera.updateProjectionMatrix();}
  if(nb.renderer)nb.renderer.setSize(w,h);
});

// ── Notebook canvas / code resize handle ─────────────────────────────────────
(function(){
  const handle=$('nb-resize-handle');
  const canvas=$('nbcanvas');
  if(!handle||!canvas)return;
  let _dragging=false,_startY=0,_startH=0;
  handle.addEventListener('mousedown',e=>{
    _dragging=true;
    _startY=e.clientY;
    _startH=canvas.getBoundingClientRect().height;
    document.body.style.cursor='ns-resize';
    document.body.style.userSelect='none';
    e.preventDefault();
  });
  document.addEventListener('mousemove',e=>{
    if(!_dragging)return;
    const dy=e.clientY-_startY;
    const newH=Math.max(40,_startH+dy);
    canvas.style.height=newH+'px';
    // Update overlay to match
    const ov=document.getElementById('nboverlay');
    if(ov)ov.style.height=newH+'px';
    // Resize renderer if active
    const w=canvas.offsetWidth||600;
    if(nb.camera){nb.camera.aspect=w/newH;nb.camera.updateProjectionMatrix();}
    if(nb.renderer)nb.renderer.setSize(w,newH);
    if(window._cm)window._cm.refresh();
  });
  document.addEventListener('mouseup',()=>{
    if(!_dragging)return;
    _dragging=false;
    document.body.style.cursor='';
    document.body.style.userSelect='';
    if(window._cm){const wrap=$('nbcode-wrap');window._cm.setSize(wrap.clientWidth,wrap.clientHeight);}
  });
  handle.addEventListener('mouseenter',()=>{handle.style.background='var(--red)';});
  handle.addEventListener('mouseleave',()=>{if(!_dragging)handle.style.background='var(--border)';});
})();

// ── Notebook pan / zoom / orbit ───────────────────────────────────────────────
// Always-on: scroll=zoom, drag=pan(2D)/orbit(3D), no user code needed
(function(){
  let _drag=false,_last={x:0,y:0};
  // 2D pan state
  let _panX=0,_panY=0,_zoom2d=1;

  function _re2d(){
    if(!nb.ctx||!nb.canvas)return;
    nb.ctx.setTransform(_zoom2d,0,0,_zoom2d,_panX,_panY);
  }

  nbcanvas.addEventListener('wheel',e=>{
    e.preventDefault();
    const delta=e.deltaY<0?1.1:0.9;
    if(nb.renderer&&nb.camera&&nb.scene){
      // 3D zoom — move camera along its Z axis
      nb.camera.position.z=Math.max(0.01,nb.camera.position.z*delta);
      nb.camera.updateProjectionMatrix();
      if(!nb.animId)nb.renderer.render(nb.scene,nb.camera);
    } else if(nb.ctx){
      // 2D zoom around cursor
      const rect=nbcanvas.getBoundingClientRect();
      const mx=e.clientX-rect.left,my=e.clientY-rect.top;
      _panX=(1-delta)*mx+_panX*delta;
      _panY=(1-delta)*my+_panY*delta;
      _zoom2d*=delta;
      _re2d();
    }
  },{passive:false});

  nbcanvas.addEventListener('mousedown',e=>{
    if(e.button!==0)return;
    _drag=true;_last={x:e.clientX,y:e.clientY};
    nbcanvas.style.cursor='grabbing';
  });
  document.addEventListener('mouseup',()=>{_drag=false;nbcanvas.style.cursor='';});
  document.addEventListener('mousemove',e=>{
    if(!_drag)return;
    const dx=e.clientX-_last.x,dy=e.clientY-_last.y;
    _last={x:e.clientX,y:e.clientY};
    if(nb.renderer&&nb.camera&&nb.scene){
      // 3D orbit — rotate camera around origin
      const sph=new (window.THREE?THREE.Spherical:Object)();
      if(!window.THREE)return;
      sph.setFromVector3(nb.camera.position);
      sph.theta-=dx*0.01; sph.phi-=dy*0.01;
      sph.phi=Math.max(0.05,Math.min(Math.PI-0.05,sph.phi));
      nb.camera.position.setFromSpherical(sph);
      nb.camera.lookAt(0,0,0);
      if(!nb.animId)nb.renderer.render(nb.scene,nb.camera);
    } else if(nb.ctx){
      _panX+=dx;_panY+=dy;_re2d();
    }
  });

  // Double-click: reset view
  nbcanvas.addEventListener('dblclick',()=>{
    if(nb.renderer&&nb.camera&&nb.scene){
      nb.fitCamera&&nb.fitCamera();
      if(!nb.animId)nb.renderer.render(nb.scene,nb.camera);
    } else if(nb.ctx){
      _panX=0;_panY=0;_zoom2d=1;_re2d();
    }
  });
})();

// ── Notebook tabs ─────────────────────────────────────────────────────────────
const _NB_LS_KEY = 'steve_nb_tabs';
window._nbTabs = [];  // [{tok, label}]
window._nbActiveTab = null;

function _nbSaveTabs(){
  try{ localStorage.setItem(_NB_LS_KEY, JSON.stringify(window._nbTabs.slice(-20))); }catch(e){}
}

function _nbAddTab(tok, label){
  const existing = window._nbTabs.findIndex(t=>t.tok===tok);
  if(existing>=0){ window._nbTabs[existing].label=label; }
  else { window._nbTabs.push({tok, label: label.slice(0,32)}); }
  window._nbActiveTab = tok;
  _nbSaveTabs();
  _nbRenderTabs();
}

function _nbLoadTab(tok){
  const c=window._codeReg[tok];
  if(!c)return;
  // Ensure panel is visible
  const panel=$('nbpanel');
  if(panel&&panel.classList.contains('collapsed')){
    panel.classList.remove('collapsed');
    const rb=$('restorebtns');
    if(rb)[...rb.querySelectorAll('.restore-btn')].forEach(b=>{if(b.textContent==='notebook')b.remove();});
  }
  window._nbActiveTab=tok;
  if(window._cm)window._cm.setValue(c.code);
  _nbRenderTabs();
  runNotebook();
}

function _nbRenderTabs(){
  const bar=$('nbtabs');if(!bar)return;
  bar.innerHTML='';
  window._nbTabs.forEach(({tok,label})=>{
    const tab=document.createElement('div');
    tab.className='nb-tab'+(tok===window._nbActiveTab?' active':'');
    tab.innerHTML=`<span onclick="_nbLoadTab('${esc(tok)}')">${esc(label)}</span><span class="nb-tab-x" onclick="_nbCloseTab('${esc(tok)}')">×</span>`;
    bar.appendChild(tab);
  });
}

function _nbCloseTab(tok){
  window._nbTabs=window._nbTabs.filter(t=>t.tok!==tok);
  if(window._nbActiveTab===tok){
    const last=window._nbTabs[window._nbTabs.length-1];
    if(last){ _nbLoadTab(last.tok); }
    else{ window._nbActiveTab=null; if(window._cm)window._cm.setValue(''); }
  }
  _nbSaveTabs();
  _nbRenderTabs();
}

// Restore tabs from server on load — recent code artifacts become tabs
(async function _nbRestoreTabs(){
  try{
    const arts = await fetch('/api/code/recent').then(r=>r.json());
    // Reverse so oldest is leftmost
    const sorted = [...arts].reverse();
    sorted.forEach(a=>{
      if(a.token&&a.code){ window._codeReg[a.token]={code:a.code,lang:a.lang||'javascript'}; }
      if(a.token&&!window._nbTabs.find(t=>t.tok===a.token)){
        window._nbTabs.push({tok:a.token, label:(a.description||a.token).slice(0,32)});
      }
    });
    if(window._nbTabs.length>0&&!window._nbActiveTab){
      window._nbActiveTab=window._nbTabs[window._nbTabs.length-1].tok;
      const active=window._codeReg[window._nbActiveTab];
      if(active&&window._cm)window._cm.setValue(active.code);
    }
    _nbRenderTabs();
  }catch(e){}
})();

// ── Notebook fullscreen: expand button + escape overlay ───────────────────────
function collapseNbFullscreen(){
  const p=$('nbpanel');
  if(!p)return;
  p.style.cssText='';
  const c=$('nbcanvas');if(c)c.style.cssText='';
  const ov=document.getElementById('nboverlay');if(ov)ov.style.cssText='';
  if(document.fullscreenElement)document.exitFullscreen().catch(()=>{});
  $('nb-fs-close').style.display='none';
}

$('expandbtn').addEventListener('click',()=>{
  const p=$('nbpanel');
  const isExpanded=p.style.position==='fixed';
  if(isExpanded){
    collapseNbFullscreen();
  }else{
    p.style.cssText='position:fixed;inset:0;z-index:500;max-width:none!important;flex:none!important';
    $('nb-fs-close').style.display='block';
  }
});

// Auto-show close button if notebook panel gets a fixed/absolute style injected
const _nbPanelObs=new MutationObserver(()=>{
  const p=$('nbpanel');if(!p)return;
  const pos=p.style.position;
  const fsb=$('nb-fs-close');if(fsb)fsb.style.display=(pos==='fixed'||pos==='absolute')?'block':'none';
});
_nbPanelObs.observe($('nbpanel'),{attributes:true,attributeFilter:['style']});

document.addEventListener('fullscreenchange',()=>{
  const fsb=$('nb-fs-close');if(fsb)fsb.style.display=document.fullscreenElement?'block':'none';
});

// ── Panel close / restore ─────────────────────────────────────────────────────
(function(){
  const rbtns=$('restorebtns');

  function collapsePanel(panel,label){
    // Reset any inline styles code may have set (e.g. full-screen takeover)
    panel.style.cssText='';
    const canvas=$('nbcanvas');
    if(canvas)canvas.style.cssText='';
    const ov=document.getElementById('nboverlay');
    if(ov)ov.style.cssText='';
    if(document.fullscreenElement)document.exitFullscreen().catch(()=>{});
    $('nb-fs-close').style.display='none';
    panel.classList.add('collapsed');
    const btn=document.createElement('button');
    btn.className='restore-btn';
    btn.textContent=label;
    btn.onclick=()=>{panel.classList.remove('collapsed');btn.remove();};
    rbtns.appendChild(btn);
  }

  $('closethink').addEventListener('click',()=>collapsePanel($('thinkpanel'),'thinking'));
  $('closenb').addEventListener('click',()=>collapsePanel($('nbpanel'),'notebook'));

  // Escape key: first collapses fullscreen, then hides panel
  document.addEventListener('keydown',e=>{
    if(e.key!=='Escape')return;
    const p=$('nbpanel');
    if(p.style.position==='fixed'||document.fullscreenElement){ collapseNbFullscreen(); return; }
    if(!p.classList.contains('collapsed')) collapsePanel(p,'notebook');
  });
})();

// ── Live screenshot panel ─────────────────────────────────────────────────────
(function(){
  const panel=$('scr-panel');
  const img=$('scr-live-img');
  const spinner=$('scr-spinner');
  const refreshBtn=$('scr-refresh');
  const closeBtn=$('scr-close');
  const autopoll=$('scr-autopoll');
  const drag=$('scr-drag');
  if(!panel||!img)return;

  let _pollTimer=null;
  let _waiting=false;

  function showPanel(){ panel.classList.add('visible'); }
  function hidePanel(){ panel.classList.remove('visible'); }

  function setFrame(url){
    img.src=url+'?t='+Date.now();
    spinner.style.display='none';
    _waiting=false;
    showPanel();
  }

  function requestCapture(){
    if(_waiting)return;
    _waiting=true;
    spinner.style.display='flex';
    fetch('/api/nb-screenshot-request',{method:'POST'}).catch(()=>{ _waiting=false; spinner.style.display='none'; });
  }

  // Receive new frame
  es.addEventListener('screenshot_ready',e=>{
    const d=JSON.parse(e.data);
    if(d.url) setFrame(d.url);
  });

  refreshBtn.addEventListener('click',requestCapture);
  closeBtn.addEventListener('click',hidePanel);

  autopoll.addEventListener('change',()=>{
    clearInterval(_pollTimer);
    if(autopoll.checked) _pollTimer=setInterval(requestCapture,3000);
  });

  // Drag
  let _dx=0,_dy=0,_dragging=false;
  drag.addEventListener('mousedown',e=>{
    if(e.target.tagName==='INPUT'||e.target.tagName==='BUTTON')return;
    _dragging=true;
    _dx=e.clientX-panel.getBoundingClientRect().left;
    _dy=e.clientY-panel.getBoundingClientRect().top;
    e.preventDefault();
  });
  document.addEventListener('mousemove',e=>{
    if(!_dragging)return;
    panel.style.left=(e.clientX-_dx)+'px';
    panel.style.top=(e.clientY-_dy)+'px';
    panel.style.right='auto';
    panel.style.bottom='auto';
  });
  document.addEventListener('mouseup',()=>{ _dragging=false; });

  // When screenshot_request is already in flight from the think feed, show spinner
  es.addEventListener('screenshot_request_ack',()=>{ _waiting=true; spinner.style.display='flex'; showPanel(); });

  // Expose so feed button can trigger
  window._scrRequestCapture=requestCapture;
  window._scrShowPanel=showPanel;
})();
</script>
<div id="sysnotif" style="position:fixed;top:2.5rem;right:.6rem;background:#1c1917;border:1px solid #92400e;color:#fbbf24;font-size:.6rem;padding:.3rem .7rem;border-radius:4px;z-index:200;opacity:0;transition:opacity .3s;pointer-events:none"></div>
<button id="nb-fs-close" onclick="collapseNbFullscreen()">✕ close</button>
</body>
</html>"""

def _load_uploads():
    """Load persisted upload images into _served_images on startup."""
    if not os.path.isdir(UPLOADS_DIR):
        return
    loaded = 0
    for fname in sorted(os.listdir(UPLOADS_DIR)):
        if not fname.lower().endswith((".png", ".jpg", ".jpeg", ".gif", ".webp")):
            continue
        token = os.path.splitext(fname)[0]
        try:
            with open(os.path.join(UPLOADS_DIR, fname), "rb") as fp:
                data = fp.read()
            with _images_lock:
                _served_images[token] = data
            loaded += 1
        except Exception:
            pass
    if loaded:
        print(f"[steve]  uploads: loaded {loaded} image(s) from {UPLOADS_DIR}")

def _load_audio():
    """Load persisted TTS audio into _audio_store on startup."""
    if not os.path.isdir(AUDIO_DIR):
        return
    loaded = 0
    for fname in sorted(os.listdir(AUDIO_DIR)):
        if not fname.lower().endswith(".mp3"):
            continue
        token = os.path.splitext(fname)[0]
        try:
            with open(os.path.join(AUDIO_DIR, fname), "rb") as fp:
                data = fp.read()
            with _audio_lock:
                _audio_store[token] = data
            loaded += 1
        except Exception:
            pass
    if loaded:
        print(f"[steve]  audio: loaded {loaded} file(s) from {AUDIO_DIR}")


if __name__ == "__main__":
    _load_uploads()
    _load_audio()
    _preload_impress_assets()
    # Start advocate loop
    _advocate_stop.clear()
    _advocate_thread = threading.Thread(target=_advocate_loop, daemon=True)
    _advocate_thread.start()
    port = int(os.environ.get("PORT", 8100))
    print(f"[steve]  http://localhost:{port}")
    print(f"[steve]  ferricula  = {FERRICULA_URL}")
    print(f"[steve]  shivvr     = {SHIVVR_URL}")
    print(f"[steve]  crawl      = {CRAWL_URL}")
    print(f"[steve]  interval   = {THINK_INTERVAL}s")
    if not ANTHROPIC_KEY:
        print("[steve]  WARNING: no ANTHROPIC_API_KEY")
    if not SERPAPI_KEY:
        print(f"[steve]  search: grub+ddg fallback ({CRAWL_URL})")
    print(f"[steve]  voice: {'ElevenLabs ✓' if ELEVEN_KEY else '✗ no ELEVENLABS_API_KEY — voice disabled'}")
    print(f"[steve]  openai: {'✓' if OPENAI_KEY else '✗ no key'} | gemini: {'✓' if GEMINI_KEY else '✗ no key'} | ollama: {OLLAMA_URL}")
    app.run(host="0.0.0.0", port=port, debug=False, threaded=True)
