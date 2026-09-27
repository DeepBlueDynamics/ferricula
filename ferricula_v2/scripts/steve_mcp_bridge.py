#!/usr/bin/env python3
"""
Steve Jobs Memory MCP Bridge (stdio JSON-RPC 2.0)
Connects bare-metal Nemesis8 agents, Claude, Cursor, and AGY to Steve's live memory runtime.
Grounded in authentic recovered memory texts from the immutable recovery dataset.

LIMITATIONS & CONTRACT:
- Transport: stdio JSON-RPC 2.0 only (not streamable HTTP).
- Retrieval: Lexical token-coverage lookup against immutable recovery data (not semantic/dense).
- Persistence: Ephemeral conversation (chat turns are not persisted to disk).
"""

import sys
import os
import json
import urllib.request
import urllib.error

# Resolve operator token
TOKEN = os.environ.get("FERRICULA_OPERATOR_TOKEN", "")
TOKEN_FILE = os.environ.get("FERRICULA_OPERATOR_TOKEN_FILE", "")
if not TOKEN and TOKEN_FILE and os.path.exists(TOKEN_FILE):
    with open(TOKEN_FILE, "r") as f:
        TOKEN = f.read().strip()

if not TOKEN:
    candidates = [
        os.path.join(os.path.dirname(__file__), "..", "secrets", "ferricula_operator_token"),
        "/workspace/memory/ferricula_v2/secrets/ferricula_operator_token",
        r"C:\Users\kordl\Code\DeepBlueDynamics\memory\ferricula_v2\secrets\ferricula_operator_token",
    ]
    for c in candidates:
        if os.path.exists(c):
            with open(c, "r") as f:
                TOKEN = f.read().strip()
                break

BASE_URL = os.environ.get("FERRICULA_BASE_URL", "http://127.0.0.1:18875")
OLLAMA_URL = os.environ.get("OLLAMA_BASE_URL", "http://127.0.0.1:11434")

def http_request(url, method="GET", data=None, headers=None):
    hdrs = headers or {}
    if TOKEN:
        hdrs["Authorization"] = f"Bearer {TOKEN}"
    body = json.dumps(data).encode("utf-8") if data is not None else None
    if body:
        hdrs["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=body, headers=hdrs, method=method)
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode("utf-8"))

def handle_status():
    try:
        return http_request(f"{BASE_URL}/status")
    except Exception as e:
        try:
            return http_request(f"{BASE_URL}/health")
        except Exception as e2:
            return {"error": f"Failed to reach Steve runtime: {e2}"}

def handle_identity():
    try:
        return http_request(f"{BASE_URL}/identity")
    except Exception as e:
        return {"error": f"Failed to get identity: {e}"}

def handle_recall(query, limit=10):
    try:
        return http_request(f"{BASE_URL}/memory/recall", method="POST", data={"query": query, "limit": limit})
    except Exception as e:
        return {"error": f"Recall error: {e}"}

def handle_chat(message):
    # Step 1: Recall relevant memories and extract hydrated text
    memories_referenced = []
    memory_context_blocks = []
    recall_status = "no_hits"
    recall_error_msg = None

    try:
        recall_resp = handle_recall(message, limit=5)
        if isinstance(recall_resp, dict):
            if "error" in recall_resp:
                recall_status = "recall_failed"
                recall_error_msg = recall_resp["error"]
            elif "hits" in recall_resp:
                hits = recall_resp["hits"]
                if hits:
                    for hit in hits:
                        if hit.get("state") != "active":
                            continue
                        text = hit.get("tags", {}).get("text", "").strip()
                        if not text:
                            continue
                        mem_id = hit.get("id")
                        score = hit.get("score", 0.0)
                        keystone = hit.get("keystone", False)
                        memories_referenced.append({
                            "id": mem_id,
                            "score": round(score, 3),
                            "keystone": keystone,
                            "text": text
                        })
                        keystone_tag = " [Keystone]" if keystone else ""
                        memory_context_blocks.append(f"- Memory #{mem_id}{keystone_tag} (lexical score {score:.2f}): \"{text}\"")
                    if memories_referenced:
                        recall_status = "grounded"
                    else:
                        recall_status = "no_eligible_active_text"
                else:
                    recall_status = "no_hits"
    except Exception as e:
        recall_status = "recall_failed"
        recall_error_msg = str(e)

    # Step 2: Build grounded system prompt with verified memory text
    system_prompt = (
        "You are Steve Jobs. Speak directly, passionately, and concisely about product excellence, "
        "simplicity, focus, and doing great work.\n"
    )
    if memory_context_blocks:
        system_prompt += (
            "\nYou are remembering the following authentic experiences and facts from your life:\n"
            + "\n".join(memory_context_blocks)
            + "\n\nDraw upon these memories naturally in your response when relevant."
        )
    else:
        system_prompt += "\nNo specific episodic memory was retrieved for this prompt; speak from your general values."

    ollama_payload = {
        "model": "gemma4:e2b",
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": message}
        ],
        "temperature": 0.7
    }

    urls = [
        f"{OLLAMA_URL}/v1/chat/completions",
        "http://host.docker.internal:11434/v1/chat/completions",
        "http://127.0.0.1:11434/v1/chat/completions"
    ]
    for url in urls:
        try:
            req = urllib.request.Request(
                url,
                data=json.dumps(ollama_payload).encode("utf-8"),
                headers={"Content-Type": "application/json"}
            )
            with urllib.request.urlopen(req, timeout=45) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                return {
                    "reply": data["choices"][0]["message"]["content"],
                    "model": "gemma4:e2b",
                    "retrieval_mode": "lexical_token_coverage",
                    "recall_status": recall_status,
                    "recall_error": recall_error_msg,
                    "memories_grounded_count": len(memories_referenced),
                    "memories_referenced": memories_referenced,
                    "persisted": False
                }
        except Exception:
            continue

    return {"error": "Failed to connect to local Ollama inference service."}

def handle_rpc(request):
    req_id = request.get("id")
    method = request.get("method")
    params = request.get("params", {})

    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {
                    "name": "steve",
                    "title": "Steve Jobs Memory & Persona",
                    "version": "2.0.0"
                }
            }
        }
    elif method == "notifications/initialized":
        return None
    elif method == "tools/list":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "tools": [
                    {
                        "name": "steve_status",
                        "description": "Read Steve Jobs runtime health, memory counts, and operational mode.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": False
                        }
                    },
                    {
                        "name": "steve_identity",
                        "description": "Read Steve Jobs identity inspection statistics (active memories, keystones, graph nodes).",
                        "inputSchema": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": False
                        }
                    },
                    {
                        "name": "steve_recall",
                        "description": "Lexical candidate search over authentic recovery data (token coverage, not semantic/dense embeddings).",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": {"type": "string", "description": "Search query or concept to recall"},
                                "limit": {"type": "integer", "description": "Max results to return", "default": 10}
                            },
                            "required": ["query"],
                            "additionalProperties": False
                        }
                    },
                    {
                        "name": "steve_chat",
                        "description": "Converse directly with Steve Jobs, grounded in eligible active memory text retrieved via lexical search. Note: Conversation is ephemeral; turns are not persisted to disk.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "message": {"type": "string", "description": "Your question or statement to Steve Jobs"}
                            },
                            "required": ["message"],
                            "additionalProperties": False
                        }
                    }
                ]
            }
        }
    elif method == "tools/call":
        name = params.get("name")
        args = params.get("arguments", {})
        if name == "steve_status":
            res = handle_status()
            return {"jsonrpc": "2.0", "id": req_id, "result": {"content": [{"type": "text", "text": json.dumps(res, indent=2)}]}}
        elif name == "steve_identity":
            res = handle_identity()
            return {"jsonrpc": "2.0", "id": req_id, "result": {"content": [{"type": "text", "text": json.dumps(res, indent=2)}]}}
        elif name == "steve_recall":
            res = handle_recall(args.get("query", ""), args.get("limit", 10))
            return {"jsonrpc": "2.0", "id": req_id, "result": {"content": [{"type": "text", "text": json.dumps(res, indent=2)}]}}
        elif name == "steve_chat":
            res = handle_chat(args.get("message", ""))
            return {"jsonrpc": "2.0", "id": req_id, "result": {"content": [{"type": "text", "text": json.dumps(res, indent=2)}]}}
        else:
            return {"jsonrpc": "2.0", "id": req_id, "error": {"code": -32601, "message": f"Method {name} not found"}}
    elif method == "ping":
        return {"jsonrpc": "2.0", "id": req_id, "result": {}}
    else:
        return {"jsonrpc": "2.0", "id": req_id, "error": {"code": -32601, "message": f"Method {method} not found"}}

def run_stdio():
    if hasattr(sys.stdin, "reconfigure"):
        try:
            sys.stdin.reconfigure(line_buffering=True)
            sys.stdout.reconfigure(line_buffering=True)
        except Exception:
            pass

    while True:
        line = sys.stdin.readline()
        if not line:
            break
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            resp = handle_rpc(req)
            if resp is not None:
                sys.stdout.write(json.dumps(resp) + "\n")
                sys.stdout.flush()
        except Exception as e:
            err = {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": str(e)}}
            sys.stdout.write(json.dumps(err) + "\n")
            sys.stdout.flush()

if __name__ == "__main__":
    run_stdio()
