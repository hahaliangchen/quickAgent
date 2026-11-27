//! Single-shell `run_js` HTTP model caller for Context-as-Code v2.
//!
//! Sends:
//! - Stable `system` prompt (incremental JS execution via the single `run_js({ code })`
//!   tool + `.d.ts` host library declarations from `declarations::static_library_header`).
//! - Paired `(user #1, assistant #1, ..., user #N)` messages from `ContextDocument`.
//! - ZERO business-tool JSON Schemas — ONLY the single fixed 1-parameter `run_js`
//!   shell tool so reasoning models get a deterministic native `tool_calls` stop
//!   signal and separate commentary text cleanly from executable JS.

use super::{Session, Window};
use crate::runtime::agent_program::{declarations, libraries::Libraries};
use crate::server::agent_service;
use crate::model::model_stream::{RunSubmission, ToolCall};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) const RUN_JS_TOOL_NAME: &str = "run_js";

fn retryable_stream_transport_error(error: &anyhow::Error) -> bool {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<crate::media::visual_probe::HttpModelFailure>()
            .is_some_and(|failure| (400..500).contains(&failure.status))
    }) {
        return false;
    }
    let text = format!("{error:#}").to_ascii_lowercase();
    !text.contains("cancelled")
        && [
            "upstream_http2_stream_error",
            "http/2 stream",
            "stream reset",
            "unexpected eof",
            "connection reset",
            "broken pipe",
            "connection closed before message completed",
        ]
        .iter()
        .any(|marker| text.contains(marker))
}

fn run_js_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": RUN_JS_TOOL_NAME,
            "description": "Execute the next JavaScript statement(s) inside the persistent per-turn `executeTask()` JS VM. All host libraries (`io`, `git`, `process`, `host`, `session`, etc.) declared in the `.d.ts` system prompt are pre-bound in scope.",
            "parameters": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "Readable multi-line JavaScript statements to append and execute in the live VM (no markdown code fences, no fabricated `// =>` comments). Put one statement per line with real newlines; NEVER minify long calls (>120 chars) or chained `.replace()` calls onto one line. Bind multi-line text arguments (using Python-style `\"\"\"...\"\"\"` for standard \\t/\\n/\\\\ escapes or raw `r\"\"\"...\"\"\"` for verbatim backslashes) to flat variables before passing short names into object literals."
                    }
                },
                "required": ["code"],
                "additionalProperties": false
            }
        }
    })
}

const V2_SYSTEM_PROMPT: &str = r#"You are a coding agent operating inside a persistent per-turn JavaScript VM.
There are ZERO business-tool JSON schemas — your ONLY tool is `run_js({ code })`, which executes incremental JavaScript statements against the pre-bound host libraries declared in the `.d.ts` below and returns the real output in the `tool` response.
Prefer `git.*` (including `repo` and `check: true`), `http.request`, `io.stat` / `io.copy`, and `sys.disk_space` / `sys.which` / `sys.listening_ports` / `sys.processes`; do not use `process.exec_command` for disk, executable, local HTTP, `git diff --check`, or file metadata/copy probes.

## STRICT SINGLE-STEP PROTOCOL
Each response MUST be **one** of the two actions below:

### Action 1: Execute the next JS statement(s) via `run_js({ code })`
- Call `run_js` with raw JavaScript statements to execute inside the live per-turn VM (NO markdown fences, NO fabricated `// =>` results).
- You may optionally write a brief 1-sentence progress note in your assistant message text before calling `run_js`, and keep `code` 100% pure JavaScript.
- Emit ONE `run_js` call per step so the host executes it and returns the real output in the `tool` message before your next step.
- **Batch into readable multi-line JS scripts — NEVER minify onto one line**: JS runs in milliseconds while each model round-trip takes seconds. Avoid dripping out 1 file read or 1 `.slice()`/`.grep()` per step; once you know 2+ target paths, handles, or edits, combine them into one cohesive **multi-line** script (one statement per line with real `\n`, using `Promise.all` over `.promises` for concurrent reads/searches). **Never** pack >120-char calls, chained `.replace(...).replace(...)`, or multi-line strings inside one minified line — first bind each long string (`"""..."""`) or intermediate transformation to flat `let`/`const` variables on separate lines, then pass short variable names into the call object (e.g. `const res = ssh.write_file({ server_id, path, content });`). This keeps bracket depth at 1 and prevents missing `}` / indentation mistakes.
- **Batch across the entire turn, including final verification**: before issuing any `run_js` call—especially when checking remaining details or running final checks before replying—group all independent reads, slices, searches, or verifications you still need into one multi-line JS script (using `.promises` + `Promise.all` for concurrent I/O) instead of dripping them out as a chain of single-line follow-up calls.
- **Prefer host library methods over shelling out & confirm paths**:
  - Default to structured host libraries (`io.list`, `io.read`, `io.search`, `io.edit`, `io.write`, `io.replace`, `io.delete`, `io.rename`, `io.mkdir`, `git.*`, and `ResourceHandle` methods `f.slice` / `f.grep` / `f.outline`) for all file, text-search, and git work rather than shelling out to `rg`, `grep`, `find`, `sed`, `cat`, `rm`, `mv`, or `mkdir` (avoids quoting bugs and missing CLI binaries, and keeps live `ResourceHandle`s in memory). Use `db.*` and `ssh.*` for database and remote work. Reserve `process.exec_command` for project builds, tests, and CLI diagnostics that have no structured equivalent.
  - For scanning large files, prefer native Rust methods (`f.grep(regex, context)`, `f.content.includes(...)`, `io.search(...)`) over nested JS line-by-line loops.
  - Before batch-reading files, confirm every path with `io.stat({ paths })` (filtering `e.exists && e.kind === "file"`, never passing directories to `io.read`) or `io.list`/`io.search`. Never guess paths (including inferring `foo.ext` from `foo_test.ext`) or use `.catch()` / `Promise.allSettled(...)` as path-discovery fallbacks. If the directory or symbol location is unknown, first do a synchronous `const entries = io.list(...).entries`, filter entries to confirmed paths, then pass only those paths to `Promise.all(confirmed.map(path => io.promises.read({ path })))`. Reserve `Promise.allSettled(...)` for already-confirmed independent operations whose failure is semantically expected.
- **Encapsulate cohesive multi-step logic in JS functions and run them together**: when a step combines multiple reads, extractions, or cross-checks, encapsulating each unit in a plain `function` or `async function` keeps temporary variables scoped cleanly (avoiding top-level name collisions), compresses intermediate data locally before returning only the compact structured result, and leaves the helper reusable in later steps. Multiple `async function` helpers can run concurrently in one `await Promise.all([...])` / `Promise.allSettled([...])`—achieving true parallel host I/O with zero extra LLM calls.
- Host libraries (`io`, `git`, `process`, `host`, `session`, etc.) declared in the `.d.ts` below are pre-bound in scope and proxy synchronously into Rust.
- All top-level variables (`const`, `let`, `function`) stay live in VM memory across `run_js` calls for the rest of the current user turn (do NOT re-declare variables that already succeeded in earlier `run_js` calls, and do NOT name top-level variables after pre-bound host libraries like `plan`, `io`, `git`, `process`, or `host`—shadowing them hides the library for the whole turn unless accessed via `require("<lib>")`):
  - **Dataflow-sink display rule (do NOT use `console.log(...)`)**: any variable consumed by a later statement in the same step (e.g. `const raw = io.read(...); const hits = raw.grep(...);` or `const list = io.list(...); list.entries.filter(...)`) stays **silent in live VM memory**; ONLY unconsumed result variables (e.g. flat multi-line `const a = f1.slice(...);\nconst b = f2.slice(...);`) and bare query expressions (e.g. `a.slice(...); b.slice(...);` or `[a.slice(...), b.slice(...)]`) are returned automatically in clean unescaped blocks. Avoid `console.log(...)` (it is strictly capped at 4KB).
  - Results up to **`64KB`** per step are returned verbatim and stay in your conversation history for the entire turn — do NOT re-read or re-slice content already shown in an earlier `tool` message. If a batched array/object exceeds `64KB`, the first `64KB` is still printed verbatim and only overflow tail items are summarized.
  - Large files/results (`> 64KB`) stay live in VM memory as `ResourceHandle` (`Ref#N`) with a compact signature + structural/error index in the `tool` output (and any silent new file/command handles loaded in a step are noted in a 1-line `// [live handles added: ...]` footer so you can reuse them without re-reading).
  - Frozen turn snapshots are immutable host assets. Use `session.snapshot({ asset_id })` to restore the exact saved file, command output, or JSON value even after the workspace changes. `session.read_asset(...)` remains available for paged asset delivery.
- **Lazy Resource Handles & Programmatic JS**: `io.read(...)` returns a live `ResourceHandle` (`Ref#N`) with zero-I/O methods: `f.slice(startLine, endLine)` (1-based inclusive, or multi-range `f.slice([[1, 10], [60, 75]])`), `f.grep(pattern, contextLines?)`, `f.outline()`, `f.head(n)`, `f.tail(n)`. Bind batch-read handles by name (e.g. `const [a, b] = await Promise.all([...])` or `const H = Object.fromEntries(paths.map((p, i) => [p, files[i]]))`) so you can reuse `a.slice(...)` / `H[path].grep(...)` across steps without re-reading unchanged files. Use JS loops, regexes, sets, and maps to diff, verify, or extract across multiple files in a single step.

## EDITING AND BUILDING
- When changing multiple lines, deeply indented code, or text/commands with backticks, `${...}`, quotes, or backslashes (Go, Markdown, TS, `sed`/`awk`), prefer Python-style triple-double-quoted strings (`"""..."""` — no escaping needed for backticks/`${...}`/`"`, while standard `\t`/`\n`/`\\` escapes work as in Python) or raw `r"""..."""` (100% verbatim with zero escaping, ideal for `sed`/`awk`/regex/literal `\n`) or multi-line template literals (`` `...` ``) instead of minifying into long single-line `'...\n...'` strings, and keep each `io.apply_edits` batch focused (typically 1–4 related files; `io.edit` applies its `edits` sequentially and atomically). Use `io.replace({ path, start_line, end_line, replacement })` for contiguous block replacements.
- Successful `io.edit` and `io.replace` responses include a line-numbered `preview` of the changed area. Once it confirms the edit, continue without an extra whole-file `io.read`.
- Do not run multiple `cargo` or package-manager build/test commands inside `Promise.all`. Run them serially—prefer combining multiple targeted test filters or `&&`-chained checks into ONE serial command (e.g. `cargo test --bin ... -- test_a test_b && cargo check ...`) instead of spending a separate round-trip per filter. If `process.exec_command` returns `running: true` with a `session_id`, wait for it with `process.write_stdin({ session_id, yield_time_ms: 15000 })` until it exits—never re-launch a duplicate `cargo test` or build command while the first is still running.
- For remote servers, always prefer the built-in `ssh.*` library (`host.contracts({ libraries: ["ssh"] })`: `ssh.list_servers`, `ssh.add_server`, `ssh.exec`, `ssh.list_dir`, `ssh.read_file`, `ssh.tail_log`, `ssh.edit_file`, `ssh.write_file`, `ssh.upload`, `ssh.download`) instead of spawning local `/bin/ssh` or Python `pty` scripts, so credentials stay in host `0600` storage and the SSH Workbench live terminal + SFTP tree follow your actions automatically. When patching an existing remote script or config, use `ssh.edit_file` (`old_text`/`new_text` or `edits`) instead of `ssh.read_file` + chained JS `.replace()` + `ssh.write_file`.
- When creating standalone documents, reports, skills, spreadsheets, or generated assets in a general chat (not bound to a specific code repository), save them with relative paths inside your current workspace (the unified general-chat assets folder) unless the user explicitly specifies another target directory, so clickable links open directly in the built-in Markdown / Excel / Word / PDF plugins.
- For databases (SQLite, MySQL, PostgreSQL), always prefer the built-in `db.*` library (`host.contracts({ libraries: ["db"] })`: `db.list_connections`, `db.save_connection`, `db.inspect_schema`, `db.query`, `db.explain`, `db.execute`, `db.export_xlsx`, `db.import_xlsx`, `db.focus_view`) instead of spawning `mysql`/`psql` CLIs or Python database scripts, so credentials stay in host storage and saved connections appear directly in Database Studio. Treat Database Studio and Excel Studio as live interactive UI workbenches — **prefer UI navigation/locate over destructive SQL filtering whenever UI operations can accomplish the goal**:
  - When the user asks to **open/find a table, browse data, highlight/select a whole column or row, or locate/scroll to a record**, operate the UI directly via `db.focus_view({ connection_id?, schema?, table?, column?, row?, offset?, range?, find_column?, find_value? })` or `excel.focus_view({ path?, sheet?, column?, row?, range?, find_column?, find_value? })` (after `db.inspect_schema` only if the table/column name is unknown). `db.focus_view` expands and selects the table in the left tree, keeps all columns and neighboring rows browsable (even on huge tables, pass `offset` + `find_column`/`find_value` to jump to the target page and scroll to the row rather than filtering away surrounding records), and highlights the target column/row/cell. **Do NOT** run a narrow `SELECT col` or `LIMIT 1` query to replace the full table, and never claim something is highlighted without calling `focus_view`.
  - Use `db.query` / `db.execute` when the user explicitly asks to **filter / only show specific matching data (`WHERE`), aggregate/count/join, compute analytics, or perform writes**. Background analytical/counting/sampling `db.query` calls run silently (`show_in_ui` defaults to `false`) without hijacking the user's open table tab; pass `show_in_ui: true` **only** when the user explicitly wants to view that filtered/sorted query result in the Database Studio `AI 查询` tab (and follow with `db.focus_view` without `table` if they also want a column/row highlighted).
- Use `browser.fill`, `browser.set_viewport`, `browser.scroll`, `browser.wait`, and `browser.screenshot` with a selector for interactive and responsive checks without shell scripts. To start a long-running dev server for browser checks, use `process.run_project_script` (and `process.stop_project_process` when done) rather than backgrounding `cmd &` inside a short-lived `exec_command`, which terminates its process group when the command exits.
- Prefer `verify.check` and `verify.test` for supported project checks; read raw runner output through `session.read_asset` only when the structured diagnostics are insufficient.
- For any non-trivial task with 3+ distinct steps or multiple files to modify, call `plan.set({ steps })` in your first `run_js` step to publish a concise 3–8 step checklist, then call `plan.update({ id, status })` as each step moves to `in_progress`, `done`, or `blocked`. Keep IDs stable; never mark unverified work done. Use `plan.add_substeps({ parent_id, steps })` to split a published step into sub-steps instead of resending the whole plan; a plan has at most two levels, and a parent cannot be `done` while one of its sub-steps is still open unless you pass `complete_children: true`. Before your final reply, close every finished step with `plan.update({ id, status: "done", complete_children: true })` (or mark remaining blockers `"blocked"`).
- After a failed `io.apply_edits` or `io.edit`, check `globalThis.__lastFailedEdit`. Patch only the failed anchor's `old_text` or 1-based `line_hint` in `.args` and retry; do not re-emit unchanged `new_text`.
- When building large code replacements in `run_js`, prefer `io.apply_edits` / `io.replace` directly; if you script custom regex/string edits, bind the large replacement text to top-level `const`/`let` variables **before** running any check or `throw`, because already-executed top-level bindings survive exceptions in this turn's REPL so the next step can reuse them directly (`.replace(...)` / `.slice(...)`) without regenerating the code.
- For a partial batch (`atomic: false`), use the draft's `failed_files` indices and retry only those files.
- Use `git.blame` for line history. `git.commit` and `git.restore` require explicit paths; use them only when the user wants those changes. `io.apply_edits` preflights a multi-file edit before writing by default.
- **UI visual self-review stops after two rounds (and batches screenshots)**: capture multiple views/tabs/themes first, then pass their `artifact_ids` together in ONE `vision.analyze_images` call (typically 2–4 images per call) instead of spending a separate vision round-trip per screenshot. One pass on the first render, then one fix-and-re-verify pass; once nothing is visibly clipped, overflowing, misaligned, or hard to read (contrast/legibility), report the result and finish — do not keep polishing details in a long loop.
- **A local screenshot does not need the browser**: when a test, script or Playwright run already saved a PNG/JPEG/WEBP/GIF inside the workspace or the system temp directory, call `await vision.register_image({ path })` and pass its `artifact_id` to `vision.analyze_images`; reopening it through `browser.open("file://...")` + `browser.screenshot` costs steps and adds the browser's own chrome to the pixels.

### Action 2: Finish turn and reply to the user
- ONLY after you have real observed evidence (never before reading the target files or running requested checks), reply directly in normal Markdown **WITHOUT calling `run_js`**.
- Everything in your plain assistant reply (without `run_js`) is delivered verbatim to the user and NEVER executed as JS (so ```` ```js ```` / ```` ```rust ```` example blocks need zero escaping).
- Write the final answer like a senior engineer giving a direct review in person. Open with 3–5 plain-language sentences that set the result and name the most consequential problems, along with the important chains you confirmed as healthy. For each issue, state the concrete runtime consequence in plain words, such as what will fail, disappear, or become misleading when the code runs. Avoid bureaucratic phrasing and repetitive template sections that mechanically repeat the same headings for every finding.
- Keep information dense. Group repeated findings such as several routing gaps or several type drifts in a compact Markdown table with columns such as `问题 | 位置 | 差异/后果 | 建议`. Quote only the critical 2–5 lines of code when a snippet is needed, and do not reproduce large structs or repeat evidence already stated.
- Concision must remove filler, stock transitions, and duplicated templates only. Preserve every verified conclusion, exact `file:line` coordinate, key evidence comparison, and measured command or test result.

## FINAL REPLY LAYOUT (minimal, Codex-style, easy to scan)
A finished turn is read by a human: keep it short, keep it airy, and let file links carry the detail.
1. **Opening line** — one sentence stating what is now true. No preamble, no restating the request.
2. **`### 已完成`** — an unordered list, one short line per finished result (what changed / what no longer happens). Never paste a long conditional or a multi-line code expression into the prose; name the behaviour and link the file instead.
3. **`### 主要文件`** — one bullet per changed file, spelled as a Markdown file link with the SHORT basename plus the workspace-relative path, optionally a line range: `- [CodeBlock.tsx](frontend/src/components/CodeBlock.tsx)` or `- [ToolRow.tsx](frontend/src/components/ToolRow.tsx#L203-L214)`. Never turn a long path plus a pile of line numbers into a gray backtick inline-code token; those render as clean clickable file links, backtick paths do not.
4. **`### 验证结果`** — 3–4 short bullets naming only the command and its outcome (`npm run typecheck` — 0 errors; `node --test tests/chat-process.test.mjs` — 29 passed; `cargo check --bin …` — 0 warnings). A command the user has to run themselves goes in ONE small fenced block, not inline.
- **Default to NO Markdown table in a completion summary.** A wide table is allowed only when the user explicitly asked for a comparison matrix or a data table, and then it keeps its column count small.
- One line per bullet. If a bullet needs a second paragraph of justification, it belongs in the opening prose, not in the list.
- Do not re-paste code you already wrote to disk; the file link is the reference.
## Parallel Delegation (`host.delegate`) & Concurrent I/O
- **Parallel host I/O, searches, and git**: pre-bound `io.*` / `git.*` are **synchronous** (calling them inside `Promise.all` runs sequentially!). For true concurrency, use the pre-bound **`.promises`** sub-namespace (or `require("<lib>/promises")`):
  - `const files = await Promise.all(paths.map(path => io.promises.read({ path })));`
  - `const [st, lg] = await Promise.all([git.promises.status(), git.promises.log({ limit: 8 })]);`
- **Ordinary JS functions & concurrent `async function` helpers** (`function foo(...)` / `async function checkA(...)`): prefer plain JS functions for deterministic extraction, filtering, cross-checking, counting, or batch transformations, and run multiple `async function` helpers together via `await Promise.all([checkA(), checkB()])` (true concurrent host I/O, compact return values, 0 extra LLM calls).
- **Delegation (`host.delegate`) — AI-decided, single lightweight tasks only**: decide for yourself whether delegating improves efficiency. Delegation is recommended (either concurrently via `Promise.allSettled([...])` / `Promise.all([...])` when independent sub-tasks benefit from parallelism, or as a single focused call) whenever offloading a clean sub-problem saves time or keeps the main frame concise; when target files heavily overlap or batched main-frame JS is faster, handle it directly in the main frame. Keep deterministic filtering, regex extraction, counting, and cross-file correlation in plain parent JS (0 extra LLM calls).
  - **STRICT RULE — single lightweight task only; NEVER delegate complex or heavyweight tasks**: each `host.delegate` call MUST be a **single, narrow, self-contained task** (e.g., comparing one specific pair of declarations and structs, inspecting one bounded file or symbol, or running one targeted verification). **Do NOT delegate broad, complex, multi-module, or open-ended investigations** to a worker: heavyweight workers lose efficiency through duplicated reads, long wandering traces, and high parent re-verification cost.
  - **Delegate context names**: fields in `context` may be used directly by their safe variable name or through `context.<key>` / `context["<key>"]`. If a field collides with a JavaScript keyword or an injected host library such as `host` or `io`, read it through `context` so the field cannot replace the host binding. Safe fields remain available as direct parameters, while the complete context object remains available for uniform access.
  - A delegated worker runs in an independent frame with the parent's permitted non-process capabilities. All `process.*` methods, nested `@virtual`, and nested `host.delegate` are forbidden.
  - Pass pre-loaded `ResourceHandle`s, relevant slices, or confirmed paths in `context` so workers start immediately without re-reading the same files. Workers may still follow direct `include!(...)` splits or references strictly needed for their single task.
  - Ask each worker for **compact, directly checkable output** (exact `file:line` coordinates, concrete diffs/facts, `verifiedClean`, and a short `summary`) rather than long prose essays. When you want to double-check key candidate findings returned by delegates, batch all spot-checks into **one multi-line JS script** (e.g., multiple `.slice(...)` calls together) instead of checking one file per step.
  - **Partial delegation**: wrap deterministic JS pre-reading/filtering in an ordinary `async` function—return directly on the fast path (0 LLM calls), and otherwise `return await host.delegate({ name, task, context: { file, hits } })`. Existing `@virtual` declarations also remain supported.
  - Example:
    ```js
    async function audit(path) {
      const file = await io.promises.read({ path });
      const hits = file.grep("TODO");
      if (hits === "(no matches)") return { path, verifiedClean: [path], summary: "no TODO" };
      return await host.delegate({ name: "audit", task: "Return issues with exact file:line evidence.", context: { file, hits } });
    }
    const [modA, modB, check] = await Promise.all([
      audit("src/a.rs"),
      audit("src/b.rs"),
      git.promises.status(),
    ]);
    ```

## Per-Turn Clean VM & Cross-Turn Continuity
- When a turn finishes, its VM is completely destroyed. Each new user turn starts with a fresh empty VM.
- Large results (`> 64KB`) are automatically archived with an `asset: asset-...` id in the `tool` output; in later turns you can page them back via `session.read_asset({ asset_id, char_offset, max_chars })`.
- Reusable cross-session JS orchestration libraries (listed below the `.d.ts` when available) can be loaded with `const lib = require("<name>");`.
- If you cannot proceed without user clarification, call `host.ask_user({ question: "..." });` inside `run_js` to pause the turn and deliver your question directly.

## Multi-Step Interaction Example
[Step 1 — batch confirmed reads and searches into one multi-line `run_js` call]:
```js
const paths = ["src/auth.rs", "src/session.rs"];
const [authFile, sessionFile] = await Promise.all(
  paths.map(path => io.promises.read({ path }))
);
const authHits = authFile.grep(/verify_token|decode/, 4);
const sessionSlice = sessionFile.slice(1, 60);
```

[Host returns `tool` result: `authFile` / `sessionFile` stay live in VM memory as handles while `authHits` and `sessionSlice` are shown cleanly in numbered lines.]

[Step 2 — batch follow-up slices, edits, and verification into one multi-line `run_js` call]:
```js
const e1 = io.replace({ path: "src/auth.rs", start_line: 61, end_line: 64, replacement: "..." });
const check = process.exec_command({ cmd: "cargo test --bin app -- auth" });
check;
```

[Step 3 — final Markdown reply WITHOUT calling `run_js`]:
Fixed the token verification bypass in `src/auth.rs` lines 61–64 and verified tests pass...
"#;

const VIRTUAL_LEAF_SYSTEM_PROMPT: &str = r#"You are executing inside an isolated single-layer `@virtual` JavaScript function frame.
Complete ONLY this function's contract using its bound parameters, `@uses` captures, and all host libraries declared in the `.d.ts` below. The parent permission and sandbox boundary still apply.
Your ONLY tool is `run_js({ code })`.

## STRICT RULES
1. **Always call `run_js({ code })`** with raw JavaScript statements to continue the function body (NO markdown fences, NO fabricated `// =>` comments). Do NOT reply with plain text outside `run_js`.
2. **Bound parameters, `@uses` captures, and any partial `@virtual` JS-prefix local variables are already live JS variables** — use them directly by identifier name (do NOT re-read or re-run work already computed by the JS prefix).
3. **Host libraries (`io`, `git`, `host`, permitted plugins, and their `.promises` interfaces)** proxy into Rust with the parent’s entry permissions. `process.*` tools are forbidden in this frame. Use structured reads and `.promises` for concurrency. When searching across files, pass a confirmed directory path such as `path: "src"` to `io.search`. Nested `@virtual` declarations and nested `host.delegate` are strictly forbidden.
4. **Stay tightly focused and return compact, directly checkable JSON via native JS `return <json_value>;` inside `run_js`**:
   - **Intermediate exploration is not the final return**: after inspecting with `.slice()`, `.grep()`, `.outline()`, `.head()`, `.tail()`, `io.read()`, or `io.search()`, do not write a top-level `return`. Use a trailing expression such as `({ a, b });` or `[a, b];` to show observations for the next worker step. A top-level `return <final_json>;` terminates this worker immediately and passes the value unchanged to its caller, so use it only after your assigned check is complete.
   - Stay strictly within your single assigned task: return concise, high-signal findings with exact `file:line` coordinates, concrete diffs/symbols, and a short `summary` (plus `verifiedClean` if relevant). Never expand into a broader multi-module investigation or perform redundant repo-wide double-checking—the caller handles cross-module synthesis and any batched JS verification.
   - If the contract cannot be satisfied, execute `return { "$task_error": "concrete reason" };`.
"#;

pub(super) struct ModelReply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    /// Native assistant wire message (`role: "assistant"`, `content`, `tool_calls`,
    /// `reasoning_content`, `_reasoning_items`, `_thinking_blocks`) preserved verbatim
    /// for multi-turn KV-cache prefix stability and encrypted reasoning replay.
    pub assistant_message: Value,
}

impl ModelReply {
    pub(super) fn primary_call_id(&self) -> Option<String> {
        self.tool_calls
            .first()
            .map(|c| c.call_id.clone())
            .filter(|id| !id.is_empty())
            .or_else(|| {
                self.assistant_message["tool_calls"]
                    .as_array()
                    .and_then(|arr| arr.first())
                    .and_then(|c| c["id"].as_str())
                    .map(str::to_owned)
            })
    }
}

fn clean_run_js_code(raw: &str) -> String {
    let trimmed = raw.trim();
    // Gracefully unwrap a single outer ```js ... ``` fence if a model placed fences inside `code`.
    let unwrapped = if trimmed.starts_with("```") {
        if let Some(first_nl) = trimmed.find('\n') {
            let lang = trimmed[3..first_nl].trim().to_ascii_lowercase();
            let body = &trimmed[first_nl + 1..];
            if matches!(lang.as_str(), "" | "js" | "javascript" | "ts" | "typescript")
                && let Some(stripped) = body.strip_suffix("```")
                && !stripped.contains("\n```")
            {
                stripped
                    .strip_suffix("\r\n")
                    .or_else(|| stripped.strip_suffix('\n'))
                    .unwrap_or(stripped)
            } else {
                trimmed
            }
        } else {
            trimmed
        }
    } else {
        trimmed
    };
    let mut s = unwrapped.trim().to_owned();
    for stray in ["</parameter>", "</invoke>", "</tool_call>", "</function>"] {
        if let Some(stripped) = s.strip_suffix(stray) {
            s = stripped.trim_end().to_owned();
        }
    }
    s
}

fn extract_tool_js(reply: &ModelReply) -> RunSubmission {
    if reply.finish_reason.as_deref() == Some("length") {
        return RunSubmission::Rejected {
            code: "OUTPUT_TRUNCATED",
            message: "The `run_js` tool call hit the output token limit. Resubmit a shorter JS step."
                .into(),
        };
    }
    let Some(call) = reply.tool_calls.first() else {
        return RunSubmission::Rejected {
            code: "EMPTY_TOOL_CALL",
            message: "Expected a `run_js({ code })` tool call.".into(),
        };
    };
    if call.name != RUN_JS_TOOL_NAME {
        return RunSubmission::Rejected {
            code: "UNKNOWN_TOOL",
            message: format!(
                "Unknown tool `{}`. The ONLY tool is `run_js({{ code }})` — invoke host libraries (`io.*`, `git.*`, `process.*`, `host.*`, `session.*`) as JS inside `run_js`.",
                call.name
            ),
        };
    }
    let parsed: Value = match serde_json::from_str(&call.arguments) {
        Ok(val) => val,
        Err(err) => {
            return RunSubmission::Rejected {
                code: "INVALID_TOOL_ARGS",
                message: format!(
                    "`run_js` arguments were not valid JSON ({err}). Pass `{{\"code\": \"...\"}}`."
                ),
            };
        }
    };
    let Some(raw_code) = parsed.get("code").and_then(Value::as_str) else {
        return RunSubmission::Rejected {
            code: "MISSING_CODE_FIELD",
            message: "`run_js` requires a string `code` field: `{\"code\": \"...\"}`.".into(),
        };
    };
    let source = clean_run_js_code(raw_code);
    if source.is_empty() {
        return RunSubmission::Rejected {
            code: "EMPTY_CODE",
            message: "`run_js` `code` was empty. Pass JavaScript statements to execute, or reply directly in Markdown without calling `run_js`.".into(),
        };
    }
    RunSubmission::Run { source }
}

/// Parse a completed main-turn model response under the single-shell `run_js` protocol.
/// - Tool call `run_js({ code })` -> `RunSubmission::Run { source }` (isolating any commentary in `reply.content`).
/// - Plain assistant text without `run_js` -> `RunSubmission::Reply { markdown }` (also strips legacy `return <<<` header if present).
pub(super) fn parse_main_submission(reply: &ModelReply) -> RunSubmission {
    if !reply.tool_calls.is_empty() {
        return extract_tool_js(reply);
    }
    if reply.finish_reason.as_deref() == Some("length") {
        return RunSubmission::Rejected {
            code: "REPLY_TRUNCATED",
            message: "The final reply hit the output token limit. Please provide a concise final Markdown reply.".into(),
        };
    }
    let trimmed = reply.content.trim();
    if trimmed.is_empty() {
        return RunSubmission::Rejected {
            code: "EMPTY_RESPONSE",
            message: "Empty response. Call `run_js({ code })` to execute JavaScript statements, or reply directly in Markdown to finish the turn.".into(),
        };
    }
    // Graceful compatibility if a model still uses `return <<<` in plain text.
    if trimmed.starts_with("return") {
        if let RunSubmission::Reply { markdown } =
            crate::model::model_stream::parse_run_submission(&reply.content, reply.finish_reason.as_deref())
        {
            return RunSubmission::Reply { markdown };
        }
    }
    RunSubmission::Reply {
        markdown: trimmed.to_owned(),
    }
}

/// Parse a completed `@virtual` leaf model response under the single-shell `run_js` protocol.
pub(super) fn parse_virtual_submission(reply: &ModelReply) -> RunSubmission {
    if !reply.tool_calls.is_empty() {
        return extract_tool_js(reply);
    }
    RunSubmission::Rejected {
        code: "VIRTUAL_TOOL_REQUIRED",
        message: "Inside a `@virtual` function, call `run_js({ code })` with JavaScript statements and finish with native JS `return { ..., summary: \"...\" };`. Do not reply with plain text outside `run_js`.".into(),
    }
}

/// Compact system prompt for plugin sessions running in `"slim"` capability profile:
/// only the minimal `run_js({ code })` execution protocol, core `host`/`session` helpers,
/// and the plugin's own tools + inlined `SKILL.md` — no coding/shell/git/ssh/browser bloat.
const SLIM_SYSTEM_PROMPT: &str = r#"You are a focused plugin assistant operating inside a per-turn JavaScript VM.
Your ONLY tool is `run_js({ code })`, which executes JavaScript statements against the pre-bound plugin and core libraries declared in the `.d.ts` below and returns the real output in the `tool` response.

## EXECUTION PROTOCOL
Each response MUST be **one** of two actions:
1. **Call `run_js({ code })`**:
   - Emit raw, readable multi-line JavaScript statements (no markdown fences, no fabricated comments).
   - Batch independent queries into one script (use `.promises` + `Promise.all` for concurrent calls) instead of one call per turn.
   - Top-level `const`/`let`/`function` bindings stay live across steps in the current turn; unconsumed result variables and bare expressions are returned automatically (avoid `console.log`).
   - If you need the full JSON input schema of any plugin method, call `host.contracts({ libraries: [...] })`. For oversized results stored as host assets, use `session.read_asset(...)` / `session.snapshot(...)`.
2. **Reply directly to the user (without calling `run_js`)**:
   - Once you have real observed tool output, answer directly in clear, natural Markdown grounded strictly in the returned data. Never fabricate tool results or citations."#;

/// Build the provider wire body: static system prompt + `.d.ts` declarations +
/// conversation turns + ONLY the single fixed `run_js({ code })` shell tool.
pub(super) fn build_request_body(
    s: &Session<'_>,
    w: &Window,
    libs: &Libraries,
) -> Value {
    let base = w.pinned_system_prompt.as_deref().unwrap_or(V2_SYSTEM_PROMPT);
    let is_slim = s.plugin.is_some()
        && s.prompt_profile() == crate::server::plugin_protocol::PromptProfile::Slim;
    let ephemeral_tail = if s.scope.starts_with("virtual:") || is_slim {
        None
    } else {
        let live_plan = crate::tools::plan_tools::format_turn_plan_for_context(
            s.state.workspace.root(),
            s.task,
            s.turn,
        );
        let blueprint_tail = workspace_blueprint_tail(s, libs);
        match (blueprint_tail, live_plan) {
            (Some(bp), Some(plan)) => Some(format!("{bp}\n\n{plan}")),
            (Some(bp), None) => Some(bp),
            (None, Some(plan)) => Some(plan),
            (None, None) => None,
        }
    };
    build_request_body_for_doc_with_plan(s, &w.context_doc, base, libs, ephemeral_tail.as_deref())
}

pub(super) fn pinned_system_prompt(s: &Session<'_>, libs: &Libraries) -> String {
    if let Some(plugin_id) = s.plugin.as_deref()
        && s.prompt_profile() == crate::server::plugin_protocol::PromptProfile::Slim
    {
        let mut text = String::from(SLIM_SYSTEM_PROMPT);
        if let Some(skill_body) = s
            .state
            .tool_catalog
            .as_ref()
            .and_then(|catalog| catalog.packages.inline_skill_body(plugin_id))
        {
            text.push_str("\n\n=== PLUGIN INSTRUCTIONS (SKILL.md) ===\n");
            text.push_str(&skill_body);
            text.push('\n');
        }
        text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
        text.push_str(&declarations::static_library_header(libs));
        return text;
    }
    let watched_task = if s.scope.starts_with("virtual:") {
        None
    } else {
        crate::context::session_browser::observer_watched_task(s.state.workspace.root(), s.task)
            .ok()
            .flatten()
    };
    let mut text = String::from(V2_SYSTEM_PROMPT);
    if let Some(target_task_id) = watched_task.as_deref() {
        text.push_str(&format!(
            "\n\n=== OBSERVER AGENT ROLE ===\n\
你是专属于主会话 `{target_task_id}` 的只读观察者 Agent（严格只读，禁止修改任何文件或执行写操作）。\n\
- 收到巡检信号或用户提问时，优先从尾部查看主会话最近在干什么：先调用 `blueprint.inspect_session({{ task_id: \"{target_task_id}\", max_recent: 20 }})` 查看最新多轮 `user_instructions`、运行中工具 `in_flight_tools`（含实时终端 `tail_lines`）和最新 20 条事件 `recent_tail`。\n\
- 需要更多前因后果或细节时，可自由翻阅 SQLite 历史（`session.read_session_history`）或使用只读文件/搜索/Git 工具核对代码真相。\n\
- 判断主会话目标与授权边界时，必须按时间顺序串联全部 `user_instructions`：后续轮次覆盖早先冲突限制；简短续跑指令（如“继续”“再连一下”）延续最近一条实质任务与授权，绝不回退套用已被覆盖的首轮旧限制。\n\
- 工具偶尔报错但主会话自行修正的属于正常执行波动，无需小题大做；仅在发现明确跑偏、长时间卡死或需止损时建议介入。\n\
- 每次回复末尾附如下 JSON 代码块供界面渲染确认卡片：\n\
```observer-suggestion\n\
{{\"should_interject\": false, \"action\": \"interject\", \"severity\": \"tip\", \"title\": \"\", \"reason\": \"当前进展正常\", \"interject\": \"\"}}\n\
```\n\
（`action` 可选 `\"interject\"` | `\"stop\"` | `\"stop_and_continue\"`；`severity` 可选 `\"tip\"` | `\"warn\"` | `\"blocker\"`；`interject` 为用户确认后发送给主会话的精炼指令。）\n\
- **战地蓝图呈报（可选，有实质沉淀时才附）**：当主会话完成一轮实质改动、踩过值得记录的真实环境/架构坑、或用户要求呈报蓝图时，额外在末尾附一个 `blueprint-note` JSON 代码块，系统会自动呈报到当前项目的「架构蓝图 → 观察者呈报」收件箱供蓝图页审阅合入；普通进行中巡检若无新沉淀则不要附此块：\n\
```blueprint-note\n\
{{\"title\": \"简明主题\", \"trigger\": \"何时需要参考这条经验\", \"summary\": \"战地结论与架构/流程要点\", \"anchor_files\": [\"相对路径\"], \"environment\": [\"端口/路径/服务隔离约定\"], \"pitfalls\": [\"踩过的坑与红线\"], \"verify_commands\": [\"有效验收命令\"]}}\n\
```\n"
        ));
    }
    text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
    if watched_task.is_some() {
        text.push_str(&declarations::static_library_header(libs));
    } else {
        text.push_str(&model_library_header(libs));
    }
    if !s.scope.starts_with("virtual:") && watched_task.is_none() {
        let orch = crate::runtime::flow_libraries::list_libraries(s);
        if !orch.is_empty() {
            text.push_str("\n// Reusable JS orchestration libraries via `require(\"<name>\")`:\n");
            for (name, desc) in orch {
                let methods = crate::runtime::flow_libraries::public_method_names(s, &name).unwrap_or_default();
                text.push_str(&format!("// - require({name:?}) -> {{ {} }}: {desc}\n", methods.join(", ")));
            }
        }
    }
    text
}

// Keep the pinned header stable and give only opted-in workspaces a blueprint hint.
fn model_library_header(libs: &Libraries) -> String {
    if !libs.contains_key("blueprint") { return declarations::static_library_header(libs); }
    let mut listed = libs.clone();
    listed.remove("blueprint");
    declarations::static_library_header(&listed)
}

fn task_blueprint_root(s: &Session<'_>, libs: &Libraries) -> Option<std::path::PathBuf> {
    if !libs.contains_key("blueprint") {
        return None;
    }
    let base = s.state.workspace.root();
    match crate::server::plugin_sessions::owner(base, s.task).ok()? {
        Some(plugin) => crate::server::plugin_sessions::workspace_dir(base, &plugin).ok(),
        None => crate::context::session_browser::task_workspace(base, s.task).ok(),
    }
}

pub(super) fn workspace_blueprint_hint(s: &Session<'_>, libs: &Libraries) -> Option<String> {
    let root = task_blueprint_root(s, libs)?;
    crate::server::builtin_plugins::blueprint::workspace_blueprint_hint(&root)
}

pub(super) fn workspace_blueprint_tail(s: &Session<'_>, libs: &Libraries) -> Option<String> {
    let root = task_blueprint_root(s, libs)?;
    crate::server::builtin_plugins::blueprint::workspace_blueprint_tail(&root)
}

pub(super) fn chat_assets_hint(s: &Session<'_>) -> Option<String> {
    if s.turn != 1 {
        return None;
    }
    let base = s.state.workspace.root();
    if crate::server::plugin_sessions::owner(base, s.task).ok()?.is_some() {
        return None;
    }
    if !crate::context::session_browser::is_general_chat(base, s.task).ok()? {
        return None;
    }
    let dir = crate::context::session_browser::task_workspace(base, s.task).ok()?;
    Some(format!(
        "[General Chat Assets Directory: {} — save generated documents, reports, spreadsheets, and Markdown files as relative paths under this folder unless the user explicitly specifies another directory.]",
        dir.display()
    ))
}

const RUNTIME_IDENTITY_MARKER: &str = "/* === RUNTIME IDENTITY === */";
const RUNTIME_MODEL_PREFIX: &str = "Current runtime model: ";
const REPORT_MODEL_PREFIX: &str = "When asked which model you are, report this exact runtime model identifier";

fn without_runtime_identity(base: &str) -> String {
    let mut lines = base.split_inclusive('\n').peekable();
    let mut text = String::with_capacity(base.len());
    while let Some(line) = lines.next() {
        if line.trim_end() == RUNTIME_IDENTITY_MARKER
            && lines.peek().is_some_and(|next| next.starts_with(RUNTIME_MODEL_PREFIX))
        {
            lines.next();
            if lines.peek().is_some_and(|next| next.starts_with(REPORT_MODEL_PREFIX)) {
                lines.next();
            }
        } else {
            text.push_str(line);
        }
    }
    text
}

pub(super) fn build_request_body_for_doc(
    s: &Session<'_>,
    doc: &super::context_doc::ContextDocument,
    base_prompt: &str,
    libs: &Libraries,
) -> Value {
    build_request_body_for_doc_with_plan(s, doc, base_prompt, libs, None)
}

fn append_ephemeral_plan_tail(
    messages: &mut Vec<Value>,
    plan_block: &str,
    is_responses: bool,
    is_messages: bool,
) {
    if is_responses {
        // Non-first `system` messages translate to trailing `developer` input in
        // `responses_request`, keeping the entire history prefix 100% cacheable.
        messages.push(json!({"role": "system", "content": plan_block}));
    } else if is_messages {
        // Anthropic Messages merges a trailing `user` message into the preceding
        // `tool_result` user turn as a trailing text block, preserving prefix cache.
        messages.push(json!({"role": "user", "content": plan_block}));
    } else if let Some(last) = messages.last_mut()
        && matches!(last["role"].as_str(), Some("tool" | "user"))
        && let Some(content) = last["content"].as_str()
    {
        // OpenAI-compatible Chat Completions: append cleanly to the tail tool/user
        // content without introducing a non-standard `system` after `tool` or a
        // synthetic `user` turn mid-tool-chain.
        last["content"] = json!(format!("{content}\n\n{plan_block}"));
    } else {
        messages.push(json!({"role": "user", "content": plan_block}));
    }
}

fn build_request_body_for_doc_with_plan(
    s: &Session<'_>,
    doc: &super::context_doc::ContextDocument,
    base_prompt: &str,
    libs: &Libraries,
    live_plan: Option<&str>,
) -> Value {
    let mut system_text = without_runtime_identity(base_prompt);
    let has_pinned_sdk = system_text.contains("/* === RUNTIME HOST SDK (.d.ts) === */");
    if !has_pinned_sdk {
        system_text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
        system_text.push_str(&model_library_header(libs));
    }
    if !has_pinned_sdk && !s.scope.starts_with("virtual:") {
        let orch = crate::runtime::flow_libraries::list_libraries(s);
        if !orch.is_empty() {
            system_text.push_str("\n// Reusable JS orchestration libraries via `require(\"<name>\")`:\n");
            for (name, desc) in orch {
                let methods = crate::runtime::flow_libraries::public_method_names(s, &name).unwrap_or_default();
                system_text.push_str(&format!(
                    "// - require({name:?}) -> {{ {} }}: {desc}\n",
                    methods.join(", ")
                ));
            }
        }
    }

    system_text.truncate(system_text.trim_end().len());
    let model = s.model;
    let provider = &s.state.provider_name;
    system_text.push_str(&format!(
        "\n\n{RUNTIME_IDENTITY_MARKER}\n{RUNTIME_MODEL_PREFIX}{model} (provider: {provider})\n\
         {REPORT_MODEL_PREFIX} (`{model}`) accurately; never guess from pretraining data or echo an earlier turn's different model."
    ));

    let is_responses = s
        .state
        .provider_routes
        .get(&s.state.provider_name)
        .is_some_and(|route| route.responses);
    let is_messages = s
        .state
        .provider_routes
        .get(&s.state.provider_name)
        .is_some_and(|route| route.messages);

    let mut messages = vec![json!({"role": "system", "content": system_text})];
    messages.extend(doc.render_conversation_messages());
    if let Some(plan_block) = live_plan {
        append_ephemeral_plan_tail(&mut messages, plan_block, is_responses, is_messages);
    }

    let mut body = json!({
        "model": s.model,
        "messages": messages,
        "tools": [run_js_tool_definition()],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "stream": true,
    });
    let estimated_input = crate::context::context_window::estimate(&body);
    body["max_tokens"] = json!(crate::model::model_settings::output_tokens(
        s.state,
        s.model,
        s.input_budget,
        estimated_input
    ));
    if let Some(effort) = &s.state.reasoning_effort {
        let mapped = match effort.as_str() {
            "minimal" => "low",
            "off" => "none",
            other => other,
        };
        body["reasoning_effort"] = json!(mapped);
    }

    if !is_messages {
        body["prompt_cache_key"] = json!(crate::model::model_transport::affinity(s.task, &s.scope));
    }
    if is_responses {
        body = crate::model::model_stream::responses_request(&body);
    } else if is_messages {
        body = crate::model::anthropic_messages::wire_body(s.state, s.model, &body);
    } else if let Some(msgs) = body["messages"].as_array_mut() {
        // Plain OpenAI-compatible Chat Completions: strip internal-only
        // Responses/Anthropic reasoning metadata and non-standard `reasoning_content`
        // heading summaries so upstream Chat Completions routes stay strictly spec-clean.
        for msg in msgs {
            if let Some(obj) = msg.as_object_mut() {
                obj.remove(crate::model::model_stream::REASONING_ITEMS);
                obj.remove(crate::model::model_stream::THINKING_BLOCKS);
                obj.remove("reasoning_content");
            }
        }
    }
    body
}

pub(super) async fn call_model(
    s: &Session<'_>,
    w: &mut Window,
    libs: &Libraries,
    step: usize,
) -> Result<ModelReply> {
    let body = build_request_body(s, w, libs);
    let node = format!("step_{}_{}", s.turn, step);
    send_model_request(s, w.task_node_id.as_deref(), body, &node, step, "agent", Some(&mut w.token_calibration)).await
}

pub(super) async fn call_virtual_model(
    s: &Session<'_>,
    doc: &super::context_doc::ContextDocument,
    leaf_libs: &Libraries,
    parent_step: usize,
    sub_step: usize,
    action_id: &str,
) -> Result<ModelReply> {
    let body = build_request_body_for_doc(s, doc, VIRTUAL_LEAF_SYSTEM_PROMPT, leaf_libs);
    let node = format!("virtual_{action_id}_{parent_step}_{sub_step}");
    send_model_request(s, s.task_node_id.as_deref(), body, &node, sub_step, "virtual", None).await
}

async fn send_model_request(
    s: &Session<'_>,
    frame: Option<&str>,
    body: Value,
    node: &str,
    step: usize,
    actor: &str,
    mut calibration: Option<&mut crate::context::context_window::TokenCalibration>,
) -> Result<ModelReply> {
    let trace = crate::context::request_context::record(
        s.state.workspace.root(),
        s.task,
        json!({
            "actor": actor,
            "stage": "execution",
            "turn": s.turn,
            "step": step,
            "nodeId": node,
            "request_id": s.turn,
            "context_scope": s.scope,
        }),
        &body,
    )
    .await
    .context("Cannot persist model input; request was not sent")?;

    if actor == "agent" {
        let estimated_input = crate::context::context_window::estimate(&body);
        let _ = super::append_event(
            s,
            "context/usage",
            json!({
                "actor": actor,
                "turn": s.turn,
                "step": step,
                "estimated_input_tokens": estimated_input,
                "input_budget": s.input_budget,
            }),
            false,
        )
        .await;
    }

    let mut attempt = 1u32;
    loop {
        let diagnostics = crate::model::model_transport::ModelRequestLog::for_agent(
            s.state,
            json!({
                "request_id": node,
                "context_id": trace.id(),
                "task_id": s.task,
                "turn": s.turn,
                "step": step,
                "actor": actor,
                "provider": s.state.provider_name,
                "model": s.model,
                "attempt": attempt,
            }),
            &body,
        ).await;
        let _permit = tokio::select! {
            _ = s.cancel.cancelled() => anyhow::bail!("model request cancelled while waiting for slot"),
            permit = s.state.model_request_slots.acquire() => permit.context("model request queue closed")?,
        };
        diagnostics.sending();
        let attempt_res = async {
            let request = crate::model::anthropic_messages::send(s.state, s.task, &s.scope, &body)
                .header("x-client-request-id", node)
                .header(
                    "x-agent-affinity",
                    crate::model::model_transport::affinity(s.task, &s.scope),
                );
            let response = tokio::select! {
                biased;
                _ = s.cancel.cancelled() => anyhow::bail!("cancelled"),
                result = request.send() => result?,
            };
            diagnostics.headers(&response);
            let response = tokio::select! {
                biased;
                _ = s.cancel.cancelled() => anyhow::bail!("cancelled"),
                result = crate::media::visual_probe::successful_response(response) => result?,
            };
            let (message, stats) = agent_service::consume_model_stream_observed(
                response,
                s.state.workspace.root(),
                s.task,
                s.turn,
                step,
                frame,
                s.cancel,
                Some(&diagnostics),
            )
            .await?;
            diagnostics.phase("response_validation");
            ensure!(
                stats["terminated"] == true,
                "model stream ended without completion"
            );
            ensure!(
                stats["finish_reason"] != "content_filter",
                "model response was filtered; no code was executed"
            );
            Ok::<(Value, Value), anyhow::Error>((message, stats))
        }
        .await;

        match attempt_res {
            Ok((message, stats)) => {
                let usage = &stats["usage"];
                let counts = crate::model::model_transport::extract_usage_counts(usage);
                let cost = usage.as_object().filter(|fields| !fields.is_empty()).and_then(|_| {
                    crate::model::model_settings::request_cost(s.state, &s.model, counts, chrono::Utc::now())
                });
                let mut telemetry = counts.telemetry(cost);
                telemetry["provider"] = json!(s.state.provider_name);
                telemetry["model"] = json!(s.model);
                diagnostics.completed(telemetry.clone());
                if counts.total_input > 0 {
                    let estimated = crate::context::context_window::estimate(&body);
                    let route = format!("{}/{}", s.state.provider_name, s.model);
                    if let Some(calibration) = calibration.as_deref_mut() {
                        calibration.observe(&route, estimated, counts.total_input as usize);
                    }
                }
                if usage.as_object().is_some_and(|fields| !fields.is_empty()) {
                    telemetry["actor"] = json!(actor);
                    telemetry["turn"] = json!(s.turn);
                    telemetry["step"] = json!(step);
                    telemetry["input_budget"] = json!(s.input_budget);
                    telemetry["source"] = json!("provider_usage");
                    let _ = super::append_event(s, "context/usage", telemetry, false).await;
                }
                let content = message["content"].as_str().unwrap_or("").to_owned();
                let tool_calls = crate::model::model_stream::tool_calls(&message);
                let finish_reason = stats["finish_reason"].as_str().map(str::to_owned);
                return Ok(ModelReply {
                    content,
                    tool_calls,
                    finish_reason,
                    assistant_message: message,
                });
            }
            Err(error) => {
                let retry_after_output =
                    diagnostics.output_started() && retryable_stream_transport_error(&error);
                if attempt >= 3
                    || s.cancel.is_cancelled()
                    || (diagnostics.output_started() && !retry_after_output)
                {
                    return Err(error);
                }
                if retry_after_output {
                    super::append_event(
                        s,
                        "assistant/delta",
                        json!({
                            "turn": s.turn,
                            "step": step,
                            "task_node_id": frame,
                            "text": "",
                            "reasoning": "",
                            "tool_progress": [],
                            "reset": true,
                        }),
                        false,
                    )
                    .await?;
                }
                let delay = diagnostics
                    .retry_after()
                    .unwrap_or_else(|| crate::model::model_transport::backoff(attempt));
                attempt += 1;
                tokio::select! {
                    _ = tokio::time::sleep(delay.min(Duration::from_secs(10))) => {}
                    _ = s.cancel.cancelled() => anyhow::bail!("cancelled"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_identity_tracks_model_switch_without_changing_same_model_system_bytes() {
        let root = std::env::temp_dir().join(format!("runtime-identity-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.provider_name = "deepseek".into();
        let mut new_state = state.clone();
        new_state.provider_name = "openai".into();
        let cancel = tokio_util::sync::CancellationToken::new();
        let old = Session {
            state: &state, task: "task", model: "deepseek-flash", turn: 1, cancel: &cancel,
            scope: "main".into(), parent: None, agent_id: None, task_node_id: None,
            plugin: None, ban_execution: false, input_budget: 100_000,
        };
        let libs = crate::runtime::agent_program::libraries::builtin();
        let pinned = pinned_system_prompt(&old, &libs);
        assert!(!pinned.contains(RUNTIME_IDENTITY_MARKER));
        let mut w = Window { pinned_system_prompt: Some(pinned.clone()), ..Default::default() };
        let old_body = build_request_body(&old, &w, &libs);
        let old_system = old_body["messages"][0]["content"].as_str().unwrap();
        assert!(old_system.contains("Current runtime model: deepseek-flash (provider: deepseek)"));

        let new = Session { state: &new_state, model: "gpt-6.2-sol", turn: 2, ..old };
        let switched = build_request_body(&new, &w, &libs);
        let system = switched["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("Current runtime model: gpt-6.2-sol (provider: openai)"));
        assert!(!system.contains("deepseek-flash"));
        assert_eq!(system.matches(RUNTIME_IDENTITY_MARKER).count(), 1);

        // Simulate restoring a legacy window that pinned the full old system message.
        w.pinned_system_prompt = Some(old_system.into());
        let reused = build_request_body(&new, &w, &libs);
        assert_eq!(system.as_bytes(), reused["messages"][0]["content"].as_str().unwrap().as_bytes());
        let repeated = build_request_body(&new, &w, &libs);
        assert_eq!(system.as_bytes(), repeated["messages"][0]["content"].as_str().unwrap().as_bytes());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ui_self_review_is_bounded_and_local_images_register_directly() {
        assert!(V2_SYSTEM_PROMPT.contains("UI visual self-review stops after two rounds"));
        assert!(V2_SYSTEM_PROMPT.contains("vision.register_image({ path })"));
        assert!(V2_SYSTEM_PROMPT.contains("io.delete"));
        assert!(V2_SYSTEM_PROMPT.contains("Reserve `process.exec_command` for project builds, tests, and CLI diagnostics"));
        assert!(V2_SYSTEM_PROMPT.contains("do not keep polishing details in a long loop"));
    }

    #[test]
    fn batch_reads_must_confirm_paths_before_parallel_reads() {
        assert!(V2_SYSTEM_PROMPT.contains("Never guess paths"));
        assert!(V2_SYSTEM_PROMPT.contains("const entries = io.list(...).entries"));
        assert!(V2_SYSTEM_PROMPT.contains(
            "Promise.all(confirmed.map(path => io.promises.read({ path })))"
        ));
        assert!(!V2_SYSTEM_PROMPT.contains("wrap uncertain paths in"));
    }

    #[test]
    fn final_reply_layout_is_sectioned_and_forbids_wide_tables() {
        assert!(V2_SYSTEM_PROMPT.contains("FINAL REPLY LAYOUT"));
        assert!(V2_SYSTEM_PROMPT.contains("### 已完成"));
        assert!(V2_SYSTEM_PROMPT.contains("### 主要文件"));
        assert!(V2_SYSTEM_PROMPT.contains("### 验证结果"));
        assert!(V2_SYSTEM_PROMPT.contains("[CodeBlock.tsx](frontend/src/components/CodeBlock.tsx)"));
        assert!(V2_SYSTEM_PROMPT.contains("ToolRow.tsx#L203-L214"));
        assert!(V2_SYSTEM_PROMPT.contains("Default to NO Markdown table"));
        assert!(V2_SYSTEM_PROMPT.contains("explicitly asked for a comparison matrix"));
        assert!(V2_SYSTEM_PROMPT.contains("gray backtick inline-code token"));
    }

    #[test]
    fn ephemeral_plan_tail_preserves_prefix_across_all_wire_formats() {
        let base = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "run_js", "arguments": "{\"code\":\"1\"}"}}]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "1"}),
        ];
        let plan = "[Current Task Plan — Turn 1 (1/2 done)]\n- [done] a: A\n- [in_progress] b: B";

        // 1. Responses API: trailing system -> developer message, history prefix untouched
        let mut resp_msgs = base.clone();
        append_ephemeral_plan_tail(&mut resp_msgs, plan, true, false);
        assert_eq!(resp_msgs[..4], base[..4]);
        let resp_wire = crate::model::model_stream::responses_request(&json!({
            "model": "gpt-test",
            "messages": resp_msgs,
            "tools": [],
        }));
        let last_input = resp_wire["input"].as_array().unwrap().last().unwrap();
        assert_eq!(last_input["role"], "developer");
        assert!(last_input["content"][0]["text"].as_str().unwrap().contains("[Current Task Plan"));

        // 2. Anthropic Messages API: trailing user merges after tool_result, system untouched
        let mut msg_msgs = base.clone();
        append_ephemeral_plan_tail(&mut msg_msgs, plan, false, true);
        assert_eq!(msg_msgs[..4], base[..4]);
        assert_eq!(msg_msgs[4]["role"], "user");
        assert_eq!(msg_msgs[4]["content"], plan);

        // 3. Chat Completions: appends into trailing tool content cleanly
        let mut chat_msgs = base.clone();
        append_ephemeral_plan_tail(&mut chat_msgs, plan, false, false);
        assert_eq!(chat_msgs.len(), 4);
        assert_eq!(chat_msgs[..3], base[..3]);
        assert_eq!(chat_msgs[3]["role"], "tool");
        assert!(chat_msgs[3]["content"].as_str().unwrap().ends_with(plan));
    }

    #[test]
    fn slim_plugin_profile_uses_compact_prompt_minimal_libs_and_inlines_skill() {
        let root =
            std::env::temp_dir().join(format!("slim-profile-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let (runtime, catalog) = crate::server::skill_packages::test_runtime();
        let mut state = agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.tool_catalog = Some(catalog);

        // Seed two plugin-owned tasks in SQLite: one `"slim"`, one `"full"`.
        let conn = agent_service::open_db(&root).unwrap();
        for (task_id, profile) in [("task-slim", "slim"), ("task-full", "full")] {
            conn.execute(
                "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'','m','draft',1,1)",
                [task_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO plugin_sessions(task_id,plugin_id) VALUES (?1,'blueprint')",
                [task_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO plugin_session_profiles(task_id,prompt_profile) VALUES (?1,?2)",
                [task_id, profile],
            )
            .unwrap();
        }
        drop(conn);

        let cancel = tokio_util::sync::CancellationToken::new();
        let window = Window::default();

        let slim_session = Session {
            state: &state,
            task: "task-slim",
            model: "m",
            turn: 1,
            cancel: &cancel,
            scope: "main".into(),
            parent: None,
            agent_id: None,
            task_node_id: None,
            plugin: Some("blueprint".into()),
            ban_execution: false,
            input_budget: 100_000,
        };
        let full_session = Session {
            task: "task-full",
            ..slim_session.clone()
        };

        let slim_libs = super::super::tool_executor::library_compile_scope(&slim_session, &window);
        let full_libs = super::super::tool_executor::library_compile_scope(&full_session, &window);

        // Slim exposes only minimal `host`, `session`, and the plugin's own library (`blueprint`).
        assert!(slim_libs.contains_key("host"));
        assert!(slim_libs.contains_key("session"));
        assert!(slim_libs.contains_key("blueprint"));
        for excluded in ["io", "git", "process", "vision", "attachments", "browser", "plan", "sys", "http"] {
            assert!(
                !slim_libs.contains_key(excluded),
                "slim profile should exclude `{excluded}`"
            );
        }
        // Full plugin session preserves vision / attachments as before.
        assert!(full_libs.contains_key("vision"));
        assert!(full_libs.contains_key("attachments"));

        let slim_prompt = pinned_system_prompt(&slim_session, &slim_libs);
        let full_prompt = pinned_system_prompt(&full_session, &full_libs);
        assert!(slim_prompt.starts_with(SLIM_SYSTEM_PROMPT));
        assert!(slim_prompt.contains("=== PLUGIN INSTRUCTIONS (SKILL.md) ==="));
        assert!(!slim_prompt.contains("io.apply_edits"));
        assert!(!slim_prompt.contains("calling io.workspace_info()"));
        assert!(
            slim_prompt.len() < full_prompt.len(),
            "slim prompt ({}) should be shorter than full prompt ({})",
            slim_prompt.len(),
            full_prompt.len()
        );
        assert!(full_prompt.starts_with(V2_SYSTEM_PROMPT));

        // Prompt prefix bytes stay identical across turns for KV cache hit stability.
        let w = Window {
            pinned_system_prompt: Some(slim_prompt),
            ..Default::default()
        };
        let b1 = build_request_body(&slim_session, &w, &slim_libs);
        let slim_turn2 = Session { turn: 2, ..slim_session };
        let b2 = build_request_body(&slim_turn2, &w, &slim_libs);
        assert_eq!(
            b1["messages"][0]["content"].as_str().unwrap().as_bytes(),
            b2["messages"][0]["content"].as_str().unwrap().as_bytes()
        );

        runtime.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
