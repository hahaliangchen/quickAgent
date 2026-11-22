//! Prepare a main-turn JS segment, drive it to completion, and record its result.

use super::{
    Session, Window, append_event, context_engine::ContextEngine, tool_executor::ToolExecutor,
    vm_driver,
};
use crate::agent_program::{self, State, libraries::Libraries};
use anyhow::{Result, bail};
use serde_json::{Value, json};

#[derive(PartialEq, Eq)]
pub(super) enum StepFlow {
    Continue,
    Stop,
}

pub(super) struct JsStepRunner<'s, 'a> {
    session: &'s Session<'a>,
}

impl<'s, 'a> JsStepRunner<'s, 'a> {
    pub(super) fn new(session: &'s Session<'a>) -> Self {
        Self { session }
    }

    /// Execute one JS step (with optional trailing `return <<<` reply).
    /// Returns `StepFlow::Stop` if the turn finished and delivered the final reply.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_js_step(
        &self,
        context: &ContextEngine<'_, '_>,
        tools: &ToolExecutor<'_, '_>,
        w: &mut Window,
        libs: &Libraries,
        step: usize,
        allow_shell: bool,
        source: String,
        pending_reply: Option<String>,
        assistant_msg: Option<Value>,
        call_id: Option<String>,
    ) -> Result<StepFlow> {
        let s = self.session;
        let Some(mut runtime) = w.program.take() else {
            bail!("turn VM missing");
        };
        runtime.install_static_libraries(libs)?;
        let pre_fps = runtime.user_binding_fingerprints();
        let log_start = runtime.logs_len();

        // Main turn allows declaring top-level `@virtual` functions (`is_virtual_leaf = false`).
        if let Err(compile_err) = runtime.prepare_segment(&source, libs.clone(), false) {
            let msg = format!("{compile_err:#}");
            append_event(
                s,
                "program/source_invalid",
                json!({
                    "actor": "agent",
                    "turn": s.turn,
                    "step": step,
                    "code": "COMPILE_ERROR",
                    "error": msg,
                    "diagnostic": {"error": msg},
                    "source": source,
                }),
                false,
            )
            .await?;
            w.program = Some(runtime);
            w.context_doc
                .record_rejection(assistant_msg, call_id, "COMPILE_ERROR", &msg);
            context.save_window(w).await?;
            return Ok(StepFlow::Continue);
        }

        let node = format!("step_{}_{}", s.turn, step);
        append_event(
            s,
            "program/start",
            json!({
                "actor": "agent",
                "turn": s.turn,
                "step": step,
                "source": source,
                "lines": source.lines().count(),
            }),
            false,
        )
        .await?;
        if w.next_ref_id == 0 {
            w.next_ref_id = 1;
        }
        let mut fallback_file_refs = std::collections::BTreeMap::new();
        let file_refs = w
            .context_doc
            .active_turn_mut()
            .map(|turn| &mut turn.file_handle_refs)
            .unwrap_or(&mut fallback_file_refs);
        let (final_state, external_tool_calls, hints) = vm_driver::drive_vm_segment(
            s,
            tools,
            &mut runtime,
            libs,
            &node,
            step,
            allow_shell,
            &mut w.next_ref_id,
            file_refs,
        )
        .await?;
        let logs = runtime.logs_since(log_start);

        match final_state {
            State::Returned(val) => {
                if runtime.segment_returned_explicitly() && external_tool_calls == 0 {
                    let answer = match val {
                        Value::String(text) => text,
                        Value::Null => String::new(),
                        other => serde_json::to_string_pretty(&other).unwrap_or_default(),
                    };
                    w.context_doc.record_completed_segment(
                        source,
                        &runtime,
                        &pre_fps,
                        &logs,
                        None,
                        &hints,
                        assistant_msg,
                        call_id,
                    );
                    context.finish_turn_with_reply(w, step, answer).await?;
                    return Ok(StepFlow::Stop);
                }
                if let Some(reply_md) = pending_reply {
                    if external_tool_calls == 0 {
                        // Pure in-memory JS projection + final reply: record tool output and finish!
                        w.context_doc.record_completed_segment(
                            source,
                            &runtime,
                            &pre_fps,
                            &logs,
                            None,
                            &hints,
                            assistant_msg,
                            call_id,
                        );
                        context.finish_turn_with_reply(w, step, reply_md).await?;
                        return Ok(StepFlow::Stop);
                    }
                    // First-Trap Truncation: external host tools/virtual calls ran, so the trailing
                    // `return <<<` was written before observing real results — discard it!
                    let note = format!(
                        "// [Host note: executed your JS above ({external_tool_calls} host/virtual call(s)) and discarded the premature `return <<<` so you can inspect the real result below before replying.]"
                    );
                    w.context_doc.record_completed_segment(
                        source,
                        &runtime,
                        &pre_fps,
                        &logs,
                        Some(note),
                        &hints,
                        assistant_msg,
                        call_id,
                    );
                    w.program = Some(runtime);
                    context.save_window(w).await?;
                    return Ok(StepFlow::Continue);
                }
                let note = (runtime.segment_returned_explicitly() && external_tool_calls > 0).then(|| {
                    format!(
                        "// [Host note: executed your JS above ({external_tool_calls} host/virtual call(s)); inspect the real output below before returning your final answer.]"
                    )
                });
                w.context_doc.record_completed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    note,
                    &hints,
                    assistant_msg,
                    call_id,
                );
                w.program = Some(runtime);
                context.save_window(w).await?;
                Ok(StepFlow::Continue)
            }
            State::Failed(err) => {
                if agent_program::runtime::is_user_blocker(&err) {
                    let question = err["question"]
                        .as_str()
                        .or_else(|| err["message"].as_str())
                        .map(str::trim)
                        .filter(|q| !q.is_empty())
                        .unwrap_or("需要你的确认后才能继续。")
                        .to_owned();
                    w.context_doc.record_paused_for_user(
                        source,
                        &runtime,
                        &pre_fps,
                        &logs,
                        &hints,
                        assistant_msg,
                        call_id,
                    );
                    append_event(
                        s,
                        "agent/needs_user",
                        json!({
                            "actor": "agent",
                            "turn": s.turn,
                            "step": step,
                            "question": question,
                        }),
                        false,
                    )
                    .await?;
                    context.finish_turn_with_reply(w, step, question).await?;
                    return Ok(StepFlow::Stop);
                }
                append_event(
                    s,
                    "program/error",
                    json!({
                        "actor": "agent",
                        "turn": s.turn,
                        "step": step,
                        "error": err,
                    }),
                    false,
                )
                .await?;
                w.context_doc.record_failed_segment(
                    source,
                    &runtime,
                    &pre_fps,
                    &logs,
                    &err,
                    assistant_msg,
                    call_id,
                );
                w.program = Some(runtime);
                context.save_window(w).await?;
                Ok(StepFlow::Continue)
            }
            State::Action(_) | State::Waiting => {
                w.program = Some(runtime);
                context.save_window(w).await?;
                bail!("VM suspended in an unexpected waiting state");
            }
        }
    }
}
