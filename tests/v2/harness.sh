#!/usr/bin/env bash
# Isolated OpenCode 2.0.8 server harness for opencode-gtk.
#
#   tests/v2/harness.sh build
#   tests/v2/harness.sh up --state DIR        # DIR must be outside the repo
#   tests/v2/harness.sh capture --state DIR [--out DIR]
#   tests/v2/harness.sh status
#   tests/v2/harness.sh down
#
# The server runs on the internal Docker network `ocgtk-v2h-net` (no egress)
# as http://ocgtk-v2h-server:4096. Other containers join that network to reach it.
# OCGTK_V2H_PREFIX (default `ocgtk-v2h`) renames the containers and network
# (`$PREFIX-server`, `$PREFIX-mock`, `$PREFIX-net`), so two runs can coexist.
set -euo pipefail

export PATH="/usr/local/bin:$PATH"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
IMAGE="ocgtk-v2h-server:2.0.8"
PREFIX="${OCGTK_V2H_PREFIX:-ocgtk-v2h}"
case "$PREFIX" in *[!a-z0-9-]*|"") echo "harness: invalid OCGTK_V2H_PREFIX" >&2; exit 1 ;; esac
NET="$PREFIX-net"
SERVER="$PREFIX-server"
MOCK="$PREFIX-mock"
BASE_URL="http://$SERVER:4096"
MOCK_URL="http://$MOCK:4100"

STATE_DIR="${OCGTK_V2H_STATE:-}"
OUT_DIR="$REPO/tests/fixtures/v2-2.0.8"

die() { echo "harness: $*" >&2; exit 1; }

parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --state) STATE_DIR="$2"; shift 2 ;;
      --out) OUT_DIR="$2"; shift 2 ;;
      *) die "unknown argument: $1" ;;
    esac
  done
}

require_state() {
  [ -n "$STATE_DIR" ] || die "--state DIR (or OCGTK_V2H_STATE) is required"
  mkdir -p "$STATE_DIR"
  STATE_DIR="$(cd "$STATE_DIR" && pwd)"
  case "$STATE_DIR/" in
    "$REPO"/*) die "state dir must be outside the repository" ;;
  esac
  chmod 700 "$STATE_DIR"
}

hardening=(--cap-drop ALL --security-opt no-new-privileges --pids-limit 256 --memory 2g --read-only)

cmd_build() {
  docker build -t "$IMAGE" "$HERE"
}

cmd_up() {
  require_state
  docker image inspect "$IMAGE" >/dev/null 2>&1 || die "image $IMAGE missing; run build first"
  if docker ps -aq --filter "name=^$PREFIX-" | grep -q .; then
    die "harness containers already exist; run down first"
  fi
  docker network inspect "$NET" >/dev/null 2>&1 || docker network create --internal "$NET" >/dev/null

  local password
  password="$(head -c 32 /dev/urandom | base64 | tr -d '/+=\n' | head -c 40)"
  (umask 077; printf '%s' "$password" > "$STATE_DIR/password")
  chmod 600 "$STATE_DIR/password"
  printf '%s\n' "$BASE_URL" > "$STATE_DIR/base_url"

  # opencode.json names the mock `ocgtk-v2h-mock`; the alias is per network.
  docker run -d --name "$MOCK" --network "$NET" --network-alias "$MOCK" --network-alias ocgtk-v2h-mock \
    "${hardening[@]}" --tmpfs /tmp:rw,nosuid,nodev,size=64m \
    "$IMAGE" python3 /harness/mock_provider.py --port 4100 >/dev/null

  OPENCODE_PASSWORD="$password" docker run -d --name "$SERVER" --network "$NET" --network-alias "$SERVER" \
    "${hardening[@]}" -e OPENCODE_PASSWORD \
    --tmpfs /state:rw,nosuid,nodev,size=512m,uid=1000,gid=1000,mode=0700 \
    --tmpfs /tmp:rw,nosuid,nodev,size=128m,uid=1000,gid=1000,mode=0700 \
    "$IMAGE" /harness/server_entry.sh >/dev/null

  local i
  for i in $(seq 1 120); do
    if docker exec "$SERVER" python3 -c '
import base64, os, sys, urllib.request
token = base64.b64encode(("opencode:" + os.environ["OPENCODE_PASSWORD"]).encode()).decode()
req = urllib.request.Request("http://127.0.0.1:4096/api/info", headers={"authorization": "Basic " + token})
sys.exit(0 if urllib.request.urlopen(req, timeout=2).status == 200 else 1)
' >/dev/null 2>&1; then
      echo "server ready: $BASE_URL (network $NET)"
      echo "password file: $STATE_DIR/password"
      return 0
    fi
    sleep 1
  done
  docker logs --tail 50 "$SERVER" >&2 || true
  die "server did not become ready"
}

cmd_capture() {
  require_state
  [ -f "$STATE_DIR/password" ] || die "no password in $STATE_DIR; run up first"
  mkdir -p "$OUT_DIR"
  OUT_DIR="$(cd "$OUT_DIR" && pwd)"
  OCGTK_V2H_PASSWORD="$(cat "$STATE_DIR/password")" docker run --rm --network "$NET" \
    "${hardening[@]}" --tmpfs /tmp:rw,nosuid,nodev,size=64m \
    -e OCGTK_V2H_PASSWORD \
    -v "$HERE:/src:ro" -v "$OUT_DIR:/out" \
    "$IMAGE" python3 /src/capture.py --base "$BASE_URL" --mock "$MOCK_URL" --out /out --workspace /state/workspace
}

cmd_status() {
  docker ps -a --filter "name=^$PREFIX-" --format '{{.Names}}\t{{.Status}}\t{{.Image}}'
  docker network ls --filter "name=^$PREFIX-" --format '{{.Name}}\t{{.Driver}}'
}

cmd_down() {
  local names
  names="$(docker ps -aq --filter "name=^$PREFIX-")"
  if [ -n "$names" ]; then
    # shellcheck disable=SC2086
    docker rm -f -v $names >/dev/null
  fi
  if docker network inspect "$NET" >/dev/null 2>&1; then
    docker network rm "$NET" >/dev/null
  fi
  if [ -n "$STATE_DIR" ] && [ -f "$STATE_DIR/password" ]; then
    rm -f "$STATE_DIR/password" "$STATE_DIR/base_url"
  fi
  echo "harness down"
}

sub="${1:-}"
[ -n "$sub" ] || die "usage: harness.sh build|up|capture|status|down [--state DIR] [--out DIR]"
shift
parse_args "$@"
case "$sub" in
  build) cmd_build ;;
  up) cmd_up ;;
  capture) cmd_capture ;;
  status) cmd_status ;;
  down) cmd_down ;;
  *) die "unknown subcommand: $sub" ;;
esac
