#!/usr/bin/env bash
# Headless end-to-end flow for the v2 client against tests/fake_opencode_server.py.
#
# Run it only headless (never on a live desktop):
#   CI:     xvfb-run --auto-servernum -s "-screen 0 1280x1024x24" bash tests/remote-flow-ui.sh
#   Docker: docker run --rm --platform linux/amd64 -v "$PWD":/app -w /app \
#             opencode-gtk-ui-test-amd64-v4:latest bash tests/remote-flow-ui.sh
# Without DISPLAY the script starts its own Xvfb; without a session bus it re-runs itself under
# dbus-run-session. Env knobs:
#   FLOW_BINARY=path      use a built client instead of `cargo build --locked`
#   FLOW_TIMEOUT=15       seconds to wait for each request-log marker
#   FLOW_DEADLINE=900     hard limit for the whole run
#   FLOW_KEEP=1           keep the temp dir (state.json, request log, app log)
#   FLOW_FORM_CANCEL_KEYS="ctrl+shift+x"     keys that cancel the form notice's form (default; see step 7)
#   FLOW_FORM_CANCEL_CLICK="dx,dy"           or a click, relative to the window's bottom-right corner
#                                            (set FLOW_FORM_CANCEL_KEYS= to use it)
#
# Every step asserts markers in the fake server's request log (tests/fake_v2/logwait.py) and
# reports PASS/FAIL per marker; nothing waits forever. This is the CP-012 acceptance test: it is
# expected to fail while the client is only partly ported to v2.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && -z "${FLOW_IN_DBUS:-}" ]] && command -v dbus-run-session >/dev/null; then
  FLOW_IN_DBUS=1 exec dbus-run-session -- bash "$0" "$@"
fi

timeout_s="${FLOW_TIMEOUT:-15}"
# The form notice's Cancel shortcut (ui.rs, pending::CANCEL_FORM_SHORTCUT).
FLOW_FORM_CANCEL_KEYS="${FLOW_FORM_CANCEL_KEYS-ctrl+shift+x}"
temporary="$(mktemp -d)"
log="${temporary}/requests.jsonl"
app_log="${temporary}/app.log"
server_pid=""
app_pid=""
xvfb_pid=""
watchdog_pid=""
window=""
mark=0
failures=()
passes=()

SES_MAIN="ses_f90000000001ffeIntegration"
SES_OTHER="ses_f90000000002ffeSecondSessn"
SES_STALE="ses_0000000000v1StaleTab000001"
BOOT_PERMISSION="per_000000000001BootPermissn1"
WORKSPACE="/state/workspace"
OTHER_DIR="/state/other"

cleanup() {
  [[ -z "${app_pid}" ]] || kill "${app_pid}" 2>/dev/null || true
  [[ -z "${server_pid}" ]] || kill "${server_pid}" 2>/dev/null || true
  [[ -z "${watchdog_pid}" ]] || kill "${watchdog_pid}" 2>/dev/null || true
  pkill -P $$ xclip 2>/dev/null || true
  [[ -z "${app_pid}" ]] || wait "${app_pid}" 2>/dev/null || true
  [[ -z "${server_pid}" ]] || wait "${server_pid}" 2>/dev/null || true
  [[ -z "${xvfb_pid}" ]] || kill "${xvfb_pid}" 2>/dev/null || true
  if [[ -n "${FLOW_KEEP:-}" ]]; then
    printf 'Kept %s\n' "${temporary}" >&2
  else
    rm -rf "${temporary}"
  fi
}
trap cleanup EXIT
(sleep "${FLOW_DEADLINE:-900}"; printf 'FLOW_DEADLINE reached\n' >&2; kill -TERM $$) &
watchdog_pid=$!

# ------------------------------------------------------------------ helpers

logq() { python3 tests/fake_v2/logwait.py "$@"; }
mark_now() { mark="$(logq seq "${log}")"; }
pass() { passes+=("$1"); printf 'PASS %s\n' "$1"; }
fail() { failures+=("$1"); printf 'FAIL %s: %s\n' "$1" "$2" >&2; }
app_alive() { [[ -n "${app_pid}" ]] && kill -0 "${app_pid}" 2>/dev/null; }

# expect NAME EXPR [TIMEOUT] [HINT] -- waits for a log record after ${mark}; prints it into ${found}.
found=""
expect() {
  local name="$1" expr="$2" wait="${3:-${timeout_s}}" hint="${4:-}"
  if ! app_alive && [[ "${name}" != server.* ]]; then
    fail "${name}" "client is not running"
    found=""
    return 1
  fi
  if found="$(logq wait "${log}" --after "${mark}" --timeout "${wait}" --expr "${expr}")"; then
    pass "${name}"
    return 0
  fi
  found=""
  fail "${name}" "no matching log record within ${wait}s: ${expr}${hint:+ -- ${hint}}"
  return 1
}

# expect_none NAME EXPR [HINT] -- no record in the whole log may match.
expect_none() {
  local name="$1" expr="$2" hint="${3:-}" offending
  if offending="$(logq none "${log}" --expr "${expr}")"; then
    pass "${name}"
  else
    fail "${name}" "unexpected requests${hint:+ (${hint})}:"$'\n'"${offending}"
  fi
}

field() { python3 -c 'import json,sys; r=json.loads(sys.argv[1]); print(eval(sys.argv[2], {}, {"r": r}))' "$1" "$2"; }

control() {
  python3 - "$(<"${temporary}/address")" "$1" <<'PY'
import json, sys, urllib.request
request = urllib.request.Request(sys.argv[1] + "/__control", data=sys.argv[2].encode(), method="POST",
                                 headers={"Content-Type": "application/json"})
with urllib.request.urlopen(request, timeout=10) as response:
    json.load(response)
PY
}

focus() { xdotool windowfocus --sync "${window}" 2>/dev/null || xdotool windowfocus "${window}" 2>/dev/null || true; }
key() { focus; xdotool key --clearmodifiers "$@"; sleep 0.3; }
type_text() { focus; xdotool type --delay 20 --clearmodifiers "$1"; sleep 0.3; }
geometry() { eval "$(xdotool getwindowgeometry --shell "${window}")"; }
click_at() { focus; xdotool mousemove --window "${window}" "$1" "$2" click 1; sleep 0.2; }

# click_until NAME EXPR "x,y x,y ..." -- tries each point until the marker shows up.
click_until() {
  local name="$1" expr="$2" points="$3" point
  for point in ${points}; do
    app_alive || break
    click_at "${point%,*}" "${point#*,}"
    if found="$(logq wait "${log}" --after "${mark}" --timeout 2 --expr "${expr}")"; then
      pass "${name}"
      return 0
    fi
  done
  expect "${name}" "${expr}" 3 "clicked ${points} (layout estimates; see ui.rs composer/prompt margins)"
}

wait_for_window() {
  local candidate
  for _ in $(seq 1 200); do
    app_alive || return 1
    candidate="$(xdotool search --onlyvisible --name '^OpenCode$' 2>/dev/null | tail -n 1)"
    if [[ -n "${candidate}" ]]; then
      window="${candidate}"
      return 0
    fi
    sleep 0.1
  done
  return 1
}

# ------------------------------------------------------------ environment

if [[ -z "${DISPLAY:-}" ]]; then
  command -v Xvfb >/dev/null || { printf 'No DISPLAY and no Xvfb; run under xvfb-run in Docker/CI\n' >&2; exit 1; }
  for display in $(seq 90 120); do
    [[ -e "/tmp/.X11-unix/X${display}" || -e "/tmp/.X${display}-lock" ]] && continue
    Xvfb ":${display}" -screen 0 1280x1024x24 -nolisten tcp >/dev/null 2>&1 &
    xvfb_pid=$!
    export DISPLAY=":${display}"
    break
  done
  for _ in $(seq 1 50); do xdpyinfo >/dev/null 2>&1 && break; xdotool getdisplaygeometry >/dev/null 2>&1 && break; sleep 0.1; done
fi

password="$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
FAKE_OPENCODE_PASSWORD="${password}" python3 tests/fake_opencode_server.py \
  --address-file "${temporary}/address" \
  --log-file "${log}" \
  --workspace "${WORKSPACE}" \
  --other-directory "${OTHER_DIR}" \
  --cwd /state/home \
  --history-turns 120 \
  --extra-sessions 120 \
  --boot-permission \
  --heartbeat-s 2 \
  --slow-delay-ms 700 \
  --slow-deltas 40 \
  --models-empty-once \
  --delay-bootstrap-ms 600 \
  --race session.list:rename \
  >"${temporary}/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do [[ -s "${temporary}/address" ]] && break; sleep 0.1; done
[[ -s "${temporary}/address" ]] || { printf 'fake server did not start\n' >&2; cat "${temporary}/server.log" >&2; exit 1; }
address="$(<"${temporary}/address")"

# v2-era state: v2 session ids plus one stale v1 id that the client must drop quietly (R2.8).
# Field names follow src/persist.rs (PersistedState/ServerState/PersistedTab).
mkdir -p "${temporary}/config/opencode-gtk"
python3 - "${address}" "${temporary}/config/opencode-gtk/state.json" <<PY
import json, sys
server, path = sys.argv[1:]
state = {
    "connection": {"server": server, "username": "opencode", "cloudflare_access": False},
    "servers": {
        server.rstrip("/"): {
            "tabs": [
                {"id": "${SES_MAIN}", "directory": "${WORKSPACE}", "title": "Integration session"},
                {"id": "${SES_OTHER}", "directory": "${OTHER_DIR}", "title": "Second session"},
                {"id": "${SES_STALE}", "directory": "${WORKSPACE}", "title": "Old v1 tab"},
            ],
            "active": "${SES_MAIN}",
            "selections": {},
            "unread": [],
            "busy": [],
        }
    },
    "zoom_level": 1.0,
}
with open(path, "w", encoding="utf-8") as stream:
    json.dump(state, stream)
PY

if [[ -n "${FLOW_BINARY:-}" ]]; then
  binary="${FLOW_BINARY}"
else
  cargo build --locked || { printf 'cargo build failed\n' >&2; exit 1; }
  binary="${CARGO_TARGET_DIR:-target}/debug/opencode-gtk"
fi

XDG_CONFIG_HOME="${temporary}/config" \
XDG_DATA_HOME="${temporary}/data" \
XDG_CACHE_HOME="${temporary}/cache" \
GSETTINGS_BACKEND=memory \
GDK_BACKEND=x11 \
GTK_A11Y=none \
NO_AT_BRIDGE=1 \
OPENCODE_SERVER_PASSWORD="${password}" \
"${binary}" --server "${address}" --username opencode >"${app_log}" 2>&1 &
app_pid=$!

# ------------------------------------------------------------ 1. bootstrap

if wait_for_window; then
  pass "ui.window"
else
  fail "ui.window" "main window 'OpenCode' never appeared"
fi

boot_wait=$((timeout_s + 10))
expect "bootstrap.info" 'http and route == "server.info" and r["status"] == 200' "${boot_wait}" "v2 bootstrap starts with GET /api/info (R2.1)" \
  || timeout_s=3  # nothing v2 is happening; fail the rest fast instead of waiting on each marker
expect "bootstrap.projects" 'http and route == "project.list"'
expect "bootstrap.sessions" 'http and route == "session.list" and q.get("parentID") == "null" and "cursor" not in q' "${boot_wait}"
expect "bootstrap.sessions.paged" 'http and route == "session.list" and "cursor" in q and "limit" in q' "${boot_wait}" \
  "122 root sessions exist; follow cursor.next and resend limit (R2.3)"
expect "bootstrap.active" 'http and route == "session.active"'
expect "bootstrap.permissions" 'http and route == "permission.request.list" and "location[directory]" in q' "${timeout_s}" \
  "pending permissions per location with location[directory] (R7.1)"
expect "bootstrap.forms" 'http and route == "form.list" and "location[directory]" in q' "${timeout_s}" "pending forms (CP-011)"
if expect "models.list" 'http and route == "model.list" and "location[directory]" in q'; then
  first_models="$(field "${found}" 'r["seq"]')"
  saved_mark="${mark}"; mark="${first_models}"
  expect "models.refetch-after-empty" 'http and route == "model.list"' "${timeout_s}" \
    "the first catalog is empty, then model.updated arrives (R5.5)"
  mark="${saved_mark}"
fi
expect "messages.initial" "http and route == 'message.list' and p.get('sessionID') == '${SES_MAIN}' and 'cursor' not in q and 'order' not in q"

# ------------------------------------------------------------ 2. permission reply (boot request, no "save": Deny + Allow once)

if [[ -n "${window}" ]] && app_alive; then
  geometry
  mark_now
  # The permission prompt replaces the composer (ui.rs show_permission); "Allow once" is the
  # rightmost button when `save` is absent. First point = button centre measured on a 1180x820
  # Xvfb screenshot of the current layout (Allow once ~ W-90,H-57; send/Stop ~ W-55,H-51).
  click_until "permission.reply" \
    "http and route == 'session.permission.reply' and p.get('sessionID') == '${SES_MAIN}' and p.get('requestID') == '${BOOT_PERMISSION}' and b.get('decision') in ('once', 'always', 'reject')" \
    "$((WIDTH - 90)),$((HEIGHT - 57)) $((WIDTH - 75)),$((HEIGHT - 48)) $((WIDTH - 100)),$((HEIGHT - 64))"
  [[ -z "${found}" ]] || printf '     decision: %s\n' "$(field "${found}" 'r["body"]["decision"]')"
fi

# ------------------------------------------------------------ 3. older history via cursor

if [[ -n "${window}" ]] && app_alive; then
  mark_now
  key ctrl+1
  xdotool mousemove --window "${window}" "$((WIDTH * 6 / 10))" "$((HEIGHT / 2))"
  for _ in $(seq 1 80); do xdotool click 4; done
  sleep 0.5
  # "Load earlier messages" sits right under the session header when scrolled to the top.
  click_until "messages.older-with-cursor" \
    "http and route == 'message.list' and p.get('sessionID') == '${SES_MAIN}' and 'cursor' in q and 'order' not in q" \
    "$((WIDTH * 6 / 10)),100 $((WIDTH * 6 / 10)),112 $((WIDTH * 6 / 10)),90 $((WIDTH * 6 / 10)),124 $((WIDTH * 6 / 10)),80"
fi

# ------------------------------------------------------------ 4. rename (F2 overlay)

if [[ -n "${window}" ]] && app_alive; then
  mark_now
  key F2
  sleep 0.3
  key ctrl+a
  type_text "Renamed integration session"
  key Return
  expect "rename" "http and route == 'session.update' and p.get('sessionID') == '${SES_MAIN}' and b.get('title') == 'Renamed integration session' and r['status'] == 204"
fi

# ------------------------------------------------------------ 5. create (Ctrl+T overlay, first project)

new_session=""
if [[ -n "${window}" ]] && app_alive; then
  mark_now
  key ctrl+t
  sleep 0.3
  key Return
  if expect "create" "http and route == 'session.create' and b.get('location') == {'directory': '${WORKSPACE}'} and 'agent' not in b['keys'] and 'model' not in b['keys']" \
    "${timeout_s}" "POST /api/session {location:{directory}}, no agent/model (R2.6, R5.4)"; then
    if found="$(logq wait "${log}" --after "${mark}" --timeout 3 --expr 'ev == "session.created"')"; then
      new_session="$(field "${found}" 'r["sessionID"]')"
    fi
  fi
fi

# ------------------------------------------------------------ 6. prompt with an attachment, then interrupt

if [[ -n "${window}" ]] && app_alive && [[ -n "${new_session}" ]]; then
  mark_now
  key ctrl+g
  printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=' \
    | base64 -d >"${temporary}/pixel.png"
  xclip -selection clipboard -t image/png -i "${temporary}/pixel.png"
  key ctrl+v
  sleep 0.5
  type_text "Flow prompt [[scenario:slow]]"
  key Return
  if expect "prompt" "http and route == 'session.prompt' and p.get('sessionID') == '${new_session}' and 'Flow prompt' in (b.get('text') or '') and r['status'] == 200"; then
    prompt_record="${found}"
    mark="$(( $(field "${prompt_record}" 'r["seq"]') - 1 ))"
    expect "prompt.client-id" "http and route == 'session.prompt' and str(b.get('id') or '').startswith('msg_')" 1 "client-generated msg_ id (R4.2)"
    expect "prompt.attachment" "http and route == 'session.prompt' and len(b.get('files') or []) == 1 and b['files'][0]['scheme'] == 'data' and b['files'][0].get('canonical') and b['files'][0].get('sniffedMime') == 'image/png' and b['files'][0].get('name')" 1 \
      "files[] data: URI with canonical base64 and a name (R4.5)"
    expect "prompt.no-agent-delivery" "http and route == 'session.prompt' and not any(k in b['keys'] for k in ('agent', 'agents', 'delivery', 'resume'))" 1 "R4.1/R5.4"
  fi
  mark_now
  if expect "prompt.streaming" "ev == 'session.text.delta' and r.get('sessionID') == '${new_session}'" "${timeout_s}"; then
    geometry
    # While busy the send button becomes Stop (send_or_abort); it is the rightmost composer control.
    click_until "interrupt" "http and route == 'session.interrupt' and p.get('sessionID') == '${new_session}'" \
      "$((WIDTH - 55)),$((HEIGHT - 51)) $((WIDTH - 50)),$((HEIGHT - 46)) $((WIDTH - 60)),$((HEIGHT - 56))"
  else
    fail "interrupt" "never saw the prompt stream, so Stop was not tried"
  fi
elif [[ -n "${window}" ]]; then
  fail "prompt" "skipped: no session was created"
fi

# ------------------------------------------------------------ 7. form notice + Cancel

if [[ -n "${window}" ]] && app_alive && [[ -n "${new_session}" ]]; then
  logq wait "${log}" --after 0 --timeout 30 --expr "ev in ('session.execution.interrupted', 'session.execution.succeeded') and r.get('sessionID') == '${new_session}'" >/dev/null
  mark_now
  key ctrl+g
  type_text "Need input [[scenario:form]]"
  key Return
  if expect "form.created" "ev == 'form.created'" "${timeout_s}" "prompt with [[scenario:form]] was not sent"; then
    sleep 1
    # The one-line notice above the composer (CP-011) cancels its form with Ctrl+Shift+X;
    # FLOW_FORM_CANCEL_CLICK can click its Cancel button instead.
    if [[ -n "${FLOW_FORM_CANCEL_KEYS:-}" ]]; then
      # shellcheck disable=SC2086
      key ${FLOW_FORM_CANCEL_KEYS}
    elif [[ -n "${FLOW_FORM_CANCEL_CLICK:-}" ]]; then
      geometry
      click_at "$((WIDTH - ${FLOW_FORM_CANCEL_CLICK%,*}))" "$((HEIGHT - ${FLOW_FORM_CANCEL_CLICK#*,}))"
    fi
    expect "form.cancel" "http and route == 'session.form.cancel' and p.get('sessionID') in ('${new_session}', 'global') and r['status'] in (204, 409)" "${timeout_s}" \
      "DELETE /api/session/{id}/form/{formID} via the notice's Cancel (FLOW_FORM_CANCEL_KEYS/CLICK)"
  fi
fi

# ------------------------------------------------------------ 8. child-session permission

if [[ -n "${window}" ]] && app_alive && [[ -n "${new_session}" ]]; then
  logq wait "${log}" --after 0 --timeout 20 --expr "ev == 'session.execution.succeeded' and r.get('sessionID') == '${new_session}'" >/dev/null
  mark_now
  key ctrl+g
  type_text "Delegate [[scenario:child-permission]]"
  key Return
  if expect "permission.child-asked" "ev == 'permission.asked' and r.get('sessionID') not in (None, '${new_session}')" "${timeout_s}"; then
    child_session="$(field "${found}" 'r["sessionID"]')"
    geometry
    # With `save` present the buttons are Deny, Allow once, Always allow (rightmost).
    click_until "permission.child-reply" \
      "http and route == 'session.permission.reply' and p.get('sessionID') == '${child_session}'" \
      "$((WIDTH - 90)),$((HEIGHT - 57)) $((WIDTH - 75)),$((HEIGHT - 48)) $((WIDTH - 100)),$((HEIGHT - 64))"
  fi
fi

# ------------------------------------------------------------ 9. SSE reconnect after faults

if app_alive; then
  mark_now
  control '{"action": "drop_sse"}'
  if expect "sse.reconnect" 'r["kind"] == "sse.open" and r["n"] >= 2' "${timeout_s}" "no replay: reconnect after an abrupt drop (R6.2)"; then
    mark="$(field "${found}" 'r["seq"]')"
    expect "sse.reconnect-resync" 'http and route in ("session.list", "server.info", "session.active")' "${timeout_s}" "refetch after reconnect (R6.2/P3)"
  fi
  mark_now
  control '{"action": "restart", "down_ms": 1500}'
  if expect "restart.reconnect" 'r["kind"] == "sse.open"' "$((timeout_s + 10))" "reconnect after a server restart"; then
    mark="$(field "${found}" 'r["seq"]')"
    expect "restart.resync" 'http and route in ("session.list", "server.info")' "${timeout_s}"
  fi
fi

# ------------------------------------------------------------ 10. whole-run invariants

expect_none "no.unknown-routes" 'http and route in (None, "method.not_allowed")' "v1 or unknown routes (e.g. /global/event, /api/agent)"
expect_none "no.bad-auth" 'http and r["auth"] != "ok"' "Basic auth on every request, including SSE"
expect_none "no.agent" 'http and route in ("session.prompt", "session.create") and any(k in b["keys"] for k in ("agent", "agents"))' "R5.4"
expect_none "no.order-with-cursor" 'http and route == "message.list" and "cursor" in q and "order" in q' "R3.2"
expect_none "no.plain-directory" 'http and route in ("model.list", "model.default", "permission.request.list", "form.list") and "directory" in q' \
  "location-scoped routes need location[directory]"
expect_none "no.form-answer" 'http and route == "session.form.reply"' "never auto-answer forms (R8)"
expect_none "no.blank-rename" 'http and route == "session.update" and not (b.get("title") or "").strip()' "R2.7"
expect_none "no.server-errors" 'http and r["status"] >= 500'

# ------------------------------------------------------------ 11. persisted state

sleep 1
if python3 - "${temporary}/config/opencode-gtk/state.json" "${address}" "${new_session}" <<PY
import json, sys
path, server, new_session = sys.argv[1:]
state = json.load(open(path, encoding="utf-8"))
tabs = {tab["id"]: tab for tab in state["servers"][server.rstrip("/")]["tabs"]}
problems = []
if tabs.get("${SES_MAIN}", {}).get("title") != "Renamed integration session":
    problems.append("renamed title not persisted")
if "${SES_STALE}" in tabs:
    problems.append("stale v1 tab was not dropped (R2.8)")
if new_session and new_session not in tabs:
    problems.append("created session has no tab")
if tabs.get("${SES_OTHER}", {}).get("title") != "Raced title":
    problems.append("event that raced the bootstrap snapshot was lost (P3): %r" % tabs.get("${SES_OTHER}", {}).get("title"))
print("; ".join(problems))
sys.exit(1 if problems else 0)
PY
then
  pass "state.persisted"
else
  fail "state.persisted" "see above"
fi

app_alive && pass "ui.still-running" || fail "ui.still-running" "client exited"

# ------------------------------------------------------------ summary

printf '\n%d passed, %d failed\n' "${#passes[@]}" "${#failures[@]}"
if ((${#failures[@]})); then
  printf 'Failed: %s\n' "${failures[*]}" >&2
  printf -- '--- client log (tail) ---\n' >&2
  tail -n 30 "${app_log}" >&2 || true
  printf -- '--- last requests ---\n' >&2
  python3 - "${log}" <<'PY' >&2
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
for row in [r for r in rows if r["kind"] in ("http", "sse.open", "sse.close")][-30:]:
    if row["kind"] == "http":
        print(row["seq"], row["method"], row["path"], row.get("query") or "", row["status"], row.get("error") or "")
    else:
        print(row["seq"], row["kind"], row.get("n"), row.get("reason", ""))
PY
  exit 1
fi
