//! Incremental assembly of an OpenAI Chat Completions or Responses response,
//! and the Responses form of a Chat Completions request.
//!
//! The accumulator is transport-only: it never emits events or applies tool
//! calls. Worker and Organizer each consume its deltas through their own
//! output channels and act only on the completed message.
use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{Value, json};

/// Where an assistant message keeps the reasoning items a Responses provider
/// returned with it, so the next request can hand them back.
pub const REASONING_ITEMS: &str = "_reasoning_items";

/// The text of a Chat Completions content value: a string, the text parts of
/// an array, or the JSON of anything else.
fn plain(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

/// The same request on the Responses API, the way Codex talks to its models.
/// The first system message is the instructions; later ones become developer
/// messages where they stand. A message's name heads its text, since input
/// messages have no name. An assistant message replays the reasoning items it
/// came with before its text and calls, so the model goes on from its own
/// reasoning; nothing is stored upstream. Like Codex, it sets no output limit.
pub fn responses_request(chat: &Value) -> Value {
    let named = |message: &Value, text: String| match message["name"].as_str().filter(|name| !name.is_empty()) {
        Some(name) => format!("[{name}]\n{text}"),
        None => text,
    };
    let mut instructions = None;
    let mut input = Vec::new();
    for message in chat["messages"].as_array().into_iter().flatten() {
        match message["role"].as_str().unwrap_or("") {
            "system" | "developer" => {
                let text = named(message, plain(&message["content"]));
                if instructions.is_none() && input.is_empty() {
                    instructions = Some(text);
                } else {
                    input.push(json!({"type":"message","role":"developer","content":[{"type":"input_text","text":text}]}));
                }
            }
            "user" => {
                let parts = match &message["content"] {
                    Value::Array(items) => {
                        let mut parts = message["name"].as_str().filter(|name| !name.is_empty())
                            .map(|name| json!({"type":"input_text","text":format!("[{name}]")})).into_iter().collect::<Vec<_>>();
                        for part in items {
                            parts.push(match part["type"].as_str() {
                                Some("text") => json!({"type":"input_text","text":part["text"]}),
                                Some("image_url") => {
                                    let image = &part["image_url"];
                                    let mut input_image = json!({"type":"input_image","image_url":image.get("url").unwrap_or(image)});
                                    if let Some(detail) = image.get("detail") {
                                        input_image["detail"] = detail.clone();
                                    }
                                    input_image
                                }
                                _ => json!({"type":"input_text","text":part.to_string()}),
                            });
                        }
                        parts
                    }
                    other => vec![json!({"type":"input_text","text":named(message, plain(other))})],
                };
                input.push(json!({"type":"message","role":"user","content":parts}));
            }
            "assistant" => {
                input.extend(message[REASONING_ITEMS].as_array().into_iter().flatten().cloned());
                let text = plain(&message["content"]);
                if !text.is_empty() {
                    input.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    input.push(json!({"type":"function_call","call_id":call["id"],"name":call["function"]["name"],"arguments":call["function"]["arguments"]}));
                }
            }
            "tool" => input.push(json!({"type":"function_call_output","call_id":message["tool_call_id"],"output":plain(&message["content"])})),
            _ => {}
        }
    }
    let tools = chat["tools"].as_array().into_iter().flatten().map(|tool| {
        let function = &tool["function"];
        json!({"type":"function","name":function["name"],"description":function["description"],"parameters":function["parameters"],"strict":false})
    }).collect::<Vec<_>>();
    let mut body = json!({"model":chat["model"],"input":input,"stream":true,"store":false,
        "include":["reasoning.encrypted_content"],
        "parallel_tool_calls":chat.get("parallel_tool_calls").cloned().unwrap_or(json!(false))});
    if let Some(text) = instructions {
        body["instructions"] = json!(text);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    let mut reasoning = json!({"summary":"auto"});
    if let Some(effort) = chat.get("reasoning_effort").filter(|effort| !effort.is_null()) {
        reasoning["effort"] = effort.clone();
    }
    body["reasoning"] = reasoning;
    if let Some(tier) = chat.get("service_tier").filter(|tier| !tier.is_null()) {
        body["service_tier"] = tier.clone();
    }
    body
}

/// Some Chat Completions templates accept system instructions only in the prefix.
/// This changes the wire projection, never the durable conversation order.
pub fn prefix_system_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut system = Vec::new();
    let mut conversation = Vec::new();
    for message in messages {
        if message["role"] == "system" {
            system.push(message["content"].clone());
        } else {
            conversation.push(message);
        }
    }
    if system.is_empty() {
        return conversation;
    }
    let content = if system.iter().all(Value::is_string) {
        json!(
            system
                .iter()
                .map(|c| c.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n\n")
        )
    } else {
        json!(system.into_iter().flat_map(|c|match c {Value::Array(a)=>a,other=>vec![json!({"type":"text","text":other.as_str().map(str::to_owned).unwrap_or_else(||other.to_string())})]}).collect::<Vec<_>>())
    };
    conversation.insert(0, json!({"role":"system","content":content}));
    conversation
}

#[derive(Debug)]
pub struct StreamError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StreamError {}

fn fail<T>(code: &'static str, message: impl Into<String>) -> anyhow::Result<T> {
    Err(anyhow::Error::new(StreamError {
        code,
        message: message.into(),
    }))
}

#[cfg(test)]
pub fn stream_error_code(error: &anyhow::Error) -> Option<&'static str> {
    error.downcast_ref::<StreamError>().map(|error| error.code)
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Bytes received for one response, SSE framing included.
    pub max_total_bytes: usize,
    /// One unterminated SSE line, or the whole body of a plain JSON response.
    pub max_line_bytes: usize,
    /// Accumulated arguments of one tool call.
    pub max_tool_argument_bytes: usize,
    /// Reasoning text is counted either way; only the Worker transcript keeps it.
    pub retain_reasoning: bool,
}

impl Limits {
    pub const WORKER: Limits = Limits {
        max_total_bytes: 64 * 1024 * 1024,
        max_line_bytes: 16 * 1024 * 1024,
        max_tool_argument_bytes: 16 * 1024 * 1024,
        retain_reasoning: true,
    };
}

/// Text produced by one `push`, in arrival order per channel.
#[derive(Debug, Default)]
pub struct Delta {
    pub text: String,
    pub reasoning: String,
    pub tool_call: bool,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.reasoning.is_empty() && !self.tool_call
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Unknown,
    Sse,
    Json,
}

#[derive(Default)]
struct PartialTool {
    id: String,
    name: String,
    arguments: String,
}

pub struct Completed {
    pub message: Value,
    pub stats: Value,
    /// The provider signalled the end of the response (`[DONE]`, a
    /// `finish_reason`, or a complete JSON body). A bare EOF is not a terminal.
    pub terminated: bool,
}

pub struct ChatStream {
    limits: Limits,
    started: Instant,
    format: Format,
    pending: Vec<u8>,
    body: Vec<u8>,
    data: Option<String>,
    event_name: String,
    text: String,
    reasoning: String,
    reasoning_chars: usize,
    tools: BTreeMap<usize, PartialTool>,
    /// Reasoning items of a Responses stream, handed back on the next request.
    items: Vec<Value>,
    /// Text of completed Responses message items, used when no delta carried it.
    item_text: String,
    usage: Value,
    finish_reason: Value,
    received_done: bool,
    received_bytes: usize,
    chunks: usize,
    data_events: usize,
    keepalives: usize,
    first_chunk_ms: Option<u64>,
    first_delta_ms: Option<u64>,
    first_tool_delta_ms: Option<u64>,
}

impl ChatStream {
    /// `started` is the request start, so every recorded time shares one origin.
    pub fn new(limits: Limits, started: Instant) -> Self {
        Self {
            limits,
            started,
            format: Format::Unknown,
            pending: Vec::new(),
            body: Vec::new(),
            data: None,
            event_name: String::new(),
            text: String::new(),
            reasoning: String::new(),
            reasoning_chars: 0,
            tools: BTreeMap::new(),
            items: Vec::new(),
            item_text: String::new(),
            usage: Value::Null,
            finish_reason: Value::Null,
            received_done: false,
            received_bytes: 0,
            chunks: 0,
            data_events: 0,
            keepalives: 0,
            first_chunk_ms: None,
            first_delta_ms: None,
            first_tool_delta_ms: None,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn received_done(&self) -> bool {
        self.received_done
    }

    pub fn push(&mut self, chunk: &[u8]) -> anyhow::Result<Delta> {
        let mut delta = Delta::default();
        if chunk.is_empty() {
            return Ok(delta);
        }
        self.chunks += 1;
        if self.first_chunk_ms.is_none() {
            self.first_chunk_ms = Some(self.elapsed_ms());
        }
        self.received_bytes += chunk.len();
        if self.received_bytes > self.limits.max_total_bytes {
            return fail(
                "RESPONSE_TOO_LARGE",
                format!(
                    "model response exceeded {} bytes",
                    self.limits.max_total_bytes
                ),
            );
        }
        if self.format == Format::Unknown {
            match chunk.iter().find(|byte| !byte.is_ascii_whitespace()) {
                Some(b'{') => self.format = Format::Json,
                Some(_) => self.format = Format::Sse,
                None => {}
            }
        }
        if self.format == Format::Json {
            self.body.extend_from_slice(chunk);
            if self.body.len() > self.limits.max_line_bytes {
                return fail(
                    "RESPONSE_TOO_LARGE",
                    format!(
                        "JSON model response exceeded {} bytes",
                        self.limits.max_line_bytes
                    ),
                );
            }
            return Ok(delta);
        }
        self.pending.extend_from_slice(chunk);
        let mut consumed = 0;
        while let Some(offset) = self.pending[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let end = consumed + offset;
            let line = self.pending[consumed..end].to_vec();
            consumed = end + 1;
            self.line(&line, &mut delta)?;
        }
        self.pending.drain(..consumed);
        if self.pending.len() > self.limits.max_line_bytes {
            return fail(
                "LINE_TOO_LARGE",
                format!("an SSE line exceeded {} bytes", self.limits.max_line_bytes),
            );
        }
        Ok(delta)
    }

    fn line(&mut self, line: &[u8], delta: &mut Delta) -> anyhow::Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // A complete line never splits a UTF-8 sequence, so decoding is exact.
        let Ok(line) = std::str::from_utf8(line) else {
            return fail("INVALID_UTF8", "model stream contained invalid UTF-8");
        };
        if line.is_empty() {
            return self.dispatch(delta);
        }
        if line.starts_with(':') {
            self.keepalives += 1;
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                // Some providers omit the blank separator between events.
                let previous_complete = self.data.as_deref().is_some_and(|data| {
                    data == "[DONE]" || serde_json::from_str::<serde::de::IgnoredAny>(data).is_ok()
                });
                if previous_complete {
                    self.dispatch(delta)?;
                }
                match self.data.as_mut() {
                    Some(data) => {
                        data.push('\n');
                        data.push_str(value);
                    }
                    None => self.data = Some(value.to_owned()),
                }
            }
            "event" => self.event_name = value.to_owned(),
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, delta: &mut Delta) -> anyhow::Result<()> {
        let event_name = std::mem::take(&mut self.event_name);
        let Some(data) = self.data.take() else {
            return Ok(());
        };
        if data.trim().is_empty() {
            return Ok(());
        }
        self.data_events += 1;
        if data.trim() == "[DONE]" {
            self.received_done = true;
            return Ok(());
        }
        if self.received_done {
            return Ok(());
        }
        let event: Value = match serde_json::from_str(&data) {
            Ok(event) => event,
            Err(error) => {
                return fail(
                    "MALFORMED_EVENT",
                    format!("model stream event is not JSON: {error}"),
                );
            }
        };
        if let Some(error) = event.get("error").filter(|error| !error.is_null()) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            return fail("UPSTREAM_ERROR", message);
        }
        if event_name == "error" {
            return fail("UPSTREAM_ERROR", data);
        }
        // A Responses stream names each event's kind in its data.
        if let Some(kind) = event.get("type").and_then(Value::as_str) {
            if kind == "error" {
                let message = event["message"].as_str().map(str::to_owned).unwrap_or_else(|| event.to_string());
                return fail("UPSTREAM_ERROR", message);
            }
            if kind.starts_with("response.") {
                return self.response_event(kind, &event, delta);
            }
        }
        if let Some(usage) = event.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = usage.clone();
        }
        if let Some(reason) = event
            .pointer("/choices/0/finish_reason")
            .filter(|reason| !reason.is_null())
        {
            self.finish_reason = reason.clone();
        }
        let Some(choice_delta) = event.pointer("/choices/0/delta") else {
            return Ok(());
        };
        let mut model_delta = false;
        if let Some(piece) = choice_delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|piece| !piece.is_empty())
        {
            self.text.push_str(piece);
            delta.text.push_str(piece);
            model_delta = true;
        }
        let reasoning = choice_delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .or_else(|| choice_delta.get("reasoning").and_then(Value::as_str))
            .filter(|piece| !piece.is_empty());
        if let Some(piece) = reasoning {
            self.reasoning_chars += piece.chars().count();
            if self.limits.retain_reasoning {
                self.reasoning.push_str(piece);
            }
            delta.reasoning.push_str(piece);
            model_delta = true;
        }
        for call in choice_delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let entry = self.tools.entry(index).or_default();
            let mut grew = false;
            if let Some(id) = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                if entry.id != id {
                    entry.id = id.to_owned();
                    grew = true;
                }
            }
            if let Some(function) = call.get("function") {
                if let Some(name) = function
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                {
                    // Some providers repeat the full name on every fragment.
                    if entry.name != name {
                        entry.name.push_str(name);
                        grew = true;
                    }
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    entry.arguments.push_str(arguments);
                    grew |= !arguments.is_empty();
                    if entry.arguments.len() > self.limits.max_tool_argument_bytes {
                        return fail(
                            "ARGUMENTS_TOO_LARGE",
                            format!(
                                "tool call arguments exceeded {} bytes",
                                self.limits.max_tool_argument_bytes
                            ),
                        );
                    }
                }
            }
            delta.tool_call |= grew;
            model_delta |= grew;
            if grew && self.first_tool_delta_ms.is_none() {
                self.first_tool_delta_ms = Some(self.elapsed_ms());
            }
        }
        if model_delta && self.first_delta_ms.is_none() {
            self.first_delta_ms = Some(self.elapsed_ms());
        }
        Ok(())
    }

    fn response_event(&mut self, kind: &str, event: &Value, delta: &mut Delta) -> anyhow::Result<()> {
        let index = event["output_index"].as_u64().unwrap_or(0) as usize;
        let piece = event["delta"].as_str().filter(|piece| !piece.is_empty());
        let mut model_delta = false;
        match kind {
            "response.output_text.delta" => if let Some(piece) = piece {
                self.text.push_str(piece);
                delta.text.push_str(piece);
                model_delta = true;
            },
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => if let Some(piece) = piece {
                self.reasoning_chars += piece.chars().count();
                if self.limits.retain_reasoning {
                    self.reasoning.push_str(piece);
                }
                delta.reasoning.push_str(piece);
                model_delta = true;
            },
            // Parts of a summary are paragraphs, in the stream as in the final message.
            "response.reasoning_summary_part.added" if self.reasoning_chars > 0 => {
                if self.limits.retain_reasoning {
                    self.reasoning.push_str("\n\n");
                }
                delta.reasoning.push_str("\n\n");
            }
            "response.function_call_arguments.delta" => if let Some(piece) = piece {
                let limit = self.limits.max_tool_argument_bytes;
                let entry = self.tools.entry(index).or_default();
                entry.arguments.push_str(piece);
                if entry.arguments.len() > limit {
                    return fail("ARGUMENTS_TOO_LARGE", format!("tool call arguments exceeded {limit} bytes"));
                }
                self.tool_delta(delta);
                model_delta = true;
            },
            "response.output_item.added" | "response.output_item.done" => {
                model_delta |= self.output_item(index, &event["item"], kind == "response.output_item.done", delta)?;
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                let response = &event["response"];
                if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
                    self.usage = usage.clone();
                }
                if kind == "response.failed" {
                    let error = &response["error"];
                    return fail("UPSTREAM_ERROR", error["message"].as_str().map(str::to_owned).unwrap_or_else(|| error.to_string()));
                }
                // A response that streamed no items still lists them.
                if self.text.is_empty() && self.item_text.is_empty() && self.tools.is_empty() && self.items.is_empty() {
                    for (index, item) in response["output"].as_array().into_iter().flatten().enumerate() {
                        self.output_item(index, item, true, delta)?;
                    }
                }
                self.finish_reason = json!(self.response_finish(response));
                self.received_done = true;
            }
            _ => {}
        }
        if model_delta && self.first_delta_ms.is_none() {
            self.first_delta_ms = Some(self.elapsed_ms());
        }
        Ok(())
    }

    /// One output item of a Responses stream: a function call, a reasoning
    /// item kept for the next request, or a message whose text may also have
    /// arrived in deltas. True when it is generation progress.
    fn output_item(&mut self, index: usize, item: &Value, done: bool, delta: &mut Delta) -> anyhow::Result<bool> {
        match item["type"].as_str() {
            Some("function_call") => {
                let limit = self.limits.max_tool_argument_bytes;
                let entry = self.tools.entry(index).or_default();
                if let Some(id) = item["call_id"].as_str().filter(|id| !id.is_empty()) {
                    entry.id = id.to_owned();
                }
                if let Some(name) = item["name"].as_str().filter(|name| !name.is_empty()) {
                    entry.name = name.to_owned();
                }
                if let Some(arguments) = item["arguments"].as_str().filter(|_| done) {
                    entry.arguments = arguments.to_owned();
                }
                if entry.arguments.len() > limit {
                    return fail("ARGUMENTS_TOO_LARGE", format!("tool call arguments exceeded {limit} bytes"));
                }
                self.tool_delta(delta);
                Ok(true)
            }
            Some("reasoning") if done => {
                self.items.push(item.clone());
                Ok(false)
            }
            Some("message") if done => {
                for part in item["content"].as_array().into_iter().flatten() {
                    if let Some(text) = part["text"].as_str() {
                        self.item_text.push_str(text);
                    }
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn tool_delta(&mut self, delta: &mut Delta) {
        delta.tool_call = true;
        if self.first_tool_delta_ms.is_none() {
            self.first_tool_delta_ms = Some(self.elapsed_ms());
        }
    }

    /// The Chat Completions finish reason a Responses result corresponds to.
    fn response_finish(&self, response: &Value) -> &'static str {
        match response["incomplete_details"]["reason"].as_str() {
            Some("max_output_tokens") => "length",
            Some("content_filter") => "content_filter",
            _ if self.tools.is_empty() => "stop",
            _ => "tool_calls",
        }
    }

    /// The assistant message assembled so far, in Chat Completions form; a
    /// Responses provider's reasoning items ride along under REASONING_ITEMS.
    fn message(&self) -> Value {
        let mut tool_calls = Vec::new();
        for (index, tool) in &self.tools {
            if tool.name.is_empty() {
                continue;
            }
            let id = if tool.id.is_empty() {
                format!("call_{}_{index}", crate::agent_service::uuid_like())
            } else {
                tool.id.clone()
            };
            let arguments = if tool.arguments.is_empty() {
                "{}".to_owned()
            } else {
                tool.arguments.clone()
            };
            tool_calls.push(json!({"id":id,"type":"function","function":{"name":tool.name,"arguments":arguments}}));
        }
        let text = if self.text.is_empty() { &self.item_text } else { &self.text };
        let mut message = json!({"role":"assistant","content":if text.is_empty() {Value::Null} else {json!(text)}});
        let summary = || {
            self.items.iter().flat_map(|item| item["summary"].as_array().into_iter().flatten())
                .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n\n")
        };
        let reasoning = if !self.reasoning.is_empty() {
            self.reasoning.clone()
        } else if self.limits.retain_reasoning {
            summary()
        } else {
            String::new()
        };
        if !reasoning.is_empty() {
            message["reasoning_content"] = json!(reasoning);
        }
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }
        if !self.items.is_empty() {
            message[REASONING_ITEMS] = json!(self.items);
        }
        message
    }

    fn format_name(&self) -> &'static str {
        match self.format {
            Format::Json => "json",
            Format::Sse => "sse",
            Format::Unknown => "empty",
        }
    }

    pub fn tool_argument_bytes(&self) -> usize {
        self.tools.values().map(|tool| tool.arguments.len()).sum()
    }

    /// Progress metadata only: partial arguments may contain private source or secrets.
    pub fn tool_progress(&self) -> Value {
        self.tools.values().map(|tool| json!({
            "name": tool.name,
            "argument_bytes": tool.arguments.len()
        })).collect()
    }

    /// Statistics so far; also saved for a cancelled or timed-out request.
    pub fn stats(&self) -> Value {
        json!({"response_format":self.format_name(),"finish_reason":self.finish_reason,"received_done":self.received_done,
            "usage":self.usage,"received_bytes":self.received_bytes,"chunks":self.chunks,"data_events":self.data_events,
            "keepalives":self.keepalives,"first_chunk_ms":self.first_chunk_ms,"first_delta_ms":self.first_delta_ms,
            "first_tool_delta_ms":self.first_tool_delta_ms,"text_chars":self.text.chars().count(),
            "reasoning_chars":self.reasoning_chars,"tool_calls":self.tools.len(),"tool_argument_bytes":self.tool_argument_bytes()})
    }

    pub fn finish(mut self) -> anyhow::Result<Completed> {
        let complete_ms = self.elapsed_ms();
        match self.format {
            Format::Unknown => fail("INCOMPLETE_STREAM", "model response ended without any data"),
            Format::Json => {
                let body = std::mem::take(&mut self.body);
                let value: Value = match serde_json::from_slice(&body) {
                    Ok(value) => value,
                    Err(error) => {
                        return fail(
                            "INVALID_JSON_RESPONSE",
                            format!("model response is neither SSE nor complete JSON: {error}"),
                        );
                    }
                };
                if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| error.to_string());
                    return fail("UPSTREAM_ERROR", message);
                }
                // A complete Responses body lists its output items.
                if value["object"] == "response" || value["output"].is_array() {
                    let mut delta = Delta::default();
                    for (index, item) in value["output"].as_array().into_iter().flatten().enumerate() {
                        self.output_item(index, item, true, &mut delta)?;
                    }
                    self.usage = value.get("usage").cloned().unwrap_or(Value::Null);
                    self.finish_reason = json!(self.response_finish(&value));
                    let mut stats = self.stats();
                    stats["first_delta_ms"] = Value::Null;
                    stats["first_tool_delta_ms"] = Value::Null;
                    stats["response_complete_ms"] = json!(complete_ms);
                    return Ok(Completed { message: self.message(), stats, terminated: true });
                }
                let Some(message) = value.pointer("/choices/0/message").cloned() else {
                    return fail("NO_MESSAGE", "model response has no choices[0].message");
                };
                self.usage = value.get("usage").cloned().unwrap_or(Value::Null);
                self.finish_reason = value
                    .pointer("/choices/0/finish_reason")
                    .cloned()
                    .unwrap_or(Value::Null);
                let mut stats = self.stats();
                stats["first_delta_ms"] = Value::Null;
                stats["first_tool_delta_ms"] = Value::Null;
                stats["tool_calls"] = json!(message["tool_calls"].as_array().map_or(0, Vec::len));
                stats["response_complete_ms"] = json!(complete_ms);
                Ok(Completed {
                    message,
                    stats,
                    terminated: true,
                })
            }
            Format::Sse => {
                let mut delta = Delta::default();
                if !self.pending.is_empty() {
                    let line = std::mem::take(&mut self.pending);
                    self.line(&line, &mut delta)?;
                }
                self.dispatch(&mut delta)?;
                let message = self.message();
                let terminated = self.received_done || !self.finish_reason.is_null();
                let mut stats = self.stats();
                stats["response_complete_ms"] = json!(complete_ms);
                Ok(Completed {
                    message,
                    stats,
                    terminated,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_or_repeated_tool_metadata_is_not_generation_progress() {
        let mut stream = ChatStream::new(LIMITS, Instant::now());
        let first = stream
            .push(tool_event(0, Some("write_file"), "").as_bytes())
            .unwrap();
        assert!(first.tool_call);
        let repeated = stream
            .push(tool_event(0, Some("write_file"), "").as_bytes())
            .unwrap();
        assert!(repeated.is_empty());
        let arguments = stream
            .push(tool_event(0, None, "{\"content\":").as_bytes())
            .unwrap();
        assert!(arguments.tool_call);
        assert_eq!(stream.tool_progress(), json!([{"name":"write_file","argument_bytes":11}]));
        stream.push(tool_event(0, None, "\"你好\"").as_bytes()).unwrap();
        assert_eq!(stream.tool_progress(), json!([{"name":"write_file","argument_bytes":19}]));
    }

    fn responses_events(events: &[Value]) -> String {
        events.iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect()
    }

    #[test]
    fn a_responses_stream_is_the_same_message_and_keeps_its_reasoning_items() {
        let reasoning = json!({"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"Checking the files"}],"encrypted_content":"enc-1"});
        let stream = responses_events(&[
            json!({"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}),
            json!({"type":"response.reasoning_summary_part.added","output_index":0,"summary_index":0}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"Checking the files"}),
            json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"\"a.txt\"}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}),
            json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":10,"output_tokens":5}}}),
        ]);
        let mut chat = ChatStream::new(Limits::WORKER, Instant::now());
        let delta = chat.push(stream.as_bytes()).unwrap();
        assert!(delta.tool_call && delta.reasoning == "Checking the files");
        assert!(chat.received_done(), "response.completed ends the stream");
        let completed = chat.finish().unwrap();
        let message = completed.message;
        assert_eq!(message["tool_calls"], json!([{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]));
        assert_eq!(message["reasoning_content"], "Checking the files");
        assert_eq!(message[REASONING_ITEMS], json!([reasoning]), "the encrypted reasoning goes back on the next request");
        assert_eq!(completed.stats["finish_reason"], "tool_calls");
        assert_eq!(completed.stats["usage"]["input_tokens"], 10);
        assert!(completed.terminated);
    }

    #[test]
    fn summary_parts_stream_as_the_paragraphs_the_final_message_keeps() {
        let part = |index: u64, text: &str| [
            json!({"type":"response.reasoning_summary_part.added","output_index":0,"summary_index":index}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":text}),
        ];
        let events = [part(0, "**Reading the file**"), part(1, "**Writing the fix**")].concat();
        let mut chat = ChatStream::new(Limits::WORKER, Instant::now());
        let streamed = events
            .iter()
            .map(|event| chat.push(responses_events(std::slice::from_ref(event)).as_bytes()).unwrap().reasoning)
            .collect::<String>();
        assert_eq!(streamed, "**Reading the file**\n\n**Writing the fix**");
        chat.push(responses_events(&[json!({"type":"response.completed","response":{"status":"completed"}})]).as_bytes()).unwrap();
        assert_eq!(chat.finish().unwrap().message["reasoning_content"], streamed);
    }

    #[test]
    fn a_responses_answer_failure_and_cut_off_read_as_their_chat_counterparts() {
        let answer = responses_events(&[
            json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"Done"}),
            json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"."}),
            json!({"type":"response.completed","response":{"status":"completed"}}),
        ]);
        let completed = feed(&[answer.as_bytes()]).unwrap();
        assert_eq!(completed.message["content"], "Done.");
        assert_eq!(completed.stats["finish_reason"], "stop");
        let cut = responses_events(&[json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}})]);
        assert_eq!(feed(&[cut.as_bytes()]).unwrap().stats["finish_reason"], "length");
        let failed = responses_events(&[json!({"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"upstream broke"}}})]);
        let Err(error) = feed(&[failed.as_bytes()]) else { panic!("a failed response is an error") };
        assert_eq!(stream_error_code(&error), Some("UPSTREAM_ERROR"));
        assert!(error.to_string().contains("upstream broke"));
        // A provider that does not stream sends the whole response.
        let whole = json!({"object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Whole"}]}]});
        assert_eq!(feed(&[whole.to_string().as_bytes()]).unwrap().message["content"], "Whole");
    }

    #[test]
    fn a_chat_request_is_carried_over_to_the_responses_form() {
        let chat = json!({"model":"gpt-6-luna","stream":true,"parallel_tool_calls":false,"max_tokens":4096,"reasoning_effort":"medium",
            "messages":[
                {"role":"system","content":"You are the main AI."},
                {"role":"system","name":"program_tool_headers","content":"declare namespace io {}"},
                {"role":"user","content":"Add --version"},
                {"role":"assistant","content":null,REASONING_ITEMS:[{"type":"reasoning","id":"rs_1","encrypted_content":"enc-1"}],
                    "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]},
                {"role":"tool","tool_call_id":"call_1","content":"{\"content\":\"alpha\"}"},
                {"role":"user","name":"program_diagnostic","content":"{\"code\":\"PROGRAM_NOT_RUN\"}"},
                {"role":"user","content":[{"type":"text","text":"see"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AA","detail":"high"}}]},
            ],
            "tools":[{"type":"function","function":{"name":"read_file","description":"Read a file","parameters":{"type":"object"}}}]});
        let body = responses_request(&chat);
        assert_eq!(body["instructions"], "You are the main AI.");
        assert_eq!(body["store"], false);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["reasoning"], json!({"effort":"medium","summary":"auto"}));
        assert!(body.get("max_output_tokens").is_none() && body.get("messages").is_none());
        assert_eq!(body["tools"], json!([{"type":"function","name":"read_file","description":"Read a file","parameters":{"type":"object"},"strict":false}]));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0], json!({"type":"message","role":"developer","content":[{"type":"input_text","text":"[program_tool_headers]\ndeclare namespace io {}"}]}));
        assert_eq!(input[1]["content"][0]["text"], "Add --version");
        assert_eq!(input[2], json!({"type":"reasoning","id":"rs_1","encrypted_content":"enc-1"}), "reasoning comes back before the call it led to");
        assert_eq!(input[3], json!({"type":"function_call","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}));
        assert_eq!(input[4], json!({"type":"function_call_output","call_id":"call_1","output":"{\"content\":\"alpha\"}"}));
        assert_eq!(input[5]["content"][0]["text"], "[program_diagnostic]\n{\"code\":\"PROGRAM_NOT_RUN\"}");
        assert_eq!(input[6]["content"][1], json!({"type":"input_image","image_url":"data:image/png;base64,AA","detail":"high"}));
    }

    #[test]
    fn prefix_system_projection_preserves_tool_pairs_and_instruction_order() {
        let messages=serde_json::json!([
            {"role":"system","content":"policy"},{"role":"user","content":"request"},
            {"role":"assistant","tool_calls":[{"id":"a"}]},{"role":"tool","tool_call_id":"a","content":"result"},
            {"role":"system","content":"capacity"}
        ]).as_array().unwrap().clone();
        let projected = super::prefix_system_messages(messages.clone());
        assert_eq!(projected[0]["content"], "policy\n\ncapacity");
        assert_eq!(&projected[1..], &messages[1..4]);
    }
    use super::*;

    const LIMITS: Limits = Limits {
        max_total_bytes: 1 << 20,
        max_line_bytes: 1 << 16,
        max_tool_argument_bytes: 4096,
        retain_reasoning: false,
    };

    fn feed(chunks: &[&[u8]]) -> anyhow::Result<Completed> {
        let mut stream = ChatStream::new(LIMITS, Instant::now());
        for chunk in chunks {
            stream.push(chunk)?;
        }
        stream.finish()
    }

    fn tool_event(index: usize, name: Option<&str>, arguments: &str) -> String {
        let mut function = json!({"arguments":arguments});
        if let Some(name) = name {
            function["name"] = json!(name);
        }
        let call = json!({"index":index,"id":if name.is_some() {json!("call_1")} else {Value::Null},"function":function});
        format!(
            "data: {}\r\n\r\n",
            json!({"choices":[{"delta":{"tool_calls":[call]}}]})
        )
    }

    #[test]
    fn tool_arguments_split_across_frames_and_utf8_boundaries_are_exact() {
        let arguments =
            json!({"goal":"上传演示文稿并确认页码","reason":"服务已返回 200"}).to_string();
        let (head, tail) = arguments.split_at(arguments.find('文').unwrap());
        let stream = format!(
            ": keepalive\n\n{}{}{}data: {}\n\ndata: [DONE]\n\n",
            tool_event(0, Some("schedule_task"), ""),
            tool_event(0, None, head),
            tool_event(0, None, tail),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"total_tokens":42}})
        );
        let bytes = stream.as_bytes();
        let split = stream.find('文').unwrap() + 1;
        // Split inside a multibyte character and inside an SSE line.
        let completed = feed(&[&bytes[..7], &bytes[7..split], &bytes[split..]]).unwrap();
        assert!(completed.terminated);
        let call = &completed.message["tool_calls"][0];
        assert_eq!(call["function"]["name"], "schedule_task");
        assert_eq!(
            serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).unwrap(),
            serde_json::from_str::<Value>(&arguments).unwrap()
        );
        assert_eq!(completed.stats["response_format"], "sse");
        assert_eq!(completed.stats["received_done"], true);
        assert_eq!(completed.stats["finish_reason"], "tool_calls");
        assert_eq!(completed.stats["usage"]["total_tokens"], 42);
        assert_eq!(completed.stats["keepalives"], 1);
        assert!(completed.stats["first_tool_delta_ms"].is_u64());
    }

    #[test]
    fn eof_without_terminal_is_not_terminated() {
        let stream = tool_event(0, Some("schedule_task"), "{\"goal\":\"half");
        let completed = feed(&[stream.as_bytes()]).unwrap();
        assert!(!completed.terminated);
    }

    #[test]
    fn upstream_error_frame_and_malformed_events_fail() {
        let error = feed(&[b"data: {\"error\":{\"message\":\"overloaded\"}}\n\n"])
            .err()
            .unwrap();
        assert_eq!(stream_error_code(&error), Some("UPSTREAM_ERROR"));
        let error = feed(&[b"data: {not json\n\n"]).err().unwrap();
        assert_eq!(stream_error_code(&error), Some("MALFORMED_EVENT"));
        let error = feed(&[b"data: \xff\xfe\n\n"]).err().unwrap();
        assert_eq!(stream_error_code(&error), Some("INVALID_UTF8"));
    }

    #[test]
    fn plain_json_response_is_reported_as_json_without_stream_delta_times() {
        let body = json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"c","type":"function",
            "function":{"name":"finish_request","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}).to_string();
        let completed = feed(&[&body.as_bytes()[..10], &body.as_bytes()[10..]]).unwrap();
        assert!(completed.terminated);
        assert_eq!(completed.stats["response_format"], "json");
        assert!(completed.stats["first_delta_ms"].is_null());
        assert_eq!(
            completed.message["tool_calls"][0]["function"]["name"],
            "finish_request"
        );
    }

    #[test]
    fn events_without_blank_separators_and_oversized_arguments() {
        let stream = format!(
            "data: {}\ndata: {}\ndata: [DONE]\n",
            json!({"choices":[{"delta":{"content":"a"}}]}),
            json!({"choices":[{"delta":{"content":"b"},"finish_reason":"stop"}]})
        );
        let completed = feed(&[stream.as_bytes()]).unwrap();
        assert_eq!(completed.message["content"], "ab");
        assert!(completed.terminated);
        let huge = tool_event(0, Some("schedule_task"), &"x".repeat(5000));
        let error = feed(&[huge.as_bytes()]).err().unwrap();
        assert_eq!(stream_error_code(&error), Some("ARGUMENTS_TOO_LARGE"));
    }
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

pub fn tool_calls(assistant_message: &Value) -> Vec<ToolCall> {
    assistant_message
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let function = item.get("function")?;
            let name = function.get("name").and_then(|v| v.as_str())?.to_string();
            Some(ToolCall {
                call_id: item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                name,
                arguments: function
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}")
                    .to_string(),
            })
        })
        .collect()
}
