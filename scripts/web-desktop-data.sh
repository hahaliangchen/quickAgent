#!/usr/bin/env bash
# Run the web version on the desktop app's own data: the same conversations,
# modules and settings. Desktop and web differ only in the UI shell.
#
#   scripts/web-test-desktop-sessions.sh serve   # build and start the backend
#
# Only one backend may use the data at a time, so this refuses to start while
# the desktop app's backend is running. Close the desktop app first.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
workspace="${GROW_DESKTOP_WORKSPACE:-$HOME/.local/share/com.grow.agent/workspace}"
bind="${WEB_TEST_AGENT_BIND:-127.0.0.1:3201}"
mcp_bind="${WEB_TEST_MCP_BIND:-127.0.0.1:3200}"

case "${1:-}" in
  serve)
    [ -d "$workspace" ] || { echo "no desktop data at $workspace" >&2; exit 1; }
    if ps -eo args= | grep -q '[g]row-agent --desktop-agent'; then
      echo "the desktop app's backend is running on this data; close the desktop app first" >&2
      exit 1
    fi
    cargo build --manifest-path "$root/Cargo.toml" -q
    # Claude's shell sets XDG_CONFIG_HOME to its own profile; Grow must read ~/.config/grow.
    exec env -u XDG_CONFIG_HOME WORKSPACE_ROOT="$workspace" AGENT_BIND="$bind" MCP_BIND="$mcp_bind" \
      "$root/target/debug/codex-workspace-mcp"
    ;;
  *)
    echo "usage: $0 serve" >&2
    exit 2
    ;;
esac
