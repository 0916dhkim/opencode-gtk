# OpenCode 2.0.8 protocol fixtures

Captured from a real, isolated `@opencode/cli@2.0.8` server by `tests/v2/harness.sh capture`
(see `tests/v2/README.md`). IDs and timestamps change on every capture; shapes don't.

## HTTP fixtures (`<name>.json`)

```json
{
  "name": "session.get",
  "request": {
    "method": "GET",
    "path": "/api/session/ses_…",
    "pathTemplate": "/api/session/{sessionID}",
    "query": "limit=3&cursor=…",
    "body": null
  },
  "response": {
    "status": 200,
    "headers": { "content-type": "application/json", "content-length": "879" },
    "body": { "data": { "…": "…" } }
  },
  "note": "optional capture remark"
}
```

- `request.query` is the raw query string (or `null`); `request.body` is the JSON sent (or `null`).
- `response.body` is the parsed JSON body, unmodified, so tests can deserialize it directly.
  It is `null` for empty bodies (204, and the 401 which has no body). A non-JSON body would be
  kept as a string in `response.bodyText`.
- `index.json` lists every fixture (name, method, path template, query, status) plus the session
  IDs per scenario and capture notes.

## SSE fixtures (`events/<scenario>.jsonl`, `events/<scenario>.raw.txt`)

- `.jsonl`: one parsed `GET /api/event` event per line, in arrival order, starting with
  `server.connected`. Heartbeat comment frames are not included.
- `.raw.txt`: response status/headers as `#` comments, then the first frames of the
  de-chunked body verbatim (each frame ends with a blank line), including `: heartbeat` comments.

## Provider traffic (`provider/`)

- `requests-summary.json`: every request the mock provider received (path, scenario, decision,
  tool names, message roles).
- `request-examples.json`: one request body per decision; the `text` entry is untrimmed, the
  others have the shared system prompt and tool schemas elided.

The Basic password never appears here; `capture.py` asserts that before finishing.
