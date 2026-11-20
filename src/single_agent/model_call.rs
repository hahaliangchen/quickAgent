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
use crate::agent_program::{declarations, libraries::Libraries};
use crate::agent_service;
use crate::model_stream::{RunSubmission, ToolCall};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) const RUN_JS_TOOL_NAME: &str = "run_js";

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

const V2_SYSTEM_PROMPT: &str = r#"You are a coding agent continuing the body of `async function executeTask()` inside a persistent per-turn JavaScript VM.
There are ZERO business-tool JSON schemas — your ONLY tool is `run_js({ code })`, which executes incremental JavaScript statements against the pre-bound host libraries declared in the `.d.ts` below.

## STRICT SINGLE-STEP PROTOCOL
Each response MUST be **one** of the two actions below:

### Action 1: Execute the next JS statement(s) via `run_js({ code })`
- Call `run_js` with raw JavaScript statements to append at the cursor inside `async function executeTask() { ...` (NO markdown fences, NO fabricated `// =>` results).
- You may optionally write a brief 1-sentence progress note in your assistant message text before calling `run_js`, and keep `code` 100% pure JavaScript.
- Emit ONE concise `run_js` call per step so the host executes it and returns real `// =>` / `EPHEMERAL_PEEK` output before your next step.
- Host libraries (`io`, `git`, `process`, `host`, `session`, etc.) declared in the `.d.ts` below are pre-bound in scope and proxy synchronously into Rust.
- All top-level variables (`const`, `let`, `function`) stay live in VM memory for the rest of the current user turn (do NOT re-declare variables that already succeeded in earlier steps):
  - Short results (`<= 2KB`) print verbatim in permanent `// =>` comments below your code.
  - Medium results (`2KB..=24KB`) leave a compact `[File(...) | ...]` / `[Object ...]` signature in `// =>` and expand in full (with line numbers) inside `EPHEMERAL_PEEK` at the bottom for ONE step only.
  - Large results (`> 24KB`) show a high-entropy index (symbols / error lines + tail / schema) in `EPHEMERAL_PEEK`.
- **Lazy Resource Handles & Code-as-Projection**: `io.read(...)` returns a live `ResourceHandle` (`Ref#N`) with built-in zero-I/O paging methods: `f.slice(startLine, endLine)` (1-based inclusive, or multi-range `f.slice([[1, 10], [60, 75]])`), `f.grep(pattern, contextLines?)`, `f.outline()`, `f.head(n)`, `f.tail(n)`. `EPHEMERAL_PEEK` disappears on the next step — pin only the lines you still need into a small variable (`<= 2KB`, e.g. `const bug = f.slice(61, 75);`), or reply directly once you have everything needed.

### Action 2: Finish turn and reply to the user
- ONLY after you have real observed evidence (never before reading the target files or running requested checks), reply directly in normal Markdown **WITHOUT calling `run_js`**.
- Everything in your plain assistant reply (without `run_js`) is delivered verbatim to the user and NEVER executed as JS (so ```` ```js ```` / ```` ```rust ```` example blocks need zero escaping).

## Ordinary JS Functions vs Isolated `@virtual` Functions
- Use ordinary JS functions/loops (`function foo(...) { ... }`) for deterministic multi-file reads, filtering, or batch computation (0 extra LLM calls).
- Declare a single-layer `/** @virtual <contract; ask for a concise summary field> */ async function inspect(item) {}` (optionally with `@uses varName` in JSDoc to pass outer bindings) and call `await inspect(x)` or `await Promise.all(items.map(inspect))` when isolated semantic judgment or parallel fan-out is needed. Pass file paths OR `ResourceHandle` objects (`const f = io.read(...); await inspect(f);`) directly — handles passed to `@virtual` stay compact (`Ref#N`) in your outer trace without expanding raw file contents, and auto-dereference inside the isolated child frame. Each `@virtual` call runs in its own isolated non-shell stack frame, discards its intermediate context on `return`, and leaves a factual `[virtual ...]` receipt + return value in your `// =>` trace.

## Per-Turn Clean VM & Cross-Turn Continuity
- When a turn finishes, its VM is completely destroyed. Each new user turn starts with a fresh empty VM.
- Large results (`> 24KB`) are automatically archived with an `asset: asset-...` id in permanent `// =>`; in later turns you can page them back via `session.read_asset({ asset_id, char_offset, max_chars })`.
- Reusable cross-session JS orchestration libraries (listed below the `.d.ts` when available) can be loaded with `const lib = require("<name>");`.
- If you cannot proceed without user clarification, call `host.ask_user({ question: "..." });` inside `run_js` to pause the turn and deliver your question directly.

## Multi-Step Interaction Example
[Step 1 — call `run_js`]:
`run_js({ code: "const f = io.read({ path: \"src/auth.rs\" });" })`

[Host executes Step 1 and appends `// => f = [File("src/auth.rs") | Ref#1 | ...]` + `EPHEMERAL_PEEK` full file with line numbers]

[Step 2 — call `run_js` to pin key lines via handle `.slice()` (optional) OR reply directly without `run_js`]:
`run_js({ code: "const verifyFn = f.slice(61, 75);" })`

[Step 3 — final Markdown reply WITHOUT calling `run_js`]:
Found the vulnerability in `src/auth.rs` lines 61–75...

```rust
// Suggested fix (shown to user, never executed)
```
"#;

const VIRTUAL_LEAF_SYSTEM_PROMPT: &str = r#"You are executing inside an isolated single-layer `@virtual` JavaScript function frame.
Complete ONLY this function's contract using its bound parameters, `@uses` captures, and allowed non-shell libraries in the `.d.ts` below.
Your ONLY tool is `run_js({ code })`.

## STRICT RULES
1. **Always call `run_js({ code })`** with raw JavaScript statements to continue the function body (NO markdown fences, NO fabricated `// =>` comments). Do NOT reply with plain text outside `run_js`.
2. **Bound parameters and `@uses` captures are already live JS variables** — use them directly by identifier name.
3. **Host libraries (`io`, `git`, `host`)** proxy synchronously into Rust. Shell (`process.*`) and nested `@virtual` declarations are strictly forbidden.
4. **Finish by executing native JS `return <json_value>;` inside `run_js`**:
   - Always include a short `summary` field in your returned object (e.g. `return { ok: true, summary: "audited src/a.rs: no issues" };`) so the caller knows exactly what you checked or changed, even for file-editing tasks.
   - If the contract cannot be satisfied, execute `return { "$task_error": "concrete reason" };`.
"#;

pub(super) struct ModelReply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
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
            crate::model_stream::parse_run_submission(&reply.content, reply.finish_reason.as_deref())
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
    build_request_body_for_doc(s, &w.context_doc, V2_SYSTEM_PROMPT, libs)
}

pub(super) fn build_request_body_for_doc(
    s: &Session<'_>,
    doc: &super::context_doc::ContextDocument,
    base_prompt: &str,
    libs: &Libraries,
) -> Value {
    let mut system_text = String::from(base_prompt);
    system_text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
    system_text.push_str(&declarations::static_library_header(libs));
    if !s.scope.starts_with("virtual:") {
        let orch = crate::flow_libraries::list_libraries(s);
        if !orch.is_empty() {
            system_text.push_str("\n// Reusable JS orchestration libraries via `require(\"<name>\")`:\n");
            for (name, desc) in orch {
                let methods = crate::flow_libraries::public_method_names(s, &name).unwrap_or_default();
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
    let estimated_input = crate::context_window::estimate(&body);
    body["max_tokens"] = json!(crate::model_settings::output_tokens(
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
        body["prompt_cache_key"] = json!(crate::model_transport::affinity(s.task, &s.scope));
    }
    if is_responses {
        body = crate::model_stream::responses_request(&body);
    } else if is_messages {
        body = crate::anthropic_messages::wire_body(s.state, s.model, &body);
    }
    body
}

pub(super) async fn call_model(
    s: &Session<'_>,
    w: &Window,
    libs: &Libraries,
    step: usize,
) -> Result<ModelReply> {
    let body = build_request_body(s, w, libs);
    let node = format!("step_{}_{}", s.turn, step);
    send_model_request(s, w.task_node_id.as_deref(), body, &node, step, "agent").await
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
    send_model_request(s, s.task_node_id.as_deref(), body, &node, sub_step, "virtual").await
}

async fn send_model_request(
    s: &Session<'_>,
    frame: Option<&str>,
    body: Value,
    node: &str,
    step: usize,
    actor: &str,
) -> Result<ModelReply> {
    let trace = crate::request_context::record(
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

    let mut attempt = 1u32;
    loop {
        let diagnostics = crate::model_transport::ModelRequestLog::new(
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
            let response = crate::anthropic_messages::send(s.state, s.task, &s.scope, &body)
                .header("x-client-request-id", node)
                .header(
                    "x-agent-affinity",
                    crate::model_transport::affinity(s.task, &s.scope),
                )
                .send()
                .await?;
            diagnostics.headers(&response);
            let response = crate::visual_probe::successful_response(response).await?;
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
            diagnostics.phase("completed");
            Ok::<(Value, Value), anyhow::Error>((message, stats))
        }
        .await;

        match attempt_res {
            Ok((message, stats)) => {
                let content = message["content"].as_str().unwrap_or("").to_owned();
                let tool_calls = crate::model_stream::tool_calls(&message);
                let finish_reason = stats["finish_reason"].as_str().map(str::to_owned);
                return Ok(ModelReply {
                    content,
                    tool_calls,
                    finish_reason,
                });
            }
            Err(error) => {
                if attempt >= 3 || s.cancel.is_cancelled() || diagnostics.output_started() {
                    return Err(error);
                }
                let delay = diagnostics
                    .retry_after()
                    .unwrap_or_else(|| crate::model_transport::backoff(attempt));
                attempt += 1;
                tokio::select! {
                    _ = tokio::time::sleep(delay.min(Duration::from_secs(10))) => {}
                    _ = s.cancel.cancelled() => anyhow::bail!("cancelled"),
                }
            }
        }
    }
}
