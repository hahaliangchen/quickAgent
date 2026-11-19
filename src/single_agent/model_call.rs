//! Zero-Schema HTTP model caller for Context-as-Code v2.
//!
//! Sends:
//! - Static `system` prompt (pure JS continuation + `return <<<` protocol +
//!   `.d.ts` host library declarations from `declarations::static_library_header`).
//! - Paired `(user #1, assistant #1, ..., user #N)` messages from `ContextDocument`.
//! - ZERO JSON Schema `tools` / `tool_choice` / `parallel_tool_calls`.

use super::{Session, Window};
use crate::agent_program::{declarations, libraries::Libraries};
use crate::agent_service;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;

const V2_SYSTEM_PROMPT: &str = r#"You are a coding agent driving a persistent per-turn JavaScript VM inside `async function executeTask()`.
There are ZERO JSON tool schemas. Writing executable JavaScript is your ONLY way to call host capabilities.

## Output Protocol (STRICT)
1. **Execute JavaScript (default)**: Output raw executable JavaScript statements directly (NO XML tags, NO Markdown explanations, NO prose outside JS comments).
   - Host libraries (`io`, `git`, `process`, `host`, `session`, etc.) declared in the `.d.ts` below are pre-bound in scope and proxy synchronously into Rust.
   - All top-level variables (`const`, `let`, `function`) stay live in VM memory for the rest of the current turn.
   - Each step's results appear permanently in `// =>` comments below your code:
     - Short results (`<= 2KB`) print verbatim in `// =>`.
     - Medium results (`2KB..=24KB`) leave a compact signature in `// =>` and expand in full (with line numbers) inside `EPHEMERAL_PEEK` at the very bottom for ONE step only.
     - Large results (`> 24KB`) show a high-entropy index (symbols / error lines + tail / schema) in `EPHEMERAL_PEEK`.
   - **Code-as-Projection**: When `EPHEMERAL_PEEK` shows a file or log, it vanishes on the next step. Pin the exact lines or extracted fields you need into a small variable (`<= 2KB`) using normal JS (e.g. `const bug = file.content.split("\n").slice(80, 86).join("\n");`) so the ground-truth text stays permanently in `// =>`.
2. **Finish & Reply to User**: When ready to give your final answer (including any Markdown explanations or ```` ```js ```` example code blocks), start the response with `return <<<` on the first line, followed by raw Markdown until EOF:
   ```
   return <<<
   Here is the analysis...

   ```js
   // Example code shown to the user (never executed by the VM)
   ```
   ```
   - Never mix executable JS statements and `return <<<` in the same step.
   - You may also finish with a plain JS `return "<short answer>";` statement if no Markdown formatting is needed.

## Per-Turn Clean VM
When a turn finishes, its VM is completely destroyed. Each new user turn starts with a fresh empty VM. If a later turn needs read-only facts from an earlier turn, copy only the minimal read-only JS lines—never replay `io.write` / `io.edit` / side-effecting commands.

## Few-Shot Example
// Turn step 1: read file (Rust proxy)
const f = io.read({ path: "src/auth.rs" });
// Next step, after inspecting EPHEMERAL_PEEK full view, pin only the key lines:
const checkFn = f.content.split("\n").slice(79, 86).join("\n");
// Once verified, finish in the next step:
// return <<<
// Found the bug at `src/auth.rs:82`...
"#;

pub(super) struct ModelReply {
    pub content: String,
    pub finish_reason: Option<String>,
}

/// Build the provider wire body: static system prompt + `.d.ts` declarations +
/// conversation turns, with ALL JSON tool-schema fields stripped.
pub(super) fn build_request_body(
    s: &Session<'_>,
    w: &Window,
    libs: &Libraries,
) -> Value {
    let mut system_text = String::from(V2_SYSTEM_PROMPT);
    system_text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
    system_text.push_str(&declarations::static_library_header(libs));

    let mut messages = vec![json!({"role": "system", "content": system_text})];
    messages.extend(w.context_doc.render_conversation_messages());

    let mut body = json!({
        "model": s.model,
        "messages": messages,
        "stream": true,
    });
    let estimated_input = crate::context_window::estimate(&body);
    body["max_tokens"] = json!(crate::model_settings::output_tokens(
        s.state,
        s.model,
        s.input_budget,
        estimated_input
    ));

    body = crate::model_stream::code_only_request(body);

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
    if is_responses {
        body = crate::model_stream::responses_request(&body);
        body = crate::model_stream::code_only_request(body);
    } else if is_messages {
        body = crate::anthropic_messages::wire_body(s.state, s.model, &body);
        body = crate::model_stream::code_only_request(body);
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
    let trace = crate::request_context::record(
        s.state.workspace.root(),
        s.task,
        json!({
            "actor": "agent",
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
                "actor": "agent",
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
                .header("x-client-request-id", node.as_str())
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
                w.task_node_id.as_deref(),
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
                let finish_reason = stats["finish_reason"].as_str().map(str::to_owned);
                return Ok(ModelReply {
                    content,
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
