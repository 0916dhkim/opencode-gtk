#!/usr/bin/env python3
"""Scripted OpenAI-compatible Chat Completions provider for the OpenCode 2.0.8 harness.

OpenCode 2.0.8 routes `@opencode/ai/providers/openai-compatible` models to
`POST {baseURL}/chat/completions` with `stream: true` (OpenAI Chat SSE framing).

Behavior is selected by a marker in the latest user message, e.g.
`[[scenario:tools]]`. Requests without tools (title generation, compaction and
other auxiliary calls) always get a short plain reply. A request whose last
message is a tool result gets the scenario's final text.

Debug endpoints: GET /_log returns every received request; POST /_reset clears it.
"""
import argparse
import json
import re
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MARKER = re.compile(r"\[\[scenario:([a-z0-9_-]+)\]\]")
MODELS = ["mock-model", "mock-model-alt"]

LOCK = threading.Lock()
LOG = []
ATTEMPTS = {}


def message_text(message):
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(part.get("text", "") for part in content if isinstance(part, dict))
    return ""


def latest_user_text(messages):
    for message in reversed(messages):
        if message.get("role") == "user":
            return message_text(message)
    return ""


def scenario_of(text):
    found = MARKER.findall(text)
    return found[-1] if found else "default"


def tool_names(body):
    names = []
    for tool in body.get("tools") or []:
        function = tool.get("function") or {}
        if function.get("name"):
            names.append(function["name"])
    return names


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "ocgtk-mock-provider/1"

    def log_message(self, fmt, *args):
        print("mock: " + fmt % args, flush=True)

    # ---- plumbing -------------------------------------------------------
    def send_json(self, status, payload, headers=None):
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def start_sse(self):
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        self.close_connection = True

    def sse(self, payload):
        self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
        self.wfile.flush()

    def chunk(self, delta, finish=None, usage=None):
        payload = {
            "id": self.completion_id,
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": self.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        }
        if usage is not None:
            payload["usage"] = usage
        self.sse(payload)

    def finish(self, reason, completion_tokens=8):
        usage = {"prompt_tokens": 42, "completion_tokens": completion_tokens, "total_tokens": 42 + completion_tokens}
        self.chunk({}, finish=reason, usage=usage)
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def stream_text(self, pieces, delay=0.0, reasoning=None):
        self.start_sse()
        self.chunk({"role": "assistant", "content": ""})
        for piece in reasoning or []:
            self.chunk({"reasoning_content": piece})
            if delay:
                time.sleep(delay)
        for piece in pieces:
            self.chunk({"content": piece})
            if delay:
                time.sleep(delay)
        self.finish("stop", completion_tokens=len(pieces) + len(reasoning or []))

    def stream_tool_calls(self, calls, text=None):
        self.start_sse()
        self.chunk({"role": "assistant", "content": ""})
        if text:
            self.chunk({"content": text})
        for index, (call_id, name, arguments) in enumerate(calls):
            self.chunk({
                "tool_calls": [{
                    "index": index,
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": ""},
                }]
            })
            encoded = json.dumps(arguments)
            middle = len(encoded) // 2
            for part in (encoded[:middle], encoded[middle:]):
                self.chunk({"tool_calls": [{"index": index, "function": {"arguments": part}}]})
        self.finish("tool_calls", completion_tokens=12)

    # ---- routes ---------------------------------------------------------
    def do_GET(self):
        if self.path == "/_log":
            with LOCK:
                return self.send_json(200, list(LOG))
        if self.path in ("/v1/models", "/models"):
            return self.send_json(200, {"object": "list", "data": [{"id": m, "object": "model", "owned_by": "mock"} for m in MODELS]})
        if self.path in ("/health", "/"):
            return self.send_json(200, {"ok": True})
        return self.send_json(404, {"error": {"message": "not found"}})

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b""
        if self.path == "/_reset":
            with LOCK:
                LOG.clear()
                ATTEMPTS.clear()
            return self.send_json(200, {"ok": True})
        if not self.path.rstrip("/").endswith("/chat/completions"):
            with LOCK:
                LOG.append({"seq": len(LOG) + 1, "path": self.path, "unhandled": True})
            return self.send_json(404, {"error": {"message": f"mock does not implement {self.path}"}})
        try:
            body = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return self.send_json(400, {"error": {"message": "invalid json"}})
        try:
            self.handle_chat(body)
        except (BrokenPipeError, ConnectionResetError):
            print("mock: client disconnected mid-stream", flush=True)

    def handle_chat(self, body):
        messages = body.get("messages") or []
        tools = tool_names(body)
        user_text = latest_user_text(messages)
        scenario = scenario_of(user_text)
        last_role = messages[-1].get("role") if messages else None
        self.model = body.get("model", "mock-model")
        self.completion_id = f"chatcmpl-mock-{int(time.time() * 1000)}"

        if not tools:
            decision = "auxiliary"
        elif last_role == "tool":
            decision = f"{scenario}:final"
        else:
            decision = scenario
        with LOCK:
            LOG.append({
                "seq": len(LOG) + 1,
                "time": time.time(),
                "path": self.path,
                "scenario": scenario,
                "decision": decision,
                "tools": tools,
                "authorization_scheme": (self.headers.get("authorization") or "").split(" ")[0],
                "body": body,
            })

        if decision == "auxiliary":
            return self.stream_text(["Mock ", "session ", "title"])
        if decision.endswith(":final"):
            finals = {
                "tools": ["Both tool calls ", "finished."],
                "permission": ["The shell command ", "completed."],
                "subagent": ["Launched a background ", "subagent."],
                "subagent-permission": ["The child subagent ", "finished."],
            }
            return self.stream_text(finals.get(scenario, ["Tool work ", "finished."]))

        if scenario == "reasoning":
            return self.stream_text(
                ["After thinking, ", "the answer is 42."],
                reasoning=["Let me think ", "about this ", "carefully."],
            )
        if scenario == "tools":
            read_name = "read" if "read" in tools else tools[0]
            glob_name = "glob" if "glob" in tools else read_name
            return self.stream_tool_calls([
                ("call_mock_read", read_name, {"path": "README.md"}),
                ("call_mock_glob", glob_name, {"pattern": "**/*.txt"}),
            ], text="Running two tools at once.")
        if scenario == "permission":
            return self.stream_tool_calls([
                ("call_mock_shell", "shell", {"command": "echo permission-probe", "description": "Echo a probe string"}),
            ])
        if scenario == "subagent":
            return self.stream_tool_calls([
                ("call_mock_subagent", "subagent", {
                    "agent": "general",
                    "description": "Mock child task",
                    "prompt": "[[scenario:child]] Reply with a short greeting.",
                    "background": True,
                }),
            ])
        if scenario == "subagent-permission":
            # Foreground child whose own turn hits the shell `ask` rule.
            return self.stream_tool_calls([
                ("call_mock_subagent_fg", "subagent", {
                    "agent": "general",
                    "description": "Mock child shell task",
                    "prompt": "[[scenario:permission]] Run the probe command.",
                }),
            ])
        if scenario == "child":
            return self.stream_text(["Hello from ", "the child ", "subagent."])
        if scenario == "error":
            return self.send_json(
                500,
                {"error": {"message": "Mock provider exploded", "type": "server_error", "code": "mock_failure"}},
                headers={"x-should-retry": "false"},
            )
        if scenario == "retry":
            with LOCK:
                ATTEMPTS[user_text] = ATTEMPTS.get(user_text, 0) + 1
                attempt = ATTEMPTS[user_text]
            if attempt == 1:
                return self.send_json(
                    503,
                    {"error": {"message": "Mock provider temporarily unavailable", "type": "server_error"}},
                    headers={"x-should-retry": "true", "retry-after-ms": "100"},
                )
            return self.stream_text(["Recovered ", "after a retry."])
        if scenario == "slow":
            return self.stream_text([f"slow-{i} " for i in range(20)], delay=0.5)
        if scenario == "long":
            return self.stream_text([f"word{i} " for i in range(300)])
        return self.stream_text(["Hello ", "from the ", "mock provider."])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=4100)
    args = parser.parse_args()
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.daemon_threads = True
    print(f"mock provider listening on {args.host}:{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
