#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import time

def run_verification():
    env = os.environ.copy()
    env["FERRICULA_BASE_URL"] = "http://host.docker.internal:18875"
    env["OLLAMA_BASE_URL"] = "http://host.docker.internal:11434"
    env["FERRICULA_OPERATOR_TOKEN_FILE"] = "/workspace/memory/ferricula_v2/secrets/ferricula_operator_token"

    bridge_path = "/workspace/memory/ferricula_v2/scripts/steve_mcp_bridge.py"
    proc = subprocess.Popen(
        [sys.executable, "-u", bridge_path],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
        bufsize=1
    )

    results = {}

    def send_rpc(req):
        proc.stdin.write(json.dumps(req) + "\n")
        proc.stdin.flush()
        line = proc.stdout.readline()
        if not line:
            err = proc.stderr.read()
            raise RuntimeError(f"Bridge closed stdout. Stderr: {err}")
        return json.loads(line)

    try:
        # 1. initialize
        print("==> 1. Testing initialize...")
        t0 = time.time()
        init_res = send_rpc({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "verifier", "version": "1.0"}
            }
        })
        results["initialize"] = {
            "status": "PASS" if init_res.get("result", {}).get("serverInfo", {}).get("name") == "steve" else "FAIL",
            "elapsed_s": round(time.time() - t0, 3),
            "payload": init_res
        }
        print(f"    Result: {results['initialize']['status']} ({results['initialize']['elapsed_s']}s)")

        # 2. tools/list
        print("==> 2. Testing tools/list...")
        t0 = time.time()
        list_res = send_rpc({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })
        tools = [t.get("name") for t in list_res.get("result", {}).get("tools", [])]
        expected_tools = ["steve_status", "steve_identity", "steve_recall", "steve_chat"]
        tools_match = all(t in tools for t in expected_tools) and len(tools) == 4
        results["tools_list"] = {
            "status": "PASS" if tools_match else "FAIL",
            "elapsed_s": round(time.time() - t0, 3),
            "tools_found": tools
        }
        print(f"    Result: {results['tools_list']['status']}, tools: {tools}")

        # 3. tools/call steve_status
        print("==> 3. Testing steve_status...")
        t0 = time.time()
        status_res = send_rpc({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "steve_status", "arguments": {}}
        })
        status_text = status_res.get("result", {}).get("content", [{}])[0].get("text", "{}")
        status_data = json.loads(status_text)
        results["steve_status"] = {
            "status": "PASS" if "memory_rows" in status_data or "ok" in status_data else "FAIL",
            "elapsed_s": round(time.time() - t0, 3),
            "data": status_data
        }
        print(f"    Result: {results['steve_status']['status']}, memory_rows: {status_data.get('memory_rows')}")

        # 4. tools/call steve_recall
        print("==> 4. Testing steve_recall (query: Macintosh typography)...")
        t0 = time.time()
        recall_res = send_rpc({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "steve_recall", "arguments": {"query": "Macintosh typography", "limit": 2}}
        })
        recall_text = recall_res.get("result", {}).get("content", [{}])[0].get("text", "{}")
        recall_data = json.loads(recall_text)
        hits = recall_data.get("hits", [])
        hit_ids = [h.get("id") for h in hits]
        results["steve_recall"] = {
            "status": "PASS" if len(hits) > 0 else "FAIL",
            "elapsed_s": round(time.time() - t0, 3),
            "hit_ids": hit_ids,
            "top_hits": [
                {
                    "id": h.get("id"),
                    "score": round(h.get("score", 0), 3),
                    "keystone": h.get("keystone"),
                    "text_preview": h.get("tags", {}).get("text", "")[:120]
                }
                for h in hits
            ]
        }
        print(f"    Result: {results['steve_recall']['status']}, hits found: {hit_ids}")

        # 5. tools/call steve_chat
        print("==> 5. Testing steve_chat (prompt: Steve, tell me about designing the typography of the Macintosh.)...")
        t0 = time.time()
        chat_res = send_rpc({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "steve_chat",
                "arguments": {"message": "Steve, tell me about designing the typography of the Macintosh."}
            }
        })
        chat_text = chat_res.get("result", {}).get("content", [{}])[0].get("text", "{}")
        chat_data = json.loads(chat_text)
        results["steve_chat"] = {
            "status": "PASS" if "reply" in chat_data and chat_data.get("recall_status") == "grounded" else "FAIL",
            "elapsed_s": round(time.time() - t0, 3),
            "model": chat_data.get("model"),
            "recall_status": chat_data.get("recall_status"),
            "memories_grounded_count": chat_data.get("memories_grounded_count"),
            "memories_referenced": [
                {"id": m.get("id"), "score": m.get("score"), "keystone": m.get("keystone")}
                for m in chat_data.get("memories_referenced", [])
            ],
            "reply": chat_data.get("reply")
        }
        print(f"    Result: {results['steve_chat']['status']} in {results['steve_chat']['elapsed_s']}s")
        print(f"    Grounded count: {chat_data.get('memories_grounded_count')}")
        print(f"    Reply preview: {chat_data.get('reply', '')[:200]}...")

    finally:
        proc.stdin.close()
        proc.terminate()
        proc.wait(timeout=5)

    out_file = "/workspace/memory/audit/2026-09-26/STEVE_STDIO_TEST_RESULT.json"
    with open(out_file, "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nAll verification tests completed. Summary saved to {out_file}")

if __name__ == "__main__":
    run_verification()
