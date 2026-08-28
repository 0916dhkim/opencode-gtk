#!/usr/bin/env bash
set -euo pipefail

cargo build --locked
binary="${CARGO_TARGET_DIR:-target}/debug/opencode-gtk"
GDK_BACKEND=x11 "${binary}" \
  --server http://127.0.0.1:9 \
  --username smoke-test &
pid=$!
trap 'kill "${pid}" 2>/dev/null || true; wait "${pid}" 2>/dev/null || true' EXIT

main_window=""
for _ in {1..50}; do
  if main_window="$(xdotool search --onlyvisible --name '^OpenCode$' 2>/dev/null | tail -n 1)" && [[ -n "${main_window}" ]]; then
    break
  fi
  if ! kill -0 "${pid}" 2>/dev/null; then
    wait "${pid}"
    exit 1
  fi
  sleep 0.1
done
[[ -n "${main_window}" ]]

xdotool windowfocus "${main_window}"
xdotool key --window "${main_window}" ctrl+t
for _ in {1..50}; do
  if xdotool search --onlyvisible --name '^New session$' >/dev/null 2>&1; then
    exit 0
  fi
  sleep 0.1
done

printf '%s\n' 'Ctrl+T did not open the new-session dialog' >&2
exit 1
