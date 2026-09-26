#!/usr/bin/env bash
# Headless smoke test for the COSMIC client: the window opens and survives the
# everyday shortcuts, both in --preview mode (canned v2 data, no network) and
# against an unreachable server. Run it only headless:
#   CI:     xvfb-run --auto-servernum bash tests/smoke-ui.sh
#   Docker: docker run --rm --platform linux/amd64 -v "$PWD":/repo -w /repo \
#             opencode-cosmic-builder-amd64:latest bash tests/smoke-ui.sh
#           (that image needs `apt-get install -y xdotool` first)
# Without DISPLAY it starts its own Xvfb. SMOKE_BINARY=path skips the build;
# SMOKE_SHOTS=dir saves a screenshot of each run.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && -z "${SMOKE_IN_DBUS:-}" ]] && command -v dbus-run-session >/dev/null; then
  SMOKE_IN_DBUS=1 exec dbus-run-session -- bash "$0" "$@"
fi

temporary="$(mktemp -d)"
xvfb_pid=""
pid=""
cleanup() {
  [[ -z "${pid}" ]] || kill "${pid}" 2>/dev/null || true
  [[ -z "${pid}" ]] || wait "${pid}" 2>/dev/null || true
  [[ -z "${xvfb_pid}" ]] || kill "${xvfb_pid}" 2>/dev/null || true
  rm -rf "${temporary}"
}
trap cleanup EXIT

if [[ -z "${DISPLAY:-}" ]]; then
  for display in $(seq 90 120); do
    [[ -e "/tmp/.X11-unix/X${display}" || -e "/tmp/.X${display}-lock" ]] && continue
    Xvfb ":${display}" -screen 0 1180x820x24 -nolisten tcp >/dev/null 2>&1 &
    xvfb_pid=$!
    export DISPLAY=":${display}"
    break
  done
  for _ in $(seq 1 50); do xdotool getdisplaygeometry >/dev/null 2>&1 && break; sleep 0.1; done
fi

if [[ -n "${SMOKE_BINARY:-}" ]]; then
  binary="${SMOKE_BINARY}"
else
  cargo build --locked
  binary="${CARGO_TARGET_DIR:-target}/debug/opencode-gtk"
fi

alive() { kill -0 "${pid}" 2>/dev/null; }

# run NAME ARGS... -- starts the client, waits for its window, sends shortcuts.
run() {
  local name="$1" window="" key
  shift
  mkdir -p "${temporary}/${name}/runtime"
  chmod 700 "${temporary}/${name}/runtime"
  XDG_CONFIG_HOME="${temporary}/${name}/config" \
  XDG_DATA_HOME="${temporary}/${name}/data" \
  XDG_CACHE_HOME="${temporary}/${name}/cache" \
  XDG_RUNTIME_DIR="${temporary}/${name}/runtime" \
  GSETTINGS_BACKEND=memory \
  "${binary}" "$@" >"${temporary}/${name}.log" 2>&1 &
  pid=$!
  # The first start in a fresh container also builds the font cache.
  for _ in {1..300}; do
    window="$(xdotool search --onlyvisible --name '^OpenCode( Preview)?$' 2>/dev/null | tail -n 1)" || true
    [[ -n "${window}" ]] && break
    alive || { cat "${temporary}/${name}.log" >&2; printf '%s: client exited before its window appeared\n' "${name}" >&2; exit 1; }
    sleep 0.1
  done
  [[ -n "${window}" ]] || { cat "${temporary}/${name}.log" >&2; printf '%s: no main window\n' "${name}" >&2; exit 1; }
  sleep 1
  # New session, sessions, rename and settings overlays, each closed again;
  # then a prompt typed into the composer.
  for key in ctrl+t Escape ctrl+p Escape F2 Escape ctrl+comma Escape ctrl+g; do
    xdotool windowfocus "${window}" 2>/dev/null || true
    xdotool key --clearmodifiers "${key}"
    sleep 0.3
  done
  xdotool type --delay 10 --clearmodifiers "smoke test"
  xdotool key --clearmodifiers Return
  sleep 1.5
  alive || { cat "${temporary}/${name}.log" >&2; printf '%s: client exited\n' "${name}" >&2; exit 1; }
  if grep -qi panicked "${temporary}/${name}.log"; then
    cat "${temporary}/${name}.log" >&2
    exit 1
  fi
  if [[ -n "${SMOKE_SHOTS:-}" ]] && command -v import >/dev/null; then
    mkdir -p "${SMOKE_SHOTS}"
    import -window root "${SMOKE_SHOTS}/smoke-${name}.png" || true
  fi
  kill "${pid}"
  wait "${pid}" 2>/dev/null || true
  pid=""
  printf 'PASS %s\n' "${name}"
}

run preview --preview
run unreachable --server http://127.0.0.1:9 --username smoke-test
