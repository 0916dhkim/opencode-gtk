#!/usr/bin/env python3
"""Drive an isolated OpenCode 2.0.8 server and record protocol fixtures.

Run through `tests/v2/harness.sh capture`. The Basic password comes from the
OCGTK_V2H_PASSWORD environment variable and must never appear in the output.
"""
import argparse
import base64
import json
import os
import secrets
import select
import shutil
import socket
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

# 1x1 transparent PNG.
PIXEL_PNG = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
)
BASE62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
TERMINAL = {"session.execution.succeeded", "session.execution.failed", "session.execution.interrupted"}
RAW_FRAMES = 12

_last_ts = 0
_counter = 0


def ascending_id(prefix):
    """Mirror of @opencode/schema identifier.ts ascending() so client IDs sort like server IDs."""
    global _last_ts, _counter
    ts = int(time.time() * 1000)
    if ts != _last_ts:
        _last_ts, _counter = ts, 0
    _counter += 1
    value = ts * 0x1000 + _counter
    head = "".join(f"{(value >> (40 - 8 * i)) & 0xFF:02x}" for i in range(6))
    tail = "".join(BASE62[b % 62] for b in secrets.token_bytes(14))
    return prefix + head + tail


class Client:
    def __init__(self, base, password, out):
        self.base = base.rstrip("/")
        parsed = urllib.parse.urlparse(self.base)
        self.host, self.port = parsed.hostname, parsed.port or 80
        self.token = base64.b64encode(f"opencode:{password}".encode()).decode()
        self.out = out
        self.index = []

    def call(self, method, path, body=None, auth=True, headers=None):
        data = None if body is None else json.dumps(body).encode()
        hdrs = {"accept": "application/json"}
        if data is not None:
            hdrs["content-type"] = "application/json"
        if auth:
            hdrs["authorization"] = "Basic " + self.token
        hdrs.update(headers or {})
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=hdrs)
        try:
            resp = urllib.request.urlopen(req, timeout=60)
        except urllib.error.HTTPError as err:
            resp = err
        raw = resp.read()
        text = raw.decode("utf-8", errors="replace")
        try:
            parsed = json.loads(text) if text else None
            is_json = bool(text)
        except json.JSONDecodeError:
            parsed, is_json = None, False
        return {
            "status": resp.status,
            "headers": {k.lower(): v for k, v in resp.headers.items()},
            "json": parsed,
            "is_json": is_json,
            "text": text,
        }

    def record(self, name, method, path, body=None, template=None, note=None, auth=True, headers=None, expect=None):
        result = self.call(method, path, body=body, auth=auth, headers=headers)
        split = urllib.parse.urlsplit(path)
        wrapper = {
            "name": name,
            "request": {
                "method": method,
                "path": split.path,
                "pathTemplate": template or split.path,
                "query": split.query or None,
                "body": body,
            },
            "response": {
                "status": result["status"],
                "headers": {
                    key: result["headers"][key]
                    for key in ("content-type", "content-length", "www-authenticate")
                    if key in result["headers"]
                },
                "body": result["json"] if result["is_json"] else None,
            },
        }
        if not result["is_json"] and result["text"]:
            wrapper["response"]["bodyText"] = result["text"]
        if note:
            wrapper["note"] = note
        write_json(os.path.join(self.out, name + ".json"), wrapper)
        self.index.append({"name": name, "method": method, "path": wrapper["request"]["pathTemplate"],
                           "query": wrapper["request"]["query"], "status": result["status"]})
        if expect is not None and result["status"] != expect:
            raise SystemExit(f"{name}: expected {expect}, got {result['status']}: {result['text'][:500]}")
        return result


def write_json(path, value):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, ensure_ascii=False)
        handle.write("\n")


class SSERecorder(threading.Thread):
    """Raw-socket SSE reader so the exact wire framing can be recorded."""

    def __init__(self, client, name):
        super().__init__(daemon=True)
        self.client = client
        self.name = name
        self.stop_flag = threading.Event()
        self.ready = threading.Event()
        self.lock = threading.Lock()
        self.status_line = None
        self.headers = {}
        self.frames = []  # raw frame text
        self.events = []  # parsed JSON payloads
        self.last_event_at = time.time()
        self.error = None

    def run(self):
        try:
            self._run()
        except Exception as exc:  # noqa: BLE001 - recorded for the caller
            self.error = repr(exc)
            self.ready.set()

    def _run(self):
        sock = socket.create_connection((self.client.host, self.client.port), timeout=10)
        request = (
            f"GET /api/event HTTP/1.1\r\nHost: {self.client.host}:{self.client.port}\r\n"
            f"Authorization: Basic {self.client.token}\r\nAccept: text/event-stream\r\n\r\n"
        )
        sock.sendall(request.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = sock.recv(65536)
            if not chunk:
                raise RuntimeError("SSE connection closed before headers")
            buf += chunk
        head, buf = buf.split(b"\r\n\r\n", 1)
        lines = head.decode().split("\r\n")
        self.status_line = lines[0]
        self.headers = {k.strip().lower(): v.strip() for k, v in (line.split(":", 1) for line in lines[1:])}
        chunked = self.headers.get("transfer-encoding", "").lower() == "chunked"
        self.ready.set()
        pending = buf
        body = b""
        while not self.stop_flag.is_set():
            if chunked:
                while True:
                    if b"\r\n" not in pending:
                        break
                    size_line, rest = pending.split(b"\r\n", 1)
                    size = int(size_line.split(b";")[0], 16)
                    if len(rest) < size + 2:
                        break
                    body += rest[:size]
                    pending = rest[size + 2:]
                    if size == 0:
                        self.stop_flag.set()
            else:
                body += pending
                pending = b""
            body = self._frames(body)
            readable, _, _ = select.select([sock], [], [], 0.2)
            if readable:
                chunk = sock.recv(65536)
                if not chunk:
                    break
                pending += chunk
        sock.close()

    def _frames(self, body):
        text = body.replace(b"\r\n", b"\n")
        while b"\n\n" in text:
            frame, text = text.split(b"\n\n", 1)
            decoded = frame.decode("utf-8")
            data = "\n".join(line[5:].lstrip(" ") for line in decoded.split("\n") if line.startswith("data:"))
            with self.lock:
                self.frames.append(decoded)
                if data:
                    event = json.loads(data)
                    self.events.append(event)
                    self.last_event_at = time.time()
        return text

    def snapshot(self):
        with self.lock:
            return list(self.events)

    def wait_for(self, predicate, timeout=30.0, what="event"):
        deadline = time.time() + timeout
        while time.time() < deadline:
            for event in self.snapshot():
                if predicate(event):
                    return event
            time.sleep(0.05)
        raise SystemExit(f"[{self.name}] timed out waiting for {what}")

    def wait_quiet(self, seconds=1.5, timeout=30.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if time.time() - self.last_event_at >= seconds:
                return
            time.sleep(0.1)

    def stop(self):
        self.stop_flag.set()
        self.join(timeout=5)

    def save(self, out):
        events_dir = os.path.join(out, "events")
        os.makedirs(events_dir, exist_ok=True)
        with open(os.path.join(events_dir, f"{self.name}.jsonl"), "w", encoding="utf-8") as handle:
            for event in self.snapshot():
                handle.write(json.dumps(event, ensure_ascii=False) + "\n")
        with open(os.path.join(events_dir, f"{self.name}.raw.txt"), "w", encoding="utf-8") as handle:
            handle.write(f"# {self.status_line}\n")
            for key in sorted(self.headers):
                if key in ("date", "keep-alive"):
                    continue
                handle.write(f"# {key}: {self.headers[key]}\n")
            handle.write(f"# First {RAW_FRAMES} frames of the de-chunked body, verbatim (frames end with a blank line).\n")
            for frame in self.frames[:RAW_FRAMES]:
                handle.write(frame + "\n\n")


def is_type(kind, session_id=None):
    def predicate(event):
        if event.get("type") != kind:
            return False
        return session_id is None or (event.get("data") or {}).get("sessionID") == session_id
    return predicate


class Capture:
    def __init__(self, args, password):
        self.args = args
        self.out = args.out
        self.client = Client(args.base, password, args.out)
        self.workspace = args.workspace
        self.loc = "location%5Bdirectory%5D=" + urllib.parse.quote(self.workspace, safe="")
        self.sessions = {}
        self.notes = {}

    # ---- helpers ------------------------------------------------------------
    def recorder(self, name):
        rec = SSERecorder(self.client, name)
        rec.start()
        rec.ready.wait(10)
        if rec.error:
            raise SystemExit(f"SSE failed: {rec.error}")
        rec.wait_for(is_type("server.connected"), 10, "server.connected")
        return rec

    def create_session(self, title=None, record_as=None):
        body = {"location": {"directory": self.workspace}}
        if title:
            body["title"] = title
        if record_as:
            result = self.client.record(record_as, "POST", "/api/session", body, expect=200)
        else:
            result = self.client.call("POST", "/api/session", body)
            if result["status"] != 200:
                raise SystemExit(f"create session failed: {result['status']} {result['text']}")
        return result["json"]["data"]["id"]

    def prompt(self, session_id, text, record_as=None, files=None, client_id=None):
        body = {"text": text}
        if client_id:
            body["id"] = client_id
        if files:
            body["files"] = files
        path = f"/api/session/{session_id}/prompt"
        if record_as:
            return self.client.record(record_as, "POST", path, body, "/api/session/{sessionID}/prompt", expect=200)
        result = self.client.call("POST", path, body)
        if result["status"] != 200:
            raise SystemExit(f"prompt failed: {result['status']} {result['text']}")
        return result

    def wait_done(self, rec, session_id, count=1, timeout=60.0, quiet=1.5):
        deadline = time.time() + timeout
        while time.time() < deadline:
            done = [e for e in rec.snapshot() if e.get("type") in TERMINAL and e["data"].get("sessionID") == session_id]
            if len(done) >= count:
                rec.wait_quiet(quiet, timeout=max(1.0, deadline - time.time()))
                return done
            time.sleep(0.05)
        raise SystemExit(f"[{rec.name}] session {session_id} did not finish")

    def messages(self, name, session_id, query="limit=50"):
        return self.client.record(name, "GET", f"/api/session/{session_id}/message?{query}",
                                  template="/api/session/{sessionID}/message", expect=200)

    def scenario(self, name, text, record_prompt=None, files=None, client_id=None, count=1, timeout=60.0):
        rec = self.recorder(name)
        sid = self.create_session()
        self.sessions[name] = sid
        self.prompt(sid, text, record_as=record_prompt, files=files, client_id=client_id)
        self.wait_done(rec, sid, count=count, timeout=timeout)
        rec.stop()
        rec.save(self.out)
        self.messages(f"session.messages.{name}", sid)
        return sid, rec

    # ---- capture steps ------------------------------------------------------
    def bootstrap(self):
        for _ in range(120):
            try:
                if self.client.call("GET", "/api/info")["status"] == 200:
                    break
            except OSError:
                pass
            time.sleep(0.5)
        self.client.record("info", "GET", "/api/info", expect=200)
        self.client.record("error.401.unauthenticated", "GET", "/api/info", auth=False,
                           note="No Authorization header.")
        self.client.record("project.list", "GET", "/api/project", expect=200)
        empty_polls = 0
        for _ in range(60):
            result = self.client.call("GET", f"/api/model?{self.loc}")
            if result["status"] == 200 and result["json"]["data"]:
                break
            empty_polls += 1
            time.sleep(0.5)
        self.notes["model.list.emptyPollsBeforeReady"] = empty_polls
        self.client.record("model.list", "GET", f"/api/model?{self.loc}", template="/api/model", expect=200)
        self.client.record("model.list.noLocation", "GET", "/api/model", expect=200,
                           note="No location query: resolved against the server process cwd.")
        self.client.record("model.default", "GET", f"/api/model/default?{self.loc}", template="/api/model/default",
                           expect=200)
        self.client.record("session.active.idle", "GET", "/api/session/active", expect=200)

    def session_basics(self):
        rec = self.recorder("session-lifecycle")
        sid = self.create_session(record_as="session.create")
        self.sessions["lifecycle"] = sid
        self.create_session(title="Titled harness session", record_as="session.create.titled")
        self.client.record("session.get", "GET", f"/api/session/{sid}", template="/api/session/{sessionID}", expect=200)
        self.client.record("session.rename", "PATCH", f"/api/session/{sid}", {"title": "Renamed by harness"},
                           template="/api/session/{sessionID}", expect=204)
        self.client.record("session.get.renamed", "GET", f"/api/session/{sid}", template="/api/session/{sessionID}",
                           expect=200)
        self.client.record("session.model.switch", "POST", f"/api/session/{sid}/model",
                           {"model": {"providerID": "mock", "id": "mock-model-alt", "variant": "high"}},
                           template="/api/session/{sessionID}/model", expect=204)
        self.client.record("session.get.modelSwitched", "GET", f"/api/session/{sid}",
                           template="/api/session/{sessionID}", expect=200)
        rec.wait_for(is_type("session.model.selected", sid), 10, "session.model.selected")
        rec.wait_quiet(1.0)
        rec.stop()
        rec.save(self.out)

    def scenarios(self):
        self.scenario("text", "Say hello. [[scenario:text]]", record_prompt="session.prompt",
                      client_id=ascending_id("msg_"))
        self.scenario("reasoning", "Think first. [[scenario:reasoning]]")
        self.scenario("tools", "Read two things at once. [[scenario:tools]]")
        self.scenario("long", "Write a lot. [[scenario:long]]")
        self.scenario("error", "Fail please. [[scenario:error]]")
        self.scenario("retry", "Fail once then recover. [[scenario:retry]]", timeout=90)
        files = [{"uri": f"data:image/png;base64,{PIXEL_PNG}", "name": "pixel.png"}]
        self.scenario("attachment", "Describe the attached image. [[scenario:text]]",
                      record_prompt="session.prompt.attachment", files=files)
        self.subagent()
        self.permission()
        self.child_permission()
        self.interrupt()

    def subagent(self):
        rec = self.recorder("subagent")
        sid = self.create_session()
        self.sessions["subagent"] = sid
        self.prompt(sid, "Delegate this. [[scenario:subagent]]")
        # Parent turn ends after launching the background child; the synthetic completion
        # then wakes the parent for a second execution.
        self.wait_done(rec, sid, count=2, timeout=60)
        rec.stop()
        rec.save(self.out)
        self.messages("session.messages.subagent", sid)
        child = next((e["data"]["sessionID"] for e in rec.snapshot()
                      if e.get("type") == "session.created" and e["data"].get("parentID") == sid), None)
        if child is None:
            child = next((e["data"]["metadata"]["sessionID"] for e in rec.snapshot()
                          if e.get("type") == "session.tool.progress"), None)
        if child:
            self.sessions["subagent-child"] = child
            self.client.record("session.get.child", "GET", f"/api/session/{child}",
                               template="/api/session/{sessionID}", expect=200)
            self.messages("session.messages.subagent-child", child)

    def permission(self):
        rec = self.recorder("permission")
        sid = self.create_session()
        self.sessions["permission"] = sid
        self.prompt(sid, "Run a shell command. [[scenario:permission]]")
        asked = rec.wait_for(is_type("permission.asked", sid), 30, "permission.asked")
        request_id = asked["data"]["id"]
        self.client.record("session.active.running", "GET", "/api/session/active", expect=200,
                           note="Captured while the permission scenario is blocked on a permission request.")
        self.client.record("permission.request.list", "GET", f"/api/permission/request?{self.loc}",
                           template="/api/permission/request", expect=200)
        self.client.record("permission.request.list.noLocation", "GET", "/api/permission/request",
                           note="No location query: the server falls back to its process cwd.")
        self.client.record("permission.request.list.directoryQuery", "GET",
                           "/api/permission/request?directory=" + urllib.parse.quote(self.workspace, safe=""),
                           template="/api/permission/request",
                           note="`directory=` is not the location selector in 2.0.8; `location[directory]=` is.")
        self.client.record("session.permission.list", "GET", f"/api/session/{sid}/permission",
                           template="/api/session/{sessionID}/permission", expect=200)
        self.client.record("session.permission.get", "GET", f"/api/session/{sid}/permission/{request_id}",
                           template="/api/session/{sessionID}/permission/{requestID}", expect=200)
        self.client.record("session.permission.reply", "POST",
                           f"/api/session/{sid}/permission/{request_id}/reply", {"decision": "once"},
                           template="/api/session/{sessionID}/permission/{requestID}/reply", expect=204)
        self.wait_done(rec, sid, timeout=60)
        rec.stop()
        rec.save(self.out)
        self.messages("session.messages.permission", sid)

    def child_permission(self):
        rec = self.recorder("subagent-permission")
        sid = self.create_session()
        self.sessions["subagent-permission"] = sid
        self.prompt(sid, "Delegate a shell command. [[scenario:subagent-permission]]")
        asked = rec.wait_for(is_type("permission.asked"), 30, "child permission.asked")
        child = asked["data"]["sessionID"]
        self.sessions["subagent-permission-child"] = child
        self.client.record("permission.request.list.child", "GET", f"/api/permission/request?{self.loc}",
                           template="/api/permission/request", expect=200,
                           note="The pending request belongs to the child session, not the root session.")
        self.client.record("session.list.root.withChildPending", "GET", "/api/session?parentID=null&limit=3",
                           template="/api/session", expect=200)
        self.client.record("session.permission.reply.child", "POST",
                           f"/api/session/{child}/permission/{asked['data']['id']}/reply", {"decision": "once"},
                           template="/api/session/{sessionID}/permission/{requestID}/reply", expect=204)
        self.wait_done(rec, sid, timeout=60)
        rec.stop()
        rec.save(self.out)
        self.messages("session.messages.subagent-permission", sid)
        self.messages("session.messages.subagent-permission-child", child)

    def interrupt(self):
        rec = self.recorder("interrupt")
        sid = self.create_session()
        self.sessions["interrupt"] = sid
        self.prompt(sid, "Stream slowly. [[scenario:slow]]")
        rec.wait_for(is_type("session.text.delta", sid), 30, "slow text delta")
        self.client.record("session.active.streaming", "GET", "/api/session/active", expect=200)
        self.prompt(sid, "Follow-up sent while busy. [[scenario:text]]", record_as="session.prompt.whileBusy")
        self.client.record("session.inbox.list", "GET", f"/api/session/{sid}/inbox",
                           template="/api/session/{sessionID}/inbox")
        time.sleep(1.0)
        self.client.record("session.interrupt", "POST", f"/api/session/{sid}/interrupt",
                           template="/api/session/{sessionID}/interrupt", expect=200)
        rec.wait_for(is_type("session.execution.interrupted", sid), 30, "execution.interrupted")
        # A follow-up that was still pending may run after the interrupt; wait for the stream to settle.
        rec.wait_quiet(3.0, timeout=30)
        self.client.record("session.interrupt.idle", "POST", f"/api/session/{sid}/interrupt",
                           template="/api/session/{sessionID}/interrupt", expect=200,
                           note="Interrupt with nothing running.")
        inbox = self.client.record("session.inbox.list.afterInterrupt", "GET", f"/api/session/{sid}/inbox",
                                   template="/api/session/{sessionID}/inbox", expect=200,
                                   note="Input sent while busy stays parked in the inbox after an interrupt.")
        for item in (inbox["json"] or {}).get("data") or []:
            self.client.record("session.inbox.cancel", "DELETE", f"/api/session/{sid}/inbox/{item['id']}",
                               template="/api/session/{sessionID}/inbox/{inboxID}")
            rec.wait_quiet(1.0, timeout=10)
            break
        rec.stop()
        rec.save(self.out)
        self.messages("session.messages.interrupt", sid)

    def history_paging(self):
        rec = self.recorder("history")
        sid = self.create_session()
        self.sessions["history"] = sid
        for index in range(4):
            self.prompt(sid, f"History turn {index + 1}. [[scenario:text]]")
            self.wait_done(rec, sid, count=index + 1, timeout=60, quiet=0.5)
        rec.stop()
        rec.save(self.out)
        page = 1
        query = "limit=3"
        while page <= 10:
            result = self.messages(f"session.messages.page{page}", sid, query)
            cursor = (result["json"] or {}).get("cursor") or {}
            if not result["json"]["data"] or not cursor.get("next"):
                break
            query = "limit=3&cursor=" + urllib.parse.quote(cursor["next"], safe="")
            page += 1
        self.notes["session.messages.pages"] = page
        self.messages("session.messages.asc", sid, "limit=3&order=asc")
        first = self.client.call("GET", f"/api/session/{sid}/message?limit=3")["json"]
        cursor = urllib.parse.quote(first["cursor"]["next"], safe="")
        self.client.record("error.400.orderWithCursor", "GET",
                           f"/api/session/{sid}/message?limit=3&order=asc&cursor={cursor}",
                           template="/api/session/{sessionID}/message")

    def session_listing(self):
        # Enough root sessions exist by now for several pages at limit=5.
        self.client.record("session.list.root.page1", "GET", "/api/session?parentID=null&limit=5",
                           template="/api/session", expect=200)
        page1 = self.client.call("GET", "/api/session?parentID=null&limit=5")["json"]
        cursor = (page1.get("cursor") or {}).get("next")
        if cursor:
            self.client.record("session.list.root.page2", "GET",
                               "/api/session?parentID=null&limit=5&cursor=" + urllib.parse.quote(cursor, safe=""),
                               template="/api/session", expect=200)
            self.client.record("session.list.root.orderWithCursor", "GET",
                               "/api/session?parentID=null&limit=5&order=asc&cursor="
                               + urllib.parse.quote(cursor, safe=""),
                               template="/api/session",
                               note="Unlike the message list, the session list does not reject order+cursor.")
        self.client.record("session.list.all", "GET", "/api/session?limit=100", template="/api/session", expect=200,
                           note="No parentID filter: child sessions are included.")
        self.client.record("session.list.directory", "GET",
                           "/api/session?limit=3&directory=" + urllib.parse.quote(self.workspace, safe=""),
                           template="/api/session", expect=200)
        self.client.record("error.404.session", "GET", "/api/session/ses_doesnotexist000000000000000",
                           template="/api/session/{sessionID}")
        self.client.record("error.400.invalidSessionID", "GET", "/api/session/not-a-session-id",
                           template="/api/session/{sessionID}")
        self.client.record("error.404.promptUnknownSession", "POST",
                           "/api/session/ses_doesnotexist000000000000000/prompt", {"text": "hello"},
                           template="/api/session/{sessionID}/prompt")
        self.client.record("session.active.afterAll", "GET", "/api/session/active", expect=200)
        self.client.record("project.list.afterSessions", "GET", "/api/project", expect=200)

    def forms(self):
        rec = self.recorder("form")
        sid = self.sessions["lifecycle"]
        fields = [
            {"key": "name", "type": "string", "title": "Name", "required": True, "placeholder": "Ada"},
            {"key": "count", "type": "integer", "title": "Count", "minimum": 1, "maximum": 5, "default": 2},
            {"key": "ok", "type": "boolean", "title": "Proceed?", "default": False},
            {"key": "tags", "type": "multiselect", "title": "Tags",
             "options": [{"value": "a", "label": "Alpha"}, {"value": "b", "label": "Beta"}]},
            {"key": "docs", "type": "external", "url": "https://example.invalid/docs", "title": "Docs"},
        ]
        created = self.client.record("session.form.create", "POST", f"/api/session/{sid}/form",
                                     {"title": "Harness form", "metadata": {"source": "harness"}, "fields": fields},
                                     template="/api/session/{sessionID}/form", expect=200)
        form_id = created["json"]["data"]["id"]
        rec.wait_for(is_type("form.created"), 10, "form.created")
        self.client.record("form.list", "GET", f"/api/form?{self.loc}", template="/api/form", expect=200)
        self.client.record("form.list.directoryQuery", "GET",
                           "/api/form?directory=" + urllib.parse.quote(self.workspace, safe=""),
                           template="/api/form")
        self.client.record("session.form.list", "GET", f"/api/session/{sid}/form",
                           template="/api/session/{sessionID}/form", expect=200)
        self.client.record("session.form.get", "GET", f"/api/session/{sid}/form/{form_id}",
                           template="/api/session/{sessionID}/form/{formID}", expect=200)
        self.client.record("session.form.cancel", "DELETE", f"/api/session/{sid}/form/{form_id}",
                           template="/api/session/{sessionID}/form/{formID}", expect=204)
        self.client.record("session.form.cancel.again", "DELETE", f"/api/session/{sid}/form/{form_id}",
                           template="/api/session/{sessionID}/form/{formID}",
                           note="Cancelling an already-cancelled form.")
        self.client.record("session.form.list.afterCancel", "GET", f"/api/session/{sid}/form",
                           template="/api/session/{sessionID}/form", expect=200)
        rec.wait_quiet(1.0)
        rec.stop()
        rec.save(self.out)

    def provider_log(self):
        with urllib.request.urlopen(self.args.mock + "/_log", timeout=10) as resp:
            log = json.loads(resp.read())
        summary = [{k: entry.get(k) for k in ("seq", "path", "scenario", "decision", "tools", "authorization_scheme")}
                   for entry in log]
        for item, entry in zip(summary, log):
            body = entry.get("body") or {}
            item["model"] = body.get("model")
            item["stream"] = body.get("stream")
            item["messageRoles"] = [m.get("role") for m in body.get("messages") or []]
        write_json(os.path.join(self.out, "provider", "requests-summary.json"), summary)
        # One untrimmed request, then one example per decision kind with the (identical)
        # system prompt and tool schemas elided to keep fixtures small.
        examples = {}
        for entry in log:
            key = entry.get("decision")
            messages = (entry.get("body") or {}).get("messages") or []
            if any(isinstance(m.get("content"), list) and any(p.get("type") == "image_url" for p in m["content"])
                   for m in messages):
                key = "attachment"
            examples.setdefault(key, entry)
        full = next((e for k, e in examples.items() if k == "text"), None)
        trimmed = {}
        for key, entry in examples.items():
            entry = json.loads(json.dumps(entry))
            body = entry.get("body") or {}
            if entry is not full and key != "text":
                if body.get("tools"):
                    body["tools"] = [f"<elided {t['function']['name']} schema>" for t in body["tools"]]
                for message in body.get("messages") or []:
                    if message.get("role") == "system" and isinstance(message.get("content"), str) \
                            and len(message["content"]) > 400:
                        message["content"] = message["content"][:400] + f"... <elided {len(message['content'])} chars>"
            trimmed[key] = entry
        write_json(os.path.join(self.out, "provider", "request-examples.json"), trimmed)

    def run(self):
        self.bootstrap()
        self.session_basics()
        self.scenarios()
        self.history_paging()
        self.forms()
        self.session_listing()
        self.provider_log()
        write_json(os.path.join(self.out, "index.json"), {
            "server": self.client.call("GET", "/api/info")["json"].get("version"),
            "workspace": self.workspace,
            "sessions": self.sessions,
            "notes": self.notes,
            "fixtures": self.client.index,
        })


def assert_no_secret(out, password, token):
    needles = [password, token, base64.b64encode(password.encode()).decode()]
    for root, _, files in os.walk(out):
        for filename in files:
            with open(os.path.join(root, filename), encoding="utf-8", errors="replace") as handle:
                content = handle.read()
            for needle in needles:
                if needle and needle in content:
                    raise SystemExit(f"secret leaked into {filename}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", required=True)
    parser.add_argument("--mock", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--workspace", default="/state/workspace")
    args = parser.parse_args()
    password = os.environ.get("OCGTK_V2H_PASSWORD")
    if not password:
        raise SystemExit("OCGTK_V2H_PASSWORD is required")
    for entry in os.listdir(args.out):
        if entry == "README.md":
            continue
        path = os.path.join(args.out, entry)
        shutil.rmtree(path) if os.path.isdir(path) else os.remove(path)
    urllib.request.urlopen(urllib.request.Request(args.mock + "/_reset", data=b"", method="POST"), timeout=10).read()
    capture = Capture(args, password)
    capture.run()
    assert_no_secret(args.out, password, capture.client.token)
    print(f"captured {len(capture.client.index)} fixtures into {args.out}")


if __name__ == "__main__":
    main()
