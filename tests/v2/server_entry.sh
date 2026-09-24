#!/bin/sh
# Starts an isolated OpenCode 2.0.8 server. All state lives under /state (tmpfs).
# OPENCODE_PASSWORD must be provided by `docker run -e OPENCODE_PASSWORD`.
set -eu
umask 077

if [ -z "${OPENCODE_PASSWORD:-}" ]; then
  echo "OPENCODE_PASSWORD is required" >&2
  exit 2
fi

export HOME=/state/home
export XDG_DATA_HOME=/state/data
export XDG_CONFIG_HOME=/state/config
export XDG_CACHE_HOME=/state/cache
export XDG_STATE_HOME=/state/xdg-state
export TMPDIR=/state/tmp
mkdir -p "$HOME" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME/opencode" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$TMPDIR"

workspace=/state/workspace
rm -rf "$workspace"
cp -R /harness/workspace-seed "$workspace"
cd "$workspace"
git init -q -b main
git -c user.name=harness -c user.email=harness@example.invalid add -A
git -c user.name=harness -c user.email=harness@example.invalid commit -q -m "Seed workspace"

export OPENCODE_CONFIG=/harness/opencode.json
export OPENCODE_CONFIG_DIR="$XDG_CONFIG_HOME/opencode"
export OPENCODE_DISABLE_MODELS_FETCH=1
export OPENCODE_DISABLE_AUTOUPDATE=1
export OPENCODE_DISABLE_PROJECT_CONFIG=1
export OPENCODE_DISABLE_FILEWATCHER=1

# Deliberately start outside the workspace so requests without a location selector
# (which fall back to the server cwd) are distinguishable from /state/workspace.
cd "$HOME"
exec /opt/opencode/bin/opencode serve --hostname 0.0.0.0 --port 4096
