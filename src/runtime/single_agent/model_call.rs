//! Single-shell `run_js` HTTP model caller for Context-as-Code v2.
//!
//! Sends:
//! - Static `system` prompt (incremental JS execution via the single `run_js({ code })`
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
                        "description": "Raw JavaScript statements to append and execute in the live VM (no markdown code fences, no fabricated `// =>` comments)."
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

## STRICT SINGLE-STEP PROTOCOL
Each response MUST be **one** of the two actions below:

### Action 1: Execute the next JS statement(s) via `run_js({ code })`
- Call `run_js` with raw JavaScript statements to execute inside the live per-turn VM (NO markdown fences, NO fabricated `// =>` results).
- You may optionally write a brief 1-sentence progress note in your assistant message text before calling `run_js`, and keep `code` 100% pure JavaScript.
- Emit ONE `run_js` call per step so the host executes it and returns the real output in the `tool` message before your next step.
- **Batch into readable multi-line JS scripts — avoid 1-line baby steps**: JS runs in milliseconds while each model round-trip takes seconds. Avoid dripping out 1 file read or 1 `.slice()`/`.grep()` per step; once you know 2+ target paths, handles, or edits, combine them into one cohesive **multi-line** script (one statement per line with real `\n`, using `Promise.all` over `.promises` for concurrent reads/searches and flat `const` sink bindings rather than cramming deeply nested `({ a: {...}, b: {...} })` onto one minified line).
- **Batch across the entire turn, including final verification**: before issuing any `run_js` call—especially when checking remaining details or running final checks before replying—group all independent reads, slices, searches, or verifications you still need into one multi-line JS script (using `.promises` + `Promise.all` for concurrent I/O) instead of dripping them out as a chain of single-line follow-up calls.
- **Prefer host library methods over shelling out & confirm paths**:
  - Default to structured host libraries (`io.list`, `io.read`, `io.search`, `io.edit`, `io.write`, `io.replace`, `io.delete`, `io.rename`, `io.mkdir`, `git.*`, and `ResourceHandle` methods `f.slice` / `f.grep` / `f.outline`) for all file, text-search, and git work rather than shelling out to `rg`, `grep`, `find`, `sed`, `cat`, `rm`, `mv`, or `mkdir` (avoids quoting bugs and missing CLI binaries, and keeps live `ResourceHandle`s in memory). Reserve `process.*` (`process.run_readonly`, `process.exec_command`) for what has no host library equivalent: running a project's own builder or test runner (cargo, npm, pytest, go test) and similar CLI diagnostics.
  - For scanning large files, prefer native Rust methods (`f.grep(regex, context)`, `f.content.includes(...)`, `io.search(...)`) over nested JS line-by-line loops.
  - Before batch-reading files, confirm every path with `io.list`/`io.search`. Never guess paths and use `.catch()` or `Promise.allSettled(...)` as path-discovery fallbacks. If the directory or symbol location is unknown, first do a synchronous `const entries = io.list(...).entries`, filter entries to confirmed paths, then pass only those paths to `Promise.all(confirmed.map(path => io.promises.read({ path })))`. Reserve `Promise.allSettled(...)` for already-confirmed independent operations whose failure is semantically expected.
- **Encapsulate cohesive multi-step logic in JS functions and run them together**: when a step combines multiple reads, extractions, or cross-checks, encapsulating each unit in a plain `function` or `async function` keeps temporary variables scoped cleanly (avoiding top-level name collisions), compresses intermediate data locally before returning only the compact structured result, and leaves the helper reusable in later steps. Multiple `async function` helpers can run concurrently in one `await Promise.all([...])` / `Promise.allSettled([...])`—achieving true parallel host I/O with zero extra LLM calls.
- Host libraries (`io`, `git`, `process`, `host`, `session`, etc.) declared in the `.d.ts` below are pre-bound in scope and proxy synchronously into Rust.
- All top-level variables (`const`, `let`, `function`) stay live in VM memory across `run_js` calls for the rest of the current user turn (do NOT re-declare variables that already succeeded in earlier `run_js` calls):
  - **Dataflow-sink display rule (do NOT use `console.log(...)`)**: any variable consumed by a later statement in the same step (e.g. `const raw = io.read(...); const hits = raw.grep(...);` or `const list = io.list(...); list.entries.filter(...)`) stays **silent in live VM memory**; ONLY unconsumed result variables (e.g. flat multi-line `const a = f1.slice(...);\nconst b = f2.slice(...);`) and bare query expressions (e.g. `a.slice(...); b.slice(...);` or `[a.slice(...), b.slice(...)]`) are returned automatically in clean unescaped blocks. Avoid `console.log(...)` (it is strictly capped at 4KB).
  - Results up to **`64KB`** per step are returned verbatim and stay in your conversation history for the entire turn — do NOT re-read or re-slice content already shown in an earlier `tool` message. If a batched array/object exceeds `64KB`, the first `64KB` is still printed verbatim and only overflow tail items are summarized.
  - Large files/results (`> 64KB`) stay live in VM memory as `ResourceHandle` (`Ref#N`) with a compact signature + structural/error index in the `tool` output (and any silent new file/command handles loaded in a step are noted in a 1-line `// [live handles added: ...]` footer so you can reuse them without re-reading).
  - Frozen turn snapshots are immutable host assets. Use `session.snapshot({ asset_id })` to restore the exact saved file, command output, or JSON value even after the workspace changes. `session.read_asset(...)` remains available for paged asset delivery.
- **Lazy Resource Handles & Programmatic JS**: `io.read(...)` returns a live `ResourceHandle` (`Ref#N`) with zero-I/O methods: `f.slice(startLine, endLine)` (1-based inclusive, or multi-range `f.slice([[1, 10], [60, 75]])`), `f.grep(pattern, contextLines?)`, `f.outline()`, `f.head(n)`, `f.tail(n)`. Bind batch-read handles by name (e.g. `const [a, b] = await Promise.all([...])` or `const H = Object.fromEntries(paths.map((p, i) => [p, files[i]]))`) so you can reuse `a.slice(...)` / `H[path].grep(...)` across steps without re-reading unchanged files. Use JS loops, regexes, sets, and maps to diff, verify, or extract across multiple files in a single step.

## EDITING AND BUILDING
- When changing multiple lines, deeply indented code, or code with complex quotes and backslashes, prefer the line-precise `io.replace({ path, start_line, end_line, replacement })`. Use a JavaScript template string or `lines.join('\n')` for the replacement instead of hand-building a long single-quoted `old_text` anchor.
- Successful `io.edit` and `io.replace` responses include a line-numbered `preview` of the changed area. Once it confirms the edit, continue without an extra whole-file `io.read`.
- Do not run multiple `cargo` or package-manager build/test commands inside `Promise.all`. Run them serially—prefer combining multiple targeted test filters or `&&`-chained checks into ONE serial command (e.g. `cargo test --bin ... -- test_a test_b && cargo check ...`) instead of spending a separate round-trip per filter.
- Use `browser.fill`, `browser.set_viewport`, `browser.scroll`, `browser.wait`, and `browser.screenshot` with a selector for interactive and responsive checks without shell scripts. To start a long-running dev server for browser checks, use `process.run_project_script` (and `process.stop_project_process` when done) rather than backgrounding `cmd &` inside a short-lived `exec_command`, which terminates its process group when the command exits.
- Prefer `verify.check` and `verify.test` for supported project checks; read raw runner output through `session.read_asset` only when the structured diagnostics are insufficient.
- For any non-trivial task with 3+ distinct steps or multiple files to modify, call `plan.set({ steps })` in your first `run_js` step to publish a concise 3–8 step checklist, then call `plan.update({ id, status })` as each step moves to `in_progress`, `done`, or `blocked`. Keep IDs stable; never mark unverified work done.
- After a failed `io.apply_edits` or `io.edit`, check `globalThis.__lastFailedEdit`. Patch only the failed anchor's `old_text` or 1-based `line_hint` in `.args` and retry; do not re-emit unchanged `new_text`.
- For a partial batch (`atomic: false`), use the draft's `failed_files` indices and retry only those files.
- Use `git.blame` for line history. `git.commit` and `git.restore` require explicit paths; use them only when the user wants those changes. `io.apply_edits` preflights a multi-file edit before writing by default.
- **UI visual self-review stops after two rounds**: one pass on the first render, then one fix-and-re-verify pass. Once nothing is visibly clipped, overflowing, misaligned, or hard to read (contrast/legibility), report the result and finish — do not keep polishing details in a long loop.
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
- **Parallel host I/O, searches, git, and read-only checks**: pre-bound `io.*` / `git.*` / `process.*` are **synchronous** (calling them inside `Promise.all` runs sequentially!). For true concurrency, use the pre-bound **`.promises`** sub-namespace (or `require("<lib>/promises")`):
  - `const files = await Promise.all(paths.map(path => io.promises.read({ path })));`
  - `const [a, b] = await Promise.all([process.promises.run_readonly({ cmd: "..." }), process.promises.run_readonly({ cmd: "..." })]);`
  - `const [st, lg] = await Promise.all([git.promises.status(), git.promises.log({ limit: 8 })]);`
- **Ordinary JS functions & concurrent `async function` helpers** (`function foo(...)` / `async function checkA(...)`): prefer plain JS functions for deterministic extraction, filtering, cross-checking, counting, or batch transformations, and run multiple `async function` helpers together via `await Promise.all([checkA(), checkB()])` (true concurrent host I/O, compact return values, 0 extra LLM calls).
- **Delegation (`host.delegate`) — AI-decided, single lightweight tasks only**: decide for yourself whether delegating improves efficiency. Delegation is recommended (either concurrently via `Promise.allSettled([...])` / `Promise.all([...])` when independent sub-tasks benefit from parallelism, or as a single focused call) whenever offloading a clean sub-problem saves time or keeps the main frame concise; when target files heavily overlap or batched main-frame JS is faster, handle it directly in the main frame. Keep deterministic filtering, regex extraction, counting, and cross-file correlation in plain parent JS (0 extra LLM calls).
  - **STRICT RULE — single lightweight task only; NEVER delegate complex or heavyweight tasks**: each `host.delegate` call MUST be a **single, narrow, self-contained task** (e.g., comparing one specific pair of declarations and structs, inspecting one bounded file or symbol, or running one targeted verification). **Do NOT delegate broad, complex, multi-module, or open-ended investigations** to a worker: heavyweight workers lose efficiency through duplicated reads, long wandering traces, and high parent re-verification cost.
  - **Delegate context names**: fields in `context` may be used directly by their safe variable name or through `context.<key>` / `context["<key>"]`. If a field collides with a JavaScript keyword or an injected host library such as `host` or `io`, read it through `context` so the field cannot replace the host binding. Safe fields remain available as direct parameters, while the complete context object remains available for uniform access.
  - A delegated worker is a teammate with the parent's regular `io.*`, `git.*`, `process.*`, plugin, and permission/sandbox capabilities, running in an independent frame discarded on return (Stack-Frame GC). Only nested `@virtual` and nested `host.delegate` are forbidden.
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
      process.promises.run_readonly({ cmd: "..." }),
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
3. **Host libraries (`io`, `git`, `host`, `process`, plugins, and their `.promises` interfaces)** proxy into Rust with the same permission and sandbox boundary as the parent. You may independently follow related files and execute the read-only commands or tests needed for this contract. Use `.promises` for true concurrency. When searching across files, prefer passing a confirmed directory path such as `path: "src"` to `io.search` instead of guessing a single module filename. Nested `@virtual` declarations and nested `host.delegate` are strictly forbidden.
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

/// Build the provider wire body: static system prompt + `.d.ts` declarations +
/// conversation turns + ONLY the single fixed `run_js({ code })` shell tool.
pub(super) fn build_request_body(
    s: &Session<'_>,
    w: &Window,
    libs: &Libraries,
) -> Value {
    let base = w.pinned_system_prompt.as_deref().unwrap_or(V2_SYSTEM_PROMPT);
    build_request_body_for_doc(s, &w.context_doc, base, libs)
}

pub(super) fn pinned_system_prompt(s: &Session<'_>, libs: &Libraries) -> String {
    let mut text = String::from(V2_SYSTEM_PROMPT);
    text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
    text.push_str(&declarations::static_library_header(libs));
    if !s.scope.starts_with("virtual:") {
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

pub(super) fn build_request_body_for_doc(
    s: &Session<'_>,
    doc: &super::context_doc::ContextDocument,
    base_prompt: &str,
    libs: &Libraries,
) -> Value {
    let mut system_text = String::from(base_prompt);
    let has_pinned_sdk = base_prompt.contains("/* === RUNTIME HOST SDK (.d.ts) === */");
    if !has_pinned_sdk {
        system_text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
        system_text.push_str(&declarations::static_library_header(libs));
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

    let mut messages = vec![json!({"role": "system", "content": system_text})];
    messages.extend(doc.render_conversation_messages());

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
        let diagnostics = crate::model::model_transport::ModelRequestLog::new(
            s.state.workspace.root(),
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
            &s.state.provider_url,
            &body,
        );
        let _permit = tokio::select! {
            _ = s.cancel.cancelled() => anyhow::bail!("model request cancelled while waiting for slot"),
            permit = s.state.model_request_slots.acquire() => permit.context("model request queue closed")?,
        };
        diagnostics.sending();
        let attempt_res = async {
            let response = crate::model::anthropic_messages::send(s.state, s.task, &s.scope, &body)
                .header("x-client-request-id", node)
                .header(
                    "x-agent-affinity",
                    crate::model::model_transport::affinity(s.task, &s.scope),
                )
                .send()
                .await?;
            diagnostics.headers(&response);
            let response = crate::media::visual_probe::successful_response(response).await?;
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
    use super::V2_SYSTEM_PROMPT;

    #[test]
    fn ui_self_review_is_bounded_and_local_images_register_directly() {
        assert!(V2_SYSTEM_PROMPT.contains("UI visual self-review stops after two rounds"));
        assert!(V2_SYSTEM_PROMPT.contains("vision.register_image({ path })"));
        assert!(V2_SYSTEM_PROMPT.contains("io.delete"));
        assert!(V2_SYSTEM_PROMPT.contains("running a project's own builder or test runner"));
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
}
