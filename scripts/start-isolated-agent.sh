#!/usr/bin/env bash
set -euo pipefail

# Launch an isolated QuickAgent instance for self-verification without touching
# the live desktop database, config, sidebar sessions, or ports.
#
# Usage:
#   ./scripts/start-isolated-agent.sh [--copy-providers] [--bind 127.0.0.1:0] [--isolated-dir /tmp/dir] [--web-dir path]
#
# Ready lines printed to stdout:
#   GROW_ISOLATED_DIR=...
#   GROW_DESKTOP_PORT=<port>
#   GROW_READY_URL=http://127.0.0.1:<port>/agent/

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN=""
if [[ -x "$ROOT/target/release/codex-workspace-mcp" && -x "$ROOT/target/debug/codex-workspace-mcp" ]]; then
  if [[ "$ROOT/target/release/codex-workspace-mcp" -nt "$ROOT/target/debug/codex-workspace-mcp" ]]; then
    BIN="$ROOT/target/release/codex-workspace-mcp"
  else
    BIN="$ROOT/target/debug/codex-workspace-mcp"
  fi
elif [[ -x "$ROOT/target/debug/codex-workspace-mcp" ]]; then
  BIN="$ROOT/target/debug/codex-workspace-mcp"
elif [[ -x "$ROOT/target/release/codex-workspace-mcp" ]]; then
  BIN="$ROOT/target/release/codex-workspace-mcp"
else
  echo "error: no built codex-workspace-mcp binary found under target/{debug,release}/; run cargo build first" >&2
  exit 1
fi

if [[ ! -f "$ROOT/frontend/dist/index.html" ]]; then
  echo "error: frontend/dist/index.html missing; run 'npm --prefix frontend run build' first" >&2
  exit 1
fi

exec "$BIN" --isolated "$@"
