#!/usr/bin/env python3
"""A stand-in for the `claude` CLI in print mode (stream-json in and out). NOT the real CLI: it prints canned lines in the shapes the seat's own claude rung reads,
and for the `tool` modes it speaks MCP to the server named in --mcp-config the way Claude Code does. The mode is the first word of the user's text; the second word
names the log file ($FAKE_CLAUDE_DIR/<word>.json: what the child was started with)."""
import sys, json, os, subprocess, threading, time

argv = sys.argv[1:]
def opt(name):
    return argv[argv.index(name) + 1]

srv = json.loads(opt("--mcp-config"))["mcpServers"]["seat"]
msg = json.loads(sys.stdin.readline())
content = msg["message"]["content"]
user = content if isinstance(content, str) else "".join(b.get("text", "") for b in content)
mode, tag = (user.split() + ["", ""])[:2]
SEEN = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_DISABLE_AUTO_MEMORY"]
d = os.environ.get("FAKE_CLAUDE_DIR")
if d and tag:
    with open(os.path.join(d, tag + ".json"), "w") as f:
        json.dump({"argv": argv, "user": user, "pid": os.getpid(), "env": {k: os.environ.get(k) == "sk-test-SECRET-123" and "KEY" or (k in os.environ) for k in SEEN}}, f)

def emit(o):
    print(json.dumps(o), flush=True)

def assistant(blocks, mid, usage=(5, 7)):
    emit({"type": "assistant", "message": {"id": mid, "role": "assistant", "content": blocks, "usage": {"input_tokens": usage[0], "output_tokens": usage[1]}}})

def result(text, usage=(5, 7), err=False):
    emit({"type": "result", "subtype": "error_during_execution" if err else "success", "is_error": err, "result": text, "usage": {"input_tokens": usage[0], "output_tokens": usage[1]}})

emit({"type": "system", "subtype": "init", "tools": []})

class Shim:
    def __init__(self):
        self.p = subprocess.Popen([srv["command"]] + srv["args"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.n = 0
    def send(self, method, params=None, note=False):
        o = {"jsonrpc": "2.0", "method": method}
        if params is not None: o["params"] = params
        if not note:
            self.n += 1; o["id"] = self.n
        self.p.stdin.write(json.dumps(o) + "\n"); self.p.stdin.flush()
        return None if note else self.n
    def recv(self):
        return json.loads(self.p.stdout.readline())

def start():
    s = Shim()
    s.send("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "x"}}); s.recv()
    s.send("notifications/initialized", note=True)
    s.send("tools/list", {}); tools = [t["name"] for t in s.recv()["result"]["tools"]]
    return s, tools

if mode == "plain":
    assistant([{"type": "text", "text": "hello from claude"}], "m1")
    result("hello from claude")
elif mode == "tool":
    s, tools = start()
    assistant([{"type": "text", "text": "checking"}, {"type": "tool_use", "id": "tu1", "name": "mcp__seat__echo", "input": {"text": "ping"}}], "m1")
    s.send("tools/call", {"name": "echo", "arguments": {"text": "ping"}})
    r = s.recv()["result"]["content"][0]["text"]
    emit({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu1", "content": r}]}})
    assistant([{"type": "text", "text": "got: " + r + " [tools: " + ",".join(tools) + "]"}], "m2", usage=(11, 13))
    result("got: " + r, usage=(99, 99))
elif mode == "two":
    s, tools = start()
    assistant([{"type": "tool_use", "id": "tu1", "name": "mcp__seat__echo", "input": {"text": "a"}}, {"type": "tool_use", "id": "tu2", "name": "mcp__seat__other", "input": {"n": 2}}], "m1")
    i1 = s.send("tools/call", {"name": "echo", "arguments": {"text": "a"}}); i2 = s.send("tools/call", {"name": "other", "arguments": {"n": 2}})
    got = {}
    for _ in range(2):
        r = s.recv(); got[r["id"]] = r["result"]["content"][0]["text"]
    assistant([{"type": "text", "text": "got: " + got[i1] + "|" + got[i2]}], "m2")
    result("done")
elif mode == "hang":
    assistant([{"type": "text", "text": "thinking"}], "m1")
    time.sleep(3600)
elif mode == "holdtool":
    s, tools = start()
    assistant([{"type": "tool_use", "id": "tu1", "name": "mcp__seat__echo", "input": {"text": "x"}}], "m1")
    s.send("tools/call", {"name": "echo", "arguments": {"text": "x"}})
    s.recv()   # waits for ever when the seat never answers
elif mode == "fail":
    sys.stderr.write("boom: no auth\n"); sys.exit(3)
elif mode == "keyleak":
    sys.stderr.write("rejected key " + os.environ.get("ANTHROPIC_API_KEY", "") + "\n"); sys.exit(1)
elif mode == "cache":
    emit({"type": "assistant", "message": {"id": "m1", "role": "assistant", "content": [{"type": "text", "text": "cached"}], "usage": {"input_tokens": 3, "cache_creation_input_tokens": 4, "cache_read_input_tokens": 50, "output_tokens": 2}}})
    result("cached")
elif mode == "iserror":
    result("the model said no", err=True)
elif mode == "long":
    assistant([{"type": "text", "text": "x" * 30000}], "m1"); result("")
