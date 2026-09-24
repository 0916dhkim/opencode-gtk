# OpenCode 2.0.8 test harness

Runs a real, isolated `@opencode/cli@2.0.8` server (npm SHA-512 verified at build time) next to a
scripted OpenAI-compatible mock provider, on the internal Docker network `ocgtk-v2h-net` (no egress).

```sh
tests/v2/harness.sh build
tests/v2/harness.sh up --state /path/outside/repo      # writes <state>/password (0600)
tests/v2/harness.sh capture --state /path/outside/repo # rewrites tests/fixtures/v2-2.0.8/
tests/v2/harness.sh status
tests/v2/harness.sh down --state /path/outside/repo
```

- Server: `http://ocgtk-v2h-server:4096`, Basic auth user `opencode`, random per-`up` password.
  Other containers reach it by joining `ocgtk-v2h-net`; no host port is published.
- Server state (HOME/XDG/DB) lives on tmpfs and is gone after `down`. The workspace is a fresh git
  repo seeded from `workspace/` at `/state/workspace`; the server's cwd is `/state/home`.
- Config: `opencode.json` (mock provider `mock/mock-model` and `mock/mock-model-alt`, the built-in
  `opencode` provider plugin removed, `websearch: false`, `shell` asks, `question` denied last).
- `capture --out DIR` writes elsewhere (for example to diff two runs).

## Mock provider scenarios

`mock_provider.py` serves `POST /v1/chat/completions` (streaming). Put a marker in the prompt:

| Marker | Behavior |
| --- | --- |
| none / `[[scenario:text]]` | short text reply |
| `[[scenario:reasoning]]` | `reasoning_content` deltas, then text |
| `[[scenario:tools]]` | two tool calls in one step (`read` README.md, `glob` `**/*.txt`), then text |
| `[[scenario:permission]]` | `shell` call that hits the `ask` rule |
| `[[scenario:subagent]]` | background `subagent` call; the child replies with text |
| `[[scenario:subagent-permission]]` | foreground `subagent` whose child calls `shell` (child permission) |
| `[[scenario:error]]` | HTTP 500 with `x-should-retry: false` |
| `[[scenario:retry]]` | first attempt 503 (`x-should-retry: true`), then text |
| `[[scenario:slow]]` | 20 deltas, 0.5 s apart |
| `[[scenario:long]]` | 300 deltas |

Requests without tools (title generation, compaction) get a short plain reply. `GET /_log` on the
mock returns every request it received; `POST /_reset` clears it.
