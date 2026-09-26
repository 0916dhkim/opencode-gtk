#!/usr/bin/env bash
# Headless keyboard-shortcut check for the COSMIC client against the fake v2
# server (tests/fake_opencode_server.py). Every keyboard effect is asserted as
# a request in the server's log, so this verifies the shortcuts end to end
# instead of only proving that the client survives them (tests/smoke-ui.sh).
#   CI:     xvfb-run --auto-servernum bash tests/shortcut-ui.sh
#   Docker: docker run --rm --platform linux/amd64 -v "$PWD":/repo -w /repo \
#             opencode-cosmic-builder-amd64:latest bash tests/shortcut-ui.sh
#           (that image needs `apt-get install -y xdotool` first)
# Without DISPLAY it starts its own Xvfb. Env knobs:
#   SHORTCUT_BINARY=path   use a built client instead of `cargo build --locked`
#   SHORTCUT_TIMEOUT=15    seconds to wait for each request-log marker
#   SHORTCUT_KEEP=1        keep the temp dir (state, request log, app log)
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && -z "${SHORTCUT_IN_DBUS:-}" ]] && command -v dbus-run-session >/dev/null; then
  SHORTCUT_IN_DBUS=1 exec dbus-run-session -- bash "$0" "$@"
fi

timeout_s="${SHORTCUT_TIMEOUT:-15}"
temporary="$(mktemp -d)"
log="${temporary}/requests.jsonl"
app_log="${temporary}/app.log"
server_pid=""
app_pid=""
xvfb_pid=""
window=""
mark=0
passes=0
failures=0

cleanup() {
  [[ -z "${app_pid}" ]] || kill "${app_pid}" 2>/dev/null || true
  [[ -z "${server_pid}" ]] || kill "${server_pid}" 2>/dev/null || true
  [[ -z "${app_pid}" ]] || wait "${app_pid}" 2>/dev/null || true
  [[ -z "${server_pid}" ]] || wait "${server_pid}" 2>/dev/null || true
  [[ -z "${xvfb_pid}" ]] || kill "${xvfb_pid}" 2>/dev/null || true
  if [[ -n "${SHORTCUT_KEEP:-}" ]]; then
    printf 'Kept %s\n' "${temporary}" >&2
  else
    rm -rf "${temporary}"
  fi
}
trap cleanup EXIT

pass() { passes=$((passes + 1)); printf 'PASS %s\n' "$1"; }
fail() { failures=$((failures + 1)); printf 'FAIL %s: %s\n' "$1" "$2" >&2; }
logq() { python3 tests/fake_v2/logwait.py "$@"; }

# expect NAME EXPR [TIMEOUT] -- waits for a matching request-log record.
found=""
expect() {
  local name="$1" expr="$2" wait="${3:-${timeout_s}}"
  if found="$(logq wait "${log}" --after "${mark}" --timeout "${wait}" --expr "${expr}")"; then
    pass "${name}"
  else
    found=""
    fail "${name}" "no matching record within ${wait}s: ${expr}"
  fi
}

# expect_count NAME EXPR N -- exactly N records must match (guards double sends).
expect_count() {
  local name="$1" expr="$2" want="$3" got
  got="$(logq count "${log}" --after "${mark}" --expr "${expr}")"
  if [[ "${got}" == "${want}" ]]; then
    pass "${name}"
  else
    fail "${name}" "expected ${want} matching record(s), got ${got}: ${expr}"
  fi
}

alive() { [[ -n "${app_pid}" ]] && kill -0 "${app_pid}" 2>/dev/null; }

if [[ -z "${DISPLAY:-}" ]]; then
  for display in $(seq 90 120); do
    [[ -e "/tmp/.X11-unix/X${display}" || -e "/tmp/.X${display}-lock" ]] && continue
    Xvfb ":${display}" -screen 0 1280x1024x24 -nolisten tcp >/dev/null 2>&1 &
    xvfb_pid=$!
    export DISPLAY=":${display}"
    break
  done
  for _ in $(seq 1 50); do xdotool getdisplaygeometry >/dev/null 2>&1 && break; sleep 0.1; done
fi

password="$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
FAKE_OPENCODE_PASSWORD="${password}" python3 tests/fake_opencode_server.py \
  --address-file "${temporary}/address" \
  --log-file "${log}" \
  --step-delay-ms 1500 \
  --slow-delay-ms 1500 \
  --slow-deltas 12 \
  >"${temporary}/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do [[ -s "${temporary}/address" ]] && break; sleep 0.1; done
if [[ ! -s "${temporary}/address" ]]; then
  printf 'fake server did not start\n' >&2
  cat "${temporary}/server.log" >&2
  exit 1
fi
address="$(<"${temporary}/address")"

if [[ -n "${SHORTCUT_BINARY:-}" ]]; then
  binary="${SHORTCUT_BINARY}"
else
  cargo build --locked || { printf 'cargo build failed\n' >&2; exit 1; }
  binary="${CARGO_TARGET_DIR:-target}/debug/opencode-gtk"
fi

mkdir -p "${temporary}/config" "${temporary}/data" "${temporary}/cache" "${temporary}/runtime"
chmod 700 "${temporary}/runtime"
XDG_CONFIG_HOME="${temporary}/config" \
XDG_DATA_HOME="${temporary}/data" \
XDG_CACHE_HOME="${temporary}/cache" \
XDG_RUNTIME_DIR="${temporary}/runtime" \
GSETTINGS_BACKEND=memory \
OPENCODE_SERVER_PASSWORD="${password}" \
"${binary}" --server "${address}" --username opencode >"${app_log}" 2>&1 &
app_pid=$!

for _ in {1..300}; do
  window="$(xdotool search --onlyvisible --name '^OpenCode$' 2>/dev/null | tail -n 1)" || true
  [[ -n "${window}" ]] && break
  alive || { cat "${app_log}" >&2; printf 'client exited before its window appeared\n' >&2; exit 1; }
  sleep 0.1
done
if [[ -z "${window}" ]]; then
  cat "${app_log}" >&2
  printf 'no main window\n' >&2
  exit 1
fi

focus() { xdotool windowfocus --sync "${window}" 2>/dev/null || xdotool windowfocus "${window}" 2>/dev/null || true; }
key() { focus; xdotool key --clearmodifiers "$@"; sleep 0.3; }
type_text() { focus; xdotool type --delay 20 --clearmodifiers "$1"; sleep 0.3; }
mark_now() { mark="$(logq seq "${log}")"; }

# The client bootstraps the fake server's main session on start, so the window
# only becomes ready once the bootstrap round trip is done.
for _ in $(seq 1 100); do
  [[ "$(logq count "${log}" --expr "http and route == 'session.list'")" != "0" ]] && break
  sleep 0.1
done
sleep 1

# ---------------------------------------------------------------- Ctrl+T
# Opens GTK's new-session palette; Enter takes the first location and Ctrl+G
# then leaves the caret in the composer.
mark_now
key ctrl+t
key Return
expect "ctrl-t.create" "http and route == 'session.create' and r['status'] == 200"
sleep 1

# -------------------------------------------------- Enter sends a prompt
mark_now
key ctrl+g
type_text "shortcut note one"
key Return
expect "enter.prompt" \
  "http and route == 'session.prompt' and 'shortcut note one' in (b.get('text') or '') and 'delivery' not in b['keys'] and r['status'] == 200"
expect_count "enter.prompt-once" \
  "http and route == 'session.prompt' and 'shortcut note one' in (b.get('text') or '')" 1

# --------------------------- Enter steers and Ctrl+Enter queues while running
# The fake run is slow (--step-delay-ms 1500), so the session is still busy.
# Neither message re-focuses the composer: typing here also checks that the
# caret survives the tray filling up while the run streams.
mark_now
type_text "shortcut steer two"
key Return
expect "enter.steer" \
  "http and route == 'session.prompt' and 'shortcut steer two' in (b.get('text') or '') and 'delivery' not in b['keys']"
expect_count "enter.steer-once" \
  "http and route == 'session.prompt' and 'shortcut steer two' in (b.get('text') or '')" 1

mark_now
type_text "shortcut queue three"
key ctrl+Return
expect "ctrl-enter.queue" \
  "http and route == 'session.prompt' and 'shortcut queue three' in (b.get('text') or '') and b.get('delivery') == 'queue' and r['status'] == 200"
expect_count "ctrl-enter.queue-once" \
  "http and route == 'session.prompt' and 'shortcut queue three' in (b.get('text') or '')" 1

# ------------------------------------------- overlay and layout shortcuts
# Ctrl+P and Ctrl+, open the drawer, Escape closes it, Ctrl+B folds the sidebar
# and Ctrl+W closes the active tab. None of them sends anything, so the check is
# that the client stays alive and never panics.
mark_now
for combo in ctrl+p Escape ctrl+comma Escape ctrl+b; do
  key "${combo}"
  alive || { cat "${app_log}" >&2; fail "overlay-keys.alive" "client exited on ${combo}"; break; }
done
if alive; then pass "overlay-keys.alive"; fi
if grep -qi panicked "${app_log}"; then
  cat "${app_log}" >&2
  fail "no-panic" "client logged a panic"
else
  pass "no-panic"
fi

mark_now
key ctrl+w
sleep 0.5
if alive; then pass "ctrl-w.close-tab.alive"; else cat "${app_log}" >&2; fail "ctrl-w.close-tab.alive" "client exited"; fi
expect_count "no.unexpected-prompts" \
  "http and route == 'session.prompt' and 'shortcut' not in (b.get('text') or '')" 0

printf '\n%s passed, %s failed\n' "${passes}" "${failures}"
[[ "${failures}" -eq 0 ]]
