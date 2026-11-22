//! Request one model step, including the single context-length retry.

use super::{
    Session, Window,
    context_engine::{ContextEngine, unix_now_ms},
    model_call,
};
use crate::{agent_program::libraries::Libraries, model_stream::RunSubmission};
use anyhow::Result;
use serde_json::Value;

pub(super) enum NextAction {
    RunJs {
        source: String,
        pending_reply: Option<String>,
        assistant_msg: Option<Value>,
        call_id: Option<String>,
    },
    FinalReply(String),
    Rejected {
        code: &'static str,
        message: String,
        raw_preview: String,
        assistant_msg: Option<Value>,
        call_id: Option<String>,
    },
}

pub(super) struct ModelGateway<'s, 'a> {
    session: &'s Session<'a>,
}

impl<'s, 'a> ModelGateway<'s, 'a> {
    pub(super) fn new(session: &'s Session<'a>) -> Self {
        Self { session }
    }

    pub(super) async fn request_next_action(
        &self,
        context: &ContextEngine<'_, '_>,
        w: &mut Window,
        libs: &Libraries,
        step: usize,
        estimated_request: usize,
    ) -> Result<NextAction> {
        let s = self.session;
        let reply = match model_call::call_model(s, w, libs, step).await {
            Ok(reply) => reply,
            Err(error) if is_context_length_error(&error) => {
                context.force_fold_active_prefix(w).await?;
                model_call::call_model(s, w, libs, step).await?
            }
            Err(error) => return Err(error),
        };
        // The timestamp is persisted with the next window save and is deliberately
        // recorded only after a model response has completed successfully.
        w.last_model_request_at_ms = Some(unix_now_ms());
        #[cfg(test)]
        if std::env::var_os("LIVE_DEBUG_STEPS").is_some() {
            let head: String = reply.content.chars().take(160).collect();
            let tc = reply
                .tool_calls
                .first()
                .map(|c| format!("{}({})", c.name, c.arguments.len()));
            eprintln!(
                "[LIVE step={step} finish={:?} tc={tc:?} text_len={}] head={head:?}",
                reply.finish_reason,
                reply.content.len()
            );
        }
        let submission = model_call::parse_main_submission(&reply);
        let assistant_msg = Some(reply.assistant_message.clone());
        let call_id = reply.primary_call_id();
        let remaining_budget_chars = s
            .input_budget
            .saturating_sub(estimated_request)
            .saturating_mul(4);
        w.context_doc
            .set_inline_budget_chars(remaining_budget_chars);
        Ok(match submission {
            RunSubmission::Run { source } => NextAction::RunJs {
                source,
                pending_reply: None,
                assistant_msg,
                call_id,
            },
            RunSubmission::CodeAndReply { source, markdown } => NextAction::RunJs {
                source,
                pending_reply: Some(markdown),
                assistant_msg,
                call_id,
            },
            RunSubmission::Reply { markdown } => NextAction::FinalReply(markdown),
            RunSubmission::Rejected { code, message } => NextAction::Rejected {
                code,
                message,
                raw_preview: reply.content.chars().take(600).collect(),
                assistant_msg,
                call_id,
            },
        })
    }
}

fn is_context_length_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    text.contains("context_length_exceeded")
        || text.contains("context length")
        || text.contains("maximum context")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_length_errors_trigger_the_single_retry_path() {
        assert!(is_context_length_error(&anyhow::anyhow!(
            "400 context_length_exceeded"
        )));
        assert!(is_context_length_error(&anyhow::anyhow!(
            "maximum context window reached"
        )));
        assert!(!is_context_length_error(&anyhow::anyhow!(
            "network timeout"
        )));
    }
}
