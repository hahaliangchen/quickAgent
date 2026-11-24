---
name: self-verify
description: Launch a fully isolated secondary QuickAgent instance (`--isolated`) to end-to-end verify frontend or backend changes in a real browser without touching the live desktop database, ports, or sidebar sessions.
---

# Self-Verify QuickAgent with an Isolated Instance

When modifying QuickAgent's own frontend (`frontend/`) or backend (`src/`), NEVER launch `./target/.../codex-workspace-mcp --desktop-agent` directly against the live desktop environment. Always use the built-in `--isolated` mode.

## Standard Workflow

1. **Build what you changed**:
   - Frontend changes: `npm --prefix frontend run build`
   - Rust backend changes: `cargo build` (or `cargo build --features internal-daily-report` when verifying the internal daily-report plugin)

2. **Start an isolated QuickAgent instance**:
   - UI / API checks without live LLM calls:
     ```bash
     ./scripts/start-isolated-agent.sh
     ```
   - Checks that also need real LLM conversations (read-only copy of provider config into the isolated temp dir):
     ```bash
     ./scripts/start-isolated-agent.sh --copy-providers
     ```
   - Optional flags: `--bind 127.0.0.1:<port>` (defaults to random free port `127.0.0.1:0`), `--isolated-dir <dir>` (defaults to auto-cleaned temp dir), `--web-dir <path>` (defaults to `frontend/dist`).
   - Read the ready URL directly from stdout:
     - `GROW_READY_URL=http://127.0.0.1:<port>/agent/`
     - `GROW_DESKTOP_PORT=<port>`
     - `GROW_ISOLATED_DIR=<path>`

3. **Verify in the browser**:
   - Open `GROW_READY_URL` with `browser.open`, test both light & dark themes, capture screenshots with `browser.screenshot`, and inspect with `vision.analyze_images`.

4. **Clean shutdown**:
   - Stop the isolated process with `SIGTERM` / `process.kill_session` when done. The server gracefully shuts down all bundled plugin sidecars and automatically removes its temporary `GROW_ISOLATED_DIR`.
