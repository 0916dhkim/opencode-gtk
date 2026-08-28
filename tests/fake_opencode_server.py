#!/usr/bin/env python3
import argparse
import json
import queue
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse


class ServerState:
    def __init__(self, events_file: Path):
        self.events_file = events_file
        self.lock = threading.Lock()
        self.clients = []
        self.subscriptions = 0
        self.permission_pending = True
        self.sent_stale_resolution = False
        self.messages = [
            {
                "info": {
                    "id": "msg_ready",
                    "sessionID": "ses_test",
                    "role": "assistant",
                    "time": {"created": 1},
                },
                "parts": [
                    {
                        "id": "part_ready",
                        "messageID": "msg_ready",
                        "sessionID": "ses_test",
                        "type": "text",
                        "text": (
                            "# Ready\n\n"
                            "- **Markdown** content\n"
                            "- Inline `code <tag>`\n\n"
                            "> Safe [link](https://example.com?a=1&b=2)\n\n"
                            "```rust\nfn main() {}\n```"
                        ),
                    }
                ],
            }
        ]

    def record(self, event: str):
        with self.lock:
            with self.events_file.open("a", encoding="utf-8") as stream:
                stream.write(f"{event}\n")

    def subscribe(self):
        events = queue.Queue()
        with self.lock:
            self.clients.append(events)
            self.subscriptions += 1
            count = self.subscriptions
        self.record(f"sse:{count}")
        return events

    def unsubscribe(self, events):
        with self.lock:
            if events in self.clients:
                self.clients.remove(events)

    def emit(self, payload):
        with self.lock:
            clients = list(self.clients)
        for events in clients:
            events.put(payload)

    def close_streams(self):
        with self.lock:
            clients = list(self.clients)
        for events in clients:
            events.put(None)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    @property
    def state(self):
        return self.server.state

    def log_message(self, *_args):
        pass

    def send_json(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def send_empty(self, status=204):
        self.send_response(status)
        self.send_header("Content-Length", "0")
        self.send_header("Connection", "close")
        self.end_headers()

    def send_event(self, payload, directory="/repo"):
        envelope = {"directory": directory, "payload": payload}
        self.wfile.write(f"data: {json.dumps(envelope)}\n\n".encode())
        self.wfile.flush()

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/global/event":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "keep-alive")
            self.end_headers()
            events = self.state.subscribe()
            try:
                self.send_event({"type": "server.connected", "properties": {}})
                while True:
                    try:
                        event = events.get(timeout=1)
                    except queue.Empty:
                        self.wfile.write(b": heartbeat\n\n")
                        self.wfile.flush()
                        continue
                    if event is None:
                        self.close_connection = True
                        return
                    self.send_event(event)
            except (BrokenPipeError, ConnectionResetError):
                return
            finally:
                self.state.unsubscribe(events)

        if path == "/global/health":
            self.send_json({"version": "1.18.15-test"})
        elif path == "/project":
            self.send_json([{"worktree": "/repo", "name": "Test project"}])
        elif path == "/session/status":
            self.send_json({})
        elif path in {"/session/ses_test/message", "/session/ses_other/message"}:
            session_id = path.split("/")[2]
            self.state.record(f"messages:{session_id}")
            if session_id == "ses_test":
                with self.state.lock:
                    messages = list(self.state.messages)
            else:
                messages = [
                    {
                        "info": {
                            "id": "msg_other",
                            "sessionID": "ses_other",
                            "role": "assistant",
                            "time": {"created": 2},
                        },
                        "parts": [
                            {
                                "id": "part_other",
                                "messageID": "msg_other",
                                "sessionID": "ses_other",
                                "type": "text",
                                "text": "Second session content",
                            }
                        ],
                    }
                ]
            self.send_json(messages)
        elif path == "/session":
            self.send_json(
                [
                    {
                        "id": "ses_test",
                        "directory": "/repo",
                        "title": "Integration session",
                        "time": {"created": 1, "updated": 3},
                    },
                    {
                        "id": "ses_other",
                        "directory": "/repo",
                        "title": "Second session",
                        "time": {"created": 2, "updated": 2},
                    },
                ]
            )
        elif path == "/permission":
            with self.state.lock:
                pending = self.state.permission_pending
            permissions = [
                {
                    "id": "per_boot",
                    "sessionID": "ses_test",
                    "permission": "bash",
                    "patterns": ["echo integration"],
                    "metadata": {},
                    "always": ["echo *"],
                },
                {
                    "id": "per_stale",
                    "sessionID": "ses_test",
                    "permission": "read",
                    "patterns": ["stale request"],
                    "metadata": {},
                    "always": ["stale request"],
                },
            ]
            self.send_json(permissions if pending else [])
        elif path == "/question":
            with self.state.lock:
                emit_resolution = not self.state.sent_stale_resolution
                self.state.sent_stale_resolution = True
            if emit_resolution:
                self.state.emit(
                    {
                        "type": "permission.replied",
                        "properties": {
                            "sessionID": "ses_test",
                            "requestID": "per_stale",
                            "reply": "reject",
                        },
                    }
                )
            self.send_json([])
        elif path == "/config/providers":
            self.send_json(
                {
                    "providers": [
                        {
                            "id": "mock",
                            "name": "Mock",
                            "models": {
                                "mock-model": {
                                    "id": "mock-model",
                                    "name": "Mock model",
                                    "capabilities": {"input": {"text": True, "image": True}},
                                }
                            },
                        }
                    ],
                    "default": {"mock": "mock-model"},
                }
            )
        elif path == "/config":
            self.send_json({"model": "mock/mock-model"})
        else:
            self.send_json({"message": f"Unknown path: {path}"}, status=404)

    def do_POST(self):
        path = urlparse(self.path).path
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")

        if path == "/session":
            title = body.get("title") or "New session"
            self.state.record(f"session:create:{title}")
            self.send_json(
                {
                    "id": "ses_new",
                    "directory": "/repo",
                    "title": title,
                    "time": {"created": 4, "updated": 4},
                }
            )
            return

        if path == "/permission/per_boot/reply":
            with self.state.lock:
                self.state.permission_pending = False
            self.state.record(f"permission:{body.get('reply')}")
            self.send_empty()
            return

        if path == "/session/ses_test/prompt_async":
            text = "".join(
                part.get("text", "") for part in body.get("parts", []) if part.get("type") == "text"
            )
            self.state.record(f"prompt:{text}")
            files = sum(part.get("type") == "file" for part in body.get("parts", []))
            self.state.record(f"files:{files}")
            assistant = {
                "info": {
                    "id": "msg_reply",
                    "sessionID": "ses_test",
                    "role": "assistant",
                    "time": {"created": 3},
                },
                "parts": [
                    {
                        "id": "part_reply",
                        "messageID": "msg_reply",
                        "sessionID": "ses_test",
                        "type": "text",
                        "text": "Pong",
                    }
                ],
            }
            with self.state.lock:
                self.state.messages.append(assistant)
            self.state.emit(
                {
                    "type": "message.updated",
                    "properties": {"sessionID": "ses_test", "info": assistant["info"]},
                }
            )
            self.state.emit(
                {"type": "message.part.updated", "properties": {"part": assistant["parts"][0]}}
            )
            self.state.emit(
                {
                    "type": "question.asked",
                    "properties": {
                        "id": "que_live",
                        "sessionID": "ses_test",
                        "questions": [
                            {
                                "header": "Integration",
                                "question": "Continue?",
                                "options": [
                                    {"label": "Yes", "description": "Continue the test"}
                                ],
                                "multiple": False,
                                "custom": True,
                            }
                        ],
                    },
                }
            )
            self.send_empty()
            self.state.close_streams()
            return

        if path == "/question/que_live/reject":
            self.state.record("question:reject")
            self.send_empty()
            return

        if path == "/question/que_live/reply":
            self.state.record("question:reply")
            self.send_empty()
            return

        if path == "/session/ses_test/abort":
            self.state.record("session:abort")
            self.send_empty()
            return

        self.send_json({"message": f"Unknown path: {path}"}, status=404)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--address-file", required=True, type=Path)
    parser.add_argument("--events-file", required=True, type=Path)
    args = parser.parse_args()
    args.events_file.touch()
    state = ServerState(args.events_file)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.state = state
    host, port = server.server_address
    args.address_file.write_text(f"http://{host}:{port}\n", encoding="utf-8")
    server.serve_forever()


if __name__ == "__main__":
    main()
