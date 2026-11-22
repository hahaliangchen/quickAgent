//! Context-as-Code v2 agent driver (Step 1: minimal pure-JS + Rust proxy loop).
//!
//! - Model ABI: raw JavaScript continuation inside `async function executeTask()`,
//!   zero JSON Schema `tools`, `.d.ts` host library declarations in `system`,
//!   and `return <<<` (or native JS `return <expr>;`) to finish a turn.
//! - Host ABI: Rust JS VM (`crate::agent_program::Runtime`) traps tool calls
//!   via `ActionKind::Tool` and proxies them into `host_tools` with full
//!   permission, sandbox, and file-baseline enforcement.
//! - Context layout: immutable Append-Only code + `// =>` receipts above, and
//!   one-step `EPHEMERAL_PEEK` at the bottom (100% KV-cache friendly).
//! - Per-turn isolation: every user turn starts with a brand-new empty VM and
//!   destroys it immediately upon emitting the final `assistant` reply.

#![allow(dead_code)]

use crate::server::agent_service::AgentServiceState;
use anyhow::{Result, bail};
use serde_json::json;
use tokio_util::sync::CancellationToken;

mod context_doc;
mod context_engine;
mod external_changes;
mod flow_runtime;
mod host_folder;
mod host_tools;
mod managed_files;
mod model_call;
mod model_gateway;
pub(crate) mod modules;
mod step_runner;
mod task_frames;
#[cfg(test)]
mod tests;
mod tool_executor;
mod tool_proxy;
mod virtual_frame;
mod vm_driver;

use context_engine::{ContextEngine, append_event};
pub(crate) use context_engine::{Window, cancel_saved_programs};
#[cfg(test)]
use host_tools::{business_tool, business_tool_inner};
use model_gateway::{ModelGateway, NextAction};
use step_runner::{JsStepRunner, StepFlow};
use tool_executor::ToolExecutor;
pub(crate) use tool_executor::library_compile_scope;

/// Production loops have no step cap; tests pass small bounds of their own.
pub(crate) const NO_ROUND_LIMIT: usize = usize::MAX;
/// A completed turn is eligible for economic folding only after the provider's
/// short-lived prompt cache is expected to have expired.
pub(crate) const IDLE_CACHE_TTL_MS: u64 = 10 * 60 * 1000;

/// Per-invocation host session context shared by the agent driver, host tool
/// backend, and flow libraries.
#[derive(Clone)]
pub(crate) struct Session<'a> {
    pub(super) agent_id: Option<String>,
    pub(super) task_node_id: Option<String>,
    pub(crate) plugin: Option<String>,
    pub(crate) state: &'a AgentServiceState,
    pub(crate) task: &'a str,
    pub(super) model: &'a str,
    pub(super) turn: usize,
    pub(super) cancel: &'a CancellationToken,
    pub(super) ban_execution: bool,
    pub(super) scope: String,
    pub(super) parent: Option<String>,
    pub(super) input_budget: usize,
}

impl Session<'_> {
    pub(super) fn delegate_id(&self) -> Option<&str> {
        self.agent_id.as_deref()
    }
}

/// Primary agent entry point (convenience wrapper without explicit plugin resource).
pub(crate) async fn run_task(
    state: AgentServiceState,
    task: String,
    model: String,
    prompt: String,
    max_rounds: usize,
    cancel: CancellationToken,
    turn: usize,
    ban_execution: bool,
) -> Result<()> {
    run_task_with_resource(
        state,
        task,
        model,
        prompt,
        max_rounds,
        cancel,
        turn,
        ban_execution,
        None,
    )
    .await
}

/// Primary Context-as-Code v2 driver entry point.
pub(crate) async fn run_task_with_resource(
    state: AgentServiceState,
    task: String,
    model: String,
    prompt: String,
    max_rounds: usize,
    cancel: CancellationToken,
    turn: usize,
    ban_execution: bool,
    program_resource: Option<String>,
) -> Result<()> {
    // The resource argument is retained for callers; this driver starts a fresh turn VM.
    let _ = program_resource;
    let session = Session {
        agent_id: None,
        task_node_id: None,
        plugin: crate::server::plugin_sessions::owner(state.workspace.root(), &task)?,
        state: &state,
        task: &task,
        model: &model,
        turn,
        cancel: &cancel,
        ban_execution,
        scope: "main".into(),
        parent: None,
        input_budget: crate::model::model_settings::input_budget(&state, &model),
    };
    AgentTurnRunner { session }.run(prompt, max_rounds).await
}

struct AgentTurnRunner<'a> {
    session: Session<'a>,
}

impl AgentTurnRunner<'_> {
    async fn run(&self, prompt: String, max_rounds: usize) -> Result<()> {
        let s = &self.session;
        let context = ContextEngine::new(s);
        let model = ModelGateway::new(s);
        let tools = ToolExecutor::new(s);
        let runner = JsStepRunner::new(s);
        let (mut w, libs) = context.begin_turn(prompt).await?;
        let allow_shell = s.plugin.is_none() && !s.ban_execution;

        for step in 1..=max_rounds {
            if s.cancel.is_cancelled() {
                context.abort_turn(&mut w).await?;
                bail!("cancelled");
            }

            append_event(
                s,
                "step/start",
                json!({
                    "actor": "agent",
                    "turn": s.turn,
                    "step": step,
                }),
                false,
            )
            .await?;

            context.deliver_interjections(&mut w, None).await?;
            let estimated_request = context.maybe_fold_before_request(&mut w, &libs).await?;
            match model
                .request_next_action(&context, &mut w, &libs, step, estimated_request)
                .await?
            {
                NextAction::Rejected {
                    code,
                    message,
                    raw_preview,
                    assistant_msg,
                    call_id,
                } => {
                    append_event(
                        s,
                        "program/source_invalid",
                        json!({
                            "actor": "agent",
                            "turn": s.turn,
                            "step": step,
                            "code": code,
                            "error": message,
                            "diagnostic": {"error": message},
                            "raw_preview": raw_preview,
                        }),
                        false,
                    )
                    .await?;
                    w.context_doc
                        .record_rejection(assistant_msg, call_id, code, &message);
                    context.save_window(&w).await?;
                }
                NextAction::FinalReply(markdown) => {
                    if context
                        .deliver_interjections(&mut w, Some(&markdown))
                        .await?
                        > 0
                    {
                        continue;
                    }
                    context
                        .finish_turn_with_reply(&mut w, step, markdown)
                        .await?;
                    return Ok(());
                }
                NextAction::RunJs {
                    source,
                    pending_reply,
                    assistant_msg,
                    call_id,
                } => {
                    if runner
                        .execute_js_step(
                            &context,
                            &tools,
                            &mut w,
                            &libs,
                            step,
                            allow_shell,
                            source,
                            pending_reply,
                            assistant_msg,
                            call_id,
                        )
                        .await?
                        == StepFlow::Stop
                    {
                        return Ok(());
                    }
                }
            }
        }

        // Reached max_rounds (mainly test cap); persist current state cleanly.
        context.save_window(&w).await?;
        Ok(())
    }
}
