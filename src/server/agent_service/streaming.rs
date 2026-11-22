//! Observed model stream decoding and coalesced assistant deltas.

use super::db::emit;
use futures::StreamExt;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Read an OpenAI chat completion stream, emit coalesced `assistant/delta` events, and return the assembled message.
/// Each delta names the function frame whose request it answers.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn consume_model_stream_observed(
    response: reqwest::Response,
    root: &std::path::Path,
    task_id: &str,
    turn: usize,
    step: usize,
    frame: Option<&str>,
    cancel: &CancellationToken,
    diagnostics: Option<&crate::model::model_transport::ModelRequestLog>,
) -> anyhow::Result<(Value, Value)> {
    let stream_started = Instant::now();
    let mut accumulator =
        crate::model::model_stream::ChatStream::new(crate::model::model_stream::Limits::WORKER, stream_started);
    let mut bytes = response.bytes_stream();
    let mut delta_text = String::new();
    let mut delta_reasoning = String::new();
    let mut last_flush = Instant::now();
    let mut flushed = false;
    let mut tool_dirty = false;
    let mut last_tool_flush = Instant::now();

    while !accumulator.received_done() {
        if let Some(log) = diagnostics {
            log.phase("stream_read");
        }
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(anyhow::anyhow!("cancelled")),
            next = bytes.next() => match next {
                Some(Ok(chunk)) => chunk,
                Some(Err(error)) => return Err(error.into()),
                None => { if let Some(log) = diagnostics { log.eof(); } break; },
            },
        };
        if let Some(log) = diagnostics {
            log.chunk(chunk.len());
            log.phase("stream_decode");
        }
        let delta = accumulator.push(&chunk)?;
        if let Some(log) = diagnostics {
            log.delta(&delta);
            // Keep decoded counts even when the outer deadline/cancellation
            // drops this future before a complete tool call is delivered.
            log.decoded(&accumulator.stats());
        }
        delta_text.push_str(&delta.text);
        delta_reasoning.push_str(&delta.reasoning);
        tool_dirty |= delta.tool_call;
        let tool_due =
            tool_dirty && (!flushed || last_tool_flush.elapsed() >= Duration::from_millis(500));
        let ready = !delta_text.is_empty() || !delta_reasoning.is_empty() || tool_due;
        let due = !flushed
            || last_flush.elapsed() >= Duration::from_millis(80)
            || delta_text.len() + delta_reasoning.len() >= 32;
        if ready && (due || tool_due) {
            if let Some(log) = diagnostics {
                log.phase("event_persistence");
            }
            let tool_progress = if tool_dirty {
                accumulator.tool_progress()
            } else {
                Value::Null
            };
            emit(
                root,
                task_id,
                "assistant/delta",
                json!({"turn":turn,"step":step,"task_node_id":frame,"text":delta_text,"reasoning":delta_reasoning,
                    "tool_progress": tool_progress}),
            )
            .await?;
            delta_text.clear();
            delta_reasoning.clear();
            if tool_dirty {
                last_tool_flush = Instant::now();
                tool_dirty = false;
            }
            last_flush = Instant::now();
            flushed = true;
        }
    }
    if !delta_text.is_empty() || !delta_reasoning.is_empty() || tool_dirty {
        if let Some(log) = diagnostics {
            log.phase("event_persistence");
        }
        let tool_progress = if tool_dirty {
            accumulator.tool_progress()
        } else {
            Value::Null
        };
        emit(
            root,
            task_id,
            "assistant/delta",
            json!({"turn":turn,"step":step,"task_node_id":frame,"text":delta_text,"reasoning":delta_reasoning,
                "tool_progress": tool_progress}),
        )
        .await?;
    }
    if let Some(log) = diagnostics {
        log.phase("stream_decode");
        log.decoded(&accumulator.stats());
    }
    let completed = accumulator.finish()?;
    let mut stats = completed.stats;
    stats["format"] = stats["response_format"].clone();
    stats["first_delta_after_headers_ms"] = stats["first_delta_ms"].clone();
    stats["stream_ms"] = stats["response_complete_ms"].clone();
    stats["terminated"] = json!(completed.terminated);
    if let Some(log) = diagnostics {
        log.decoded(&stats);
    }
    Ok((completed.message, stats))
}
