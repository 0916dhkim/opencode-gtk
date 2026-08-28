#!/usr/bin/env bash
set -euo pipefail

temporary="$(mktemp -d)"
server_pid=""
server_two_pid=""
app_pid=""
cleanup() {
  [[ -z "${app_pid}" ]] || kill "${app_pid}" 2>/dev/null || true
  [[ -z "${server_pid}" ]] || kill "${server_pid}" 2>/dev/null || true
  [[ -z "${server_two_pid}" ]] || kill "${server_two_pid}" 2>/dev/null || true
  [[ -z "${app_pid}" ]] || wait "${app_pid}" 2>/dev/null || true
  [[ -z "${server_pid}" ]] || wait "${server_pid}" 2>/dev/null || true
  [[ -z "${server_two_pid}" ]] || wait "${server_two_pid}" 2>/dev/null || true
  rm -rf "${temporary}"
}
trap cleanup EXIT

wait_for_file() {
  local path="$1"
  for _ in {1..100}; do
    [[ -s "${path}" ]] && return 0
    sleep 0.1
  done
  return 1
}

wait_for_event() {
  local expected="$1"
  local events_file="${2:-${temporary}/events}"
  for _ in {1..100}; do
    if [[ -f "${events_file}" ]] && [[ "$(<"${events_file}")" == *"${expected}"* ]]; then
      return 0
    fi
    sleep 0.1
  done
  printf 'Timed out waiting for fake-server event: %s\n' "${expected}" >&2
  if [[ -f "${events_file}" ]]; then
    printf '%s\n' "$(<"${events_file}")" >&2
  fi
  return 1
}

wait_for_window() {
  local title="$1"
  local window=""
  for _ in {1..100}; do
    while read -r candidate; do
      window="${candidate}"
    done < <(xdotool search --onlyvisible --name "^${title}$" 2>/dev/null || true)
    if [[ -n "${window}" ]]; then
      printf '%s\n' "${window}"
      return 0
    fi
    sleep 0.1
  done
  printf 'Timed out waiting for window: %s\n' "${title}" >&2
  return 1
}

wait_for_no_window() {
  local title="$1"
  local quiet=0
  for _ in {1..50}; do
    if ! xdotool search --onlyvisible --name "^${title}$" >/dev/null 2>&1; then
      quiet=$((quiet + 1))
      if [[ "${quiet}" -ge 5 ]]; then
        return 0
      fi
    else
      quiet=0
    fi
    sleep 0.1
  done
  printf 'Window remained open: %s\n' "${title}" >&2
  return 1
}

python3 tests/fake_opencode_server.py \
  --address-file "${temporary}/address" \
  --events-file "${temporary}/events" &
server_pid=$!
wait_for_file "${temporary}/address"
python3 tests/fake_opencode_server.py \
  --address-file "${temporary}/address-two" \
  --events-file "${temporary}/events-two" &
server_two_pid=$!
wait_for_file "${temporary}/address-two"

mkdir -p "${temporary}/config/opencode-gtk"
python3 - "$(<"${temporary}/address")" "${temporary}/config/opencode-gtk/state.json" <<'PY'
import json
import sys

server, path = sys.argv[1:]
state = {
    "connection": {
        "server": server,
        "username": "opencode",
        "cloudflare_access": False,
    },
    "theme": "dark",
    "servers": {
        server.rstrip("/"): {
            "tabs": [
                {"id": "ses_test", "directory": "/repo", "title": "Integration session"},
                {"id": "ses_other", "directory": "/repo", "title": "Second session"},
            ],
            "active": "ses_test",
            "selections": {},
        }
    },
}
with open(path, "w", encoding="utf-8") as stream:
    json.dump(state, stream)
PY

cargo build --locked
binary="${CARGO_TARGET_DIR:-target}/debug/opencode-gtk"
XDG_CONFIG_HOME="${temporary}/config" \
GSETTINGS_BACKEND=memory \
GDK_BACKEND=x11 \
"${binary}" --server "$(<"${temporary}/address")" &
app_pid=$!

main_window="$(wait_for_window OpenCode)"
permission_window="$(wait_for_window 'Permission required')"
xdotool windowfocus "${permission_window}"
eval "$(xdotool getwindowgeometry --shell "${permission_window}")"
xdotool mousemove --window "${permission_window}" "$((WIDTH - 270))" "$((HEIGHT - 30))" click 1
wait_for_event "permission:reject"
wait_for_no_window 'Permission required'

wait_for_event "messages:ses_test"
xdotool windowfocus "${main_window}"
xdotool key --window "${main_window}" ctrl+2
wait_for_event "messages:ses_other"
xdotool key --window "${main_window}" ctrl+1

xdotool windowfocus "${main_window}"
xdotool key --window "${main_window}" ctrl+t
new_session_window="$(wait_for_window 'New session')"
xdotool windowfocus "${new_session_window}"
xdotool key --window "${new_session_window}" Return
wait_for_event "session:create:New session"
wait_for_no_window 'New session'

xdotool windowfocus "${main_window}"
xdotool mousemove --window "${main_window}" 90 70 mousedown 1
for x in 140 190 240 290 320; do
  xdotool mousemove --sync --window "${main_window}" "${x}" 70
  sleep 0.1
done
xdotool mouseup 1
sleep 0.5
python3 - "${temporary}/config/opencode-gtk/state.json" "$(<"${temporary}/address")" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
server = sys.argv[2].rstrip("/")
tabs = state["servers"][server]["tabs"]
assert [tab["id"] for tab in tabs[:2]] == ["ses_other", "ses_test"], state
PY

xdotool windowfocus "${main_window}"
xdotool key --window "${main_window}" ctrl+comma
settings_window="$(wait_for_window Settings)"
xdotool windowfocus "${settings_window}"
xclip -selection clipboard <"${temporary}/address-two"
xdotool key --window "${settings_window}" ctrl+a
xdotool key --window "${settings_window}" ctrl+v
sleep 0.5
xdotool key --window "${settings_window}" Tab
xdotool key --window "${settings_window}" alt+l
sleep 0.5
eval "$(xdotool getwindowgeometry --shell "${settings_window}")"
xdotool mousemove --window "${settings_window}" "$((WIDTH - 55))" "$((HEIGHT - 30))" click 1
wait_for_no_window Settings
wait_for_event "sse:1" "${temporary}/events-two"

permission_window="$(wait_for_window 'Permission required')"
xdotool windowfocus "${permission_window}"
eval "$(xdotool getwindowgeometry --shell "${permission_window}")"
xdotool mousemove --window "${permission_window}" "$((WIDTH - 270))" "$((HEIGHT - 30))" click 1
wait_for_event "permission:reject" "${temporary}/events-two"
wait_for_no_window 'Permission required'

python3 - "${temporary}/config/opencode-gtk/state.json" "$(<"${temporary}/address-two")" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
assert state["connection"]["server"] == sys.argv[2], state
assert state["theme"] == "light", state
PY

xdotool windowfocus "${main_window}"
xdotool mousemove --window "${main_window}" 500 680 click 1
printf '%s' $'integration\nping' | xclip -selection clipboard
xdotool key --window "${main_window}" ctrl+v
sleep 0.5
xdotool key --window "${main_window}" Return
wait_for_event $'prompt:integration\nping' "${temporary}/events-two"

question_window="$(wait_for_window 'OpenCode needs your input')"
xdotool windowfocus "${question_window}"
xdotool key --window "${question_window}" space Tab Tab Tab Return
wait_for_event "question:" "${temporary}/events-two"
wait_for_event "sse:2" "${temporary}/events-two"
kill -0 "${app_pid}"
