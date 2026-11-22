//! Incremental assembly of an OpenAI Chat Completions or Responses response,
//! and the Responses form of a Chat Completions request.
//!
//! The accumulator is transport-only: it never emits events or applies tool
//! calls. Worker and Organizer each consume its deltas through their own
//! output channels and act only on the completed message.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde_json::{Value, json};

/// Where an assistant message keeps the reasoning items a Responses provider
/// returned with it, so the next request can hand them back.
pub const REASONING_ITEMS: &str = "_reasoning_items";

/// Thinking text with the provider signature a Messages route replays. It is
/// transport replay data, never part of the conversation the model reads.
pub const THINKING_BLOCKS: &str = "_thinking_blocks";

/// The pure-JS + `return <<<` response protocol used by Context-as-Code v2.
///
/// - Normal execution steps emit raw JavaScript directly (no XML `<run>` tags,
///   no JSON tool calls).
/// - Final user-facing Markdown replies (which may freely contain ```` ```js ````
///   example code blocks with zero escaping) begin with `return <<<` followed by
///   the raw Markdown reply until EOF (optional trailing `>>>` closing line is
///   stripped cleanly).
/// - `CodeAndReply`: executable JS followed by `return <<<`:
///   - If running `source` makes 0 external host tool calls (pure in-memory
///     slicing/projection), the host pins `// =>` and immediately delivers `markdown`.
///   - If running `source` makes `> 0` external host tool calls (First-Trap
///     Truncation), the host keeps the real tool results in `EPHEMERAL_PEEK` and
///     discards the premature `markdown` so the model inspects real output next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunSubmission {
    Run { source: String },
    Reply { markdown: String },
    CodeAndReply { source: String, markdown: String },
    Rejected { code: &'static str, message: String },
}

/// Find the first `return <<<` boundary (either at line start or immediately
/// after a JS token boundary such as `;` or whitespace) and return
/// `(prefix_end_byte, reply_body_start_byte)`.
fn find_reply_heredoc(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find("return") {
        let start = search_from + rel;
        search_from = start + 6;
        // Must not be part of a longer identifier (e.g. `myreturn`) or inside a `//` line comment.
        if start > 0 {
            let prev = text[..start].chars().next_back().unwrap();
            if prev.is_ascii_alphanumeric() || prev == '_' || prev == '$' {
                continue;
            }
        }
        let line_start = text[..start].rfind('\n').map_or(0, |p| p + 1);
        if text[line_start..start].trim_start().starts_with("//") {
            continue;
        }
        let after_return = &text[start + 6..];
        let Some(after_space) = after_return.strip_prefix(|c: char| c == ' ' || c == '\t') else {
            continue;
        };
        let after_ws = after_space.trim_start_matches([' ', '\t']);
        let Some(after_marker) = after_ws.strip_prefix("<<<") else {
            continue;
        };
        let after_trailing = after_marker.trim_start_matches([' ', '\t']);
        if let Some(after_nl) = after_trailing
            .strip_prefix("\r\n")
            .or_else(|| after_trailing.strip_prefix('\n'))
        {
            let body_start = text.len() - after_nl.len();
            return Some((start, body_start));
        }
        if after_trailing.is_empty() && bytes.len() == text.len() {
            return Some((start, text.len()));
        }
    }
    None
}

fn clean_heredoc_markdown(raw: &str) -> String {
    let trimmed = raw.trim_end();
    // Strip an optional closing `>>>` or `<<<` line if the model echoed delimiters.
    for closer in [">>>", "<<<"] {
        if let Some(prefix) = trimmed.strip_suffix(closer) {
            let before = prefix.trim_end_matches([' ', '\t']);
            if before.is_empty() || before.ends_with('\n') {
                return before.trim_end().to_owned();
            }
        }
    }
    trimmed.to_owned()
}

/// Clean stray RL tool-tag tails (e.g. `</parameter>`) that models occasionally
/// emit right after a valid JS statement.
fn clean_js_source(raw: &str) -> String {
    let mut s = raw.trim().to_owned();
    for stray in ["</parameter>", "</invoke>", "</tool_call>", "</function>"] {
        if let Some(stripped) = s.strip_suffix(stray) {
            s = stripped.trim_end().to_owned();
        }
    }
    s
}

/// Strip a single outer Markdown code fence (```` ```js ... ``` ````) if the
/// model accidentally wrapped an executable JS step in fences with no prose outside.
fn strip_single_js_fence(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return None;
    }
    let first_nl = trimmed.find('\n')?;
    let lang = trimmed[3..first_nl].trim().to_ascii_lowercase();
    if !matches!(lang.as_str(), "" | "js" | "javascript" | "ts" | "typescript") {
        return None;
    }
    let body = &trimmed[first_nl + 1..];
    let stripped = body.strip_suffix("```")?;
    if stripped.contains("\n```") {
        return None;
    }
    let inner = stripped
        .strip_suffix("\r\n")
        .or_else(|| stripped.strip_suffix('\n'))
        .unwrap_or(stripped);
    Some(inner.to_owned())
}

/// Returns `true` when `text` already contains at least one syntactically
/// balanced executable JS statement/block (ending in `;` or `}`) and is NOT a
/// `return <<<` Markdown reply. Used to stop reasoning models cleanly when they
/// finish a JS step and resume reasoning inside the same HTTP stream expecting
/// a native tool output (otherwise they repeat `const ...` declarations and
/// fail compilation with `is already declared in this scope`).
pub fn is_complete_js_burst(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || find_reply_heredoc(trimmed).is_some() {
        return false;
    }
    let owned_inner;
    let source = if trimmed.starts_with("```") {
        let Some(inner) = strip_single_js_fence(trimmed) else {
            return false;
        };
        owned_inner = inner;
        owned_inner.as_str()
    } else {
        trimmed
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum LexMode {
        Code,
        SingleQuote,
        DoubleQuote,
        Template,
        LineComment,
        BlockComment,
    }

    let bytes = source.as_bytes();
    let mut i = 0usize;
    let mut mode = LexMode::Code;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    // Stack of brace depths at which `${` opened inside a template literal.
    let mut template_expr_braces: Vec<i32> = Vec::new();
    let mut last_code_byte: Option<u8> = None;

    while i < bytes.len() {
        let b = bytes[i];
        match mode {
            LexMode::Code => {
                if b == b'/' && i + 1 < bytes.len() {
                    if bytes[i + 1] == b'/' {
                        mode = LexMode::LineComment;
                        i += 2;
                        continue;
                    }
                    if bytes[i + 1] == b'*' {
                        mode = LexMode::BlockComment;
                        i += 2;
                        continue;
                    }
                }
                match b {
                    b'\'' => {
                        mode = LexMode::SingleQuote;
                        last_code_byte = Some(b);
                    }
                    b'"' => {
                        mode = LexMode::DoubleQuote;
                        last_code_byte = Some(b);
                    }
                    b'`' => {
                        mode = LexMode::Template;
                        last_code_byte = Some(b);
                    }
                    b'(' => {
                        paren += 1;
                        last_code_byte = Some(b);
                    }
                    b')' => {
                        paren -= 1;
                        if paren < 0 {
                            return false;
                        }
                        last_code_byte = Some(b);
                    }
                    b'[' => {
                        bracket += 1;
                        last_code_byte = Some(b);
                    }
                    b']' => {
                        bracket -= 1;
                        if bracket < 0 {
                            return false;
                        }
                        last_code_byte = Some(b);
                    }
                    b'{' => {
                        brace += 1;
                        last_code_byte = Some(b);
                    }
                    b'}' => {
                        if template_expr_braces.last().copied() == Some(brace) {
                            template_expr_braces.pop();
                            brace -= 1;
                            mode = LexMode::Template;
                        } else {
                            brace -= 1;
                            if brace < 0 {
                                return false;
                            }
                            last_code_byte = Some(b);
                        }
                    }
                    b if b.is_ascii_whitespace() => {}
                    _ => {
                        last_code_byte = Some(b);
                    }
                }
                i += 1;
            }
            LexMode::SingleQuote => {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'\'' {
                    mode = LexMode::Code;
                    last_code_byte = Some(b);
                } else if b == b'\n' {
                    return false;
                }
                i += 1;
            }
            LexMode::DoubleQuote => {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    mode = LexMode::Code;
                    last_code_byte = Some(b);
                } else if b == b'\n' {
                    return false;
                }
                i += 1;
            }
            LexMode::Template => {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'`' {
                    mode = LexMode::Code;
                    last_code_byte = Some(b);
                    i += 1;
                    continue;
                }
                if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    brace += 1;
                    template_expr_braces.push(brace);
                    mode = LexMode::Code;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            LexMode::LineComment => {
                if b == b'\n' {
                    mode = LexMode::Code;
                }
                i += 1;
            }
            LexMode::BlockComment => {
                if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    mode = LexMode::Code;
                    i += 2;
                    continue;
                }
                i += 1;
            }
        }
    }

    matches!(mode, LexMode::Code | LexMode::LineComment)
        && paren == 0
        && bracket == 0
        && brace == 0
        && template_expr_braces.is_empty()
        && matches!(last_code_byte, Some(b';' | b'}'))
}

/// Parse one completed model response under the pure-JS + `return <<<` protocol.
pub fn parse_run_submission(text: &str, finish_reason: Option<&str>) -> RunSubmission {
    let heredoc = find_reply_heredoc(text);
    let is_pure_reply = heredoc.is_some_and(|(start, _)| text[..start].trim().is_empty());
    if finish_reason == Some("length") {
        let code = if is_pure_reply {
            "REPLY_TRUNCATED"
        } else {
            "OUTPUT_TRUNCATED"
        };
        return RunSubmission::Rejected {
            code,
            message: "The model response hit the output token limit. Resubmit a shorter JS step or a concise `return <<<` reply.".into(),
        };
    }
    if text.trim().is_empty() {
        return RunSubmission::Rejected {
            code: "EMPTY_RESPONSE",
            message: "Empty response. Emit JavaScript statements to continue, or start with `return <<<` followed by your final Markdown reply.".into(),
        };
    }
    if let Some((prefix_end, body_start)) = heredoc {
        let prefix = &text[..prefix_end];
        let markdown = clean_heredoc_markdown(&text[body_start..]);
        if prefix.trim().is_empty() {
            if markdown.is_empty() {
                return RunSubmission::Rejected {
                    code: "EMPTY_REPLY",
                    message: "The `return <<<` reply body was empty. Provide your final Markdown reply after `return <<<`.".into(),
                };
            }
            return RunSubmission::Reply { markdown };
        }
        if let Some(stripped) = strip_single_js_fence(prefix) {
            let source = clean_js_source(&stripped);
            if markdown.is_empty() {
                return RunSubmission::Run { source };
            }
            return RunSubmission::CodeAndReply { source, markdown };
        }
        if prefix.trim_start().starts_with("```") {
            // Non-JS Markdown fences (e.g. ```rust / ```toml) must NEVER be fed to
            // the JS compiler where ``` parses as an empty tagged template literal!
            return RunSubmission::Rejected {
                code: "MARKDOWN_NOT_JS",
                message: "Non-JavaScript Markdown code fences (such as ```rust) cannot be executed as JS. To send a final Markdown answer with example code, start line 1 with `return <<<`.".into(),
            };
        }
        let source = clean_js_source(prefix);
        if markdown.is_empty() {
            return RunSubmission::Run { source };
        }
        return RunSubmission::CodeAndReply { source, markdown };
    }
    if let Some(stripped) = strip_single_js_fence(text) {
        return RunSubmission::Run {
            source: clean_js_source(&stripped),
        };
    }
    if text.trim_start().starts_with("```") {
        return RunSubmission::Rejected {
            code: "MARKDOWN_NOT_JS",
            message: "Non-JavaScript Markdown code fences (such as ```rust) cannot be executed as JS. To send a final Markdown answer with example code, start line 1 with `return <<<`.".into(),
        };
    }
    RunSubmission::Run {
        source: clean_js_source(text),
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(test)]
pub enum RunStreamEvent {
    RunProgress(String),
    ReplyDelta(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg(test)]
enum GateMode {
    #[default]
    Undecided,
    Code,
    Reply,
}

/// UTF-8-safe streaming gate for pure-JS + `return <<<` output.
///
/// Only responses beginning with `return <<<\n` stream user-visible Markdown
/// deltas (`ReplyDelta`). All executable JavaScript steps stay in `Code` mode
/// (`RunProgress`) so raw execution code never leaks into chat bubbles.
#[derive(Default)]
#[cfg(test)]
pub struct RunStreamGate {
    pending: String,
    mode: GateMode,
}

#[cfg(test)]
impl RunStreamGate {
    pub fn is_code(&self) -> bool {
        self.mode == GateMode::Code
    }

    pub fn push(&mut self, chunk: &str) -> Vec<RunStreamEvent> {
        if chunk.is_empty() {
            return Vec::new();
        }
        match self.mode {
            GateMode::Reply => return vec![RunStreamEvent::ReplyDelta(chunk.to_owned())],
            GateMode::Code => return vec![RunStreamEvent::RunProgress(chunk.to_owned())],
            GateMode::Undecided => {}
        }
        self.pending.push_str(chunk);
        let mut events = Vec::new();
        let trimmed_start_len = self.pending.len() - self.pending.trim_start().len();
        let candidate = &self.pending[trimmed_start_len..];
        if candidate.is_empty() {
            return events;
        }
        // Check whether `candidate` can still match `return[ \t]+<<<[ \t]*\r?\n`.
        if "return".starts_with(candidate) {
            return events;
        }
        let Some(after_return) = candidate.strip_prefix("return") else {
            self.mode = GateMode::Code;
            events.push(RunStreamEvent::RunProgress(std::mem::take(&mut self.pending)));
            return events;
        };
        let after_ws = after_return.trim_start_matches([' ', '\t']);
        if after_ws.is_empty() {
            // Seen `return` + spaces/tabs so far; wait for next token.
            if after_return.is_empty() {
                return events;
            }
            return events;
        }
        if after_return.len() == after_ws.len() {
            // Something like `returnValue` or `return(` — definitely JS code.
            self.mode = GateMode::Code;
            events.push(RunStreamEvent::RunProgress(std::mem::take(&mut self.pending)));
            return events;
        }
        if "<<<".starts_with(after_ws) {
            return events;
        }
        let Some(after_marker) = after_ws.strip_prefix("<<<") else {
            // e.g. `return "ok";` or `return { ... };` — JS return expression.
            self.mode = GateMode::Code;
            events.push(RunStreamEvent::RunProgress(std::mem::take(&mut self.pending)));
            return events;
        };
        let after_trailing = after_marker.trim_start_matches([' ', '\t']);
        if after_trailing.is_empty() || after_trailing == "\r" {
            // Wait for the newline terminating the `return <<<` header line.
            return events;
        }
        if let Some(after_nl) = after_trailing
            .strip_prefix("\r\n")
            .or_else(|| after_trailing.strip_prefix('\n'))
        {
            self.mode = GateMode::Reply;
            let body = after_nl.to_owned();
            self.pending.clear();
            if !body.is_empty() {
                events.push(RunStreamEvent::ReplyDelta(body));
            }
            return events;
        }
        // Non-whitespace on the same line after `<<<` -> treat as code.
        self.mode = GateMode::Code;
        events.push(RunStreamEvent::RunProgress(std::mem::take(&mut self.pending)));
        events
    }

    pub fn finish(&mut self) -> Vec<RunStreamEvent> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let event = match self.mode {
            GateMode::Reply => RunStreamEvent::ReplyDelta(std::mem::take(&mut self.pending)),
            GateMode::Code | GateMode::Undecided => {
                RunStreamEvent::RunProgress(std::mem::take(&mut self.pending))
            }
        };
        vec![event]
    }
}

/// The event kinds of an Anthropic Messages stream that carry the answer.
const MESSAGES_EVENTS: [&str; 6] = [
    "message_start",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message_delta",
    "message_stop",
];

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
        "include":["reasoning.encrypted_content"]});
    if chat.get("tools").is_some() || chat.get("parallel_tool_calls").is_some() {
        body["parallel_tool_calls"] = chat.get("parallel_tool_calls").cloned().unwrap_or(json!(false));
    }
    if let Some(text) = instructions {
        if chat["model"]
            .as_str()
            .is_some_and(|model| model.to_ascii_lowercase().starts_with("gpt-6.2"))
        {
            input.insert(
                0,
                json!({"type":"message","role":"developer","content":[{"type":"input_text","text":text}]}),
            );
            body["input"] = Value::Array(input);
        } else {
            body["instructions"] = json!(text);
        }
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
    if let Some(key) = chat.get("prompt_cache_key").filter(|key| !key.is_null()) {
        body["prompt_cache_key"] = key.clone();
    }
    if let Some(tier) = chat.get("service_tier").filter(|tier| !tier.is_null()) {
        body["service_tier"] = tier.clone();
    }
    body
}

/// Some Chat Completions templates accept system instructions only in the prefix.
/// This changes the wire projection, never the durable conversation order.
#[cfg(test)]
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

/// The Chat Completions finish reason a Messages stop reason corresponds to.
fn messages_finish(reason: &str) -> &'static str {
    match reason {
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        _ => "stop",
    }
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

/// What one block of a Messages stream carries, so a delta knows its kind.
#[derive(Clone, Copy, PartialEq)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
    Other,
}

/// Cumulative Messages usage; the provider reports cached input apart from the
/// prompt it read, so both are kept.
#[derive(Clone, Copy, Default)]
struct CacheUsage {
    input: u64,
    output: u64,
    read: u64,
    written: u64,
}

impl CacheUsage {
    /// The same usage in Chat Completions shape. A prompt is what the provider
    /// read: the uncached input plus what it took from or wrote to its cache.
    fn value(&self) -> Value {
        json!({
            "input_tokens": self.input,
            "output_tokens": self.output,
            "cache_read_input_tokens": self.read,
            "cache_creation_input_tokens": self.written,
            "prompt_tokens": self.input + self.read + self.written,
            "completion_tokens": self.output,
            "total_tokens": self.input + self.output + self.read + self.written,
            "prompt_tokens_details": {"cached_tokens": self.read},
        })
    }
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
    /// First complete JS code burst captured if a reasoning model resumes
    /// thinking mid-stream after already emitting executable JS statements.
    first_code_burst: Option<String>,
    reasoning: String,
    reasoning_chars: usize,
    tools: BTreeMap<usize, PartialTool>,
    /// Reasoning items of a Responses stream, handed back on the next request.
    items: Vec<Value>,
    /// Text of completed Responses message items, used when no delta carried it.
    item_text: String,
    /// Kind of each Messages content block, by block index.
    blocks: BTreeMap<usize, BlockKind>,
    /// Thinking text and signature per Messages block, replayed on the next request.
    thinking: BTreeMap<usize, (String, String)>,
    /// Tool blocks whose input arrives as `input_json_delta` fragments.
    streamed_input: BTreeSet<usize>,
    /// Cumulative Messages usage, including the cached input it reports.
    cache: CacheUsage,
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
            first_code_burst: None,
            reasoning: String::new(),
            reasoning_chars: 0,
            tools: BTreeMap::new(),
            items: Vec::new(),
            item_text: String::new(),
            blocks: BTreeMap::new(),
            thinking: BTreeMap::new(),
            streamed_input: BTreeSet::new(),
            cache: CacheUsage::default(),
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

    /// If a reasoning model resumed reasoning after emitting a syntactically
    /// complete JS burst, returns that clean first burst so the caller can
    /// stop reading the HTTP stream early and execute it without duplicate
    /// variable-declaration compile errors.
    #[cfg(test)]
    pub fn interrupted_code_burst(&self) -> Option<&str> {
        self.first_code_burst.as_deref()
    }

    fn note_reasoning_piece(&mut self, piece: &str) {
        if self.first_code_burst.is_none()
            && !piece.trim().is_empty()
            && !self.text.trim().is_empty()
            && is_complete_js_burst(&self.text)
        {
            self.first_code_burst = Some(self.text.clone());
        }
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
            if MESSAGES_EVENTS.contains(&kind) {
                return self.messages_event(kind, &event, delta);
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
            self.note_reasoning_piece(piece);
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
                self.note_reasoning_piece(piece);
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

    /// One event of an Anthropic Messages stream. The answer arrives as content
    /// blocks: text, thinking with the signature that replays it, and tool input
    /// fragments, with usage reported at the start and as it grows.
    fn messages_event(&mut self, kind: &str, event: &Value, delta: &mut Delta) -> anyhow::Result<()> {
        let block = event["index"].as_u64().unwrap_or(0) as usize;
        let mut model_delta = false;
        match kind {
            "message_start" => {
                if let Some(usage) = event["message"].get("usage").filter(|usage| !usage.is_null()) {
                    self.absorb_usage(usage)?;
                }
            }
            "content_block_start" => {
                let native = &event["content_block"];
                let kind = match native["type"].as_str() {
                    Some("text") => BlockKind::Text,
                    Some("thinking" | "redacted_thinking") => BlockKind::Thinking,
                    Some("tool_use") => BlockKind::ToolUse,
                    _ => BlockKind::Other,
                };
                self.blocks.insert(block, kind);
                match kind {
                    BlockKind::Text => {
                        if let Some(piece) = native["text"].as_str().filter(|piece| !piece.is_empty()) {
                            self.text.push_str(piece);
                            delta.text.push_str(piece);
                            model_delta = true;
                        }
                    }
                    BlockKind::Thinking => {
                        if let Some(signature) = native["signature"].as_str().filter(|signature| !signature.is_empty()) {
                            self.thinking.entry(block).or_default().1.push_str(signature);
                        }
                    }
                    BlockKind::ToolUse => {
                        let limit = self.limits.max_tool_argument_bytes;
                        let tool = self.tools.entry(block).or_default();
                        tool.id = native["id"].as_str().unwrap_or_default().to_owned();
                        tool.name = native["name"].as_str().unwrap_or_default().to_owned();
                        // A non-streaming provider sends the whole input here.
                        if let Some(input) = native.get("input").filter(|input| input.as_object().is_some_and(|object| !object.is_empty())) {
                            tool.arguments = input.to_string();
                            if tool.arguments.len() > limit {
                                return fail("ARGUMENTS_TOO_LARGE", format!("tool call arguments exceeded {limit} bytes"));
                            }
                        }
                        self.tool_delta(delta);
                        model_delta = true;
                    }
                    BlockKind::Other => {}
                }
            }
            "content_block_delta" => {
                let piece = &event["delta"];
                match piece["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(text) = piece["text"].as_str().filter(|text| !text.is_empty()) {
                            self.text.push_str(text);
                            delta.text.push_str(text);
                            model_delta = true;
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = piece["thinking"].as_str().filter(|text| !text.is_empty()) {
                            self.note_reasoning_piece(text);
                            self.reasoning_chars += text.chars().count();
                            if self.limits.retain_reasoning {
                                self.reasoning.push_str(text);
                                self.thinking.entry(block).or_default().0.push_str(text);
                            }
                            delta.reasoning.push_str(text);
                            model_delta = true;
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(signature) = piece["signature"].as_str().filter(|signature| !signature.is_empty()) {
                            self.thinking.entry(block).or_default().1.push_str(signature);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(fragment) = piece["partial_json"].as_str().filter(|fragment| !fragment.is_empty()) {
                            let limit = self.limits.max_tool_argument_bytes;
                            if self.streamed_input.insert(block) {
                                // Fragments are the whole input, not a continuation of a start block.
                                self.tools.entry(block).or_default().arguments.clear();
                            }
                            let tool = self.tools.entry(block).or_default();
                            tool.arguments.push_str(fragment);
                            if tool.arguments.len() > limit {
                                return fail("ARGUMENTS_TOO_LARGE", format!("tool call arguments exceeded {limit} bytes"));
                            }
                            self.tool_delta(delta);
                            model_delta = true;
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.finish_reason = json!(messages_finish(reason));
                }
                if let Some(usage) = event.get("usage").filter(|usage| !usage.is_null()) {
                    self.absorb_usage(usage)?;
                }
            }
            "message_stop" => self.received_done = true,
            _ => {}
        }
        if model_delta && self.first_delta_ms.is_none() {
            self.first_delta_ms = Some(self.elapsed_ms());
        }
        Ok(())
    }

    /// Messages reports cumulative usage: the prompt and its cache reads at the
    /// start, output tokens as the answer grows. Both stay cumulative here.
    fn absorb_usage(&mut self, usage: &Value) -> anyhow::Result<()> {
        for (key, slot) in [
            ("input_tokens", &mut self.cache.input),
            ("output_tokens", &mut self.cache.output),
            ("cache_read_input_tokens", &mut self.cache.read),
            ("cache_creation_input_tokens", &mut self.cache.written),
        ] {
            match usage.get(key) {
                None | Some(Value::Null) => {}
                Some(value) => match value.as_u64() {
                    Some(value) => *slot = value,
                    None => return fail("MESSAGES_USAGE", format!("Messages usage has an invalid {key}")),
                },
            }
        }
        self.usage = self.cache.value();
        Ok(())
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
        // Thinking with its signature goes back on the next request, in order.
        let thinking = self
            .thinking
            .values()
            .filter(|(text, _)| !text.is_empty())
            .map(|(text, signature)| {
                let mut block = json!({"thinking":text});
                if !signature.is_empty() {
                    block["signature"] = json!(signature);
                }
                block
            })
            .collect::<Vec<_>>();
        if !thinking.is_empty() {
            message[THINKING_BLOCKS] = json!(thinking);
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

    /// Finish early using the captured clean first JS code burst after a
    /// reasoning model resumed thinking mid-stream expecting a host output.
    #[cfg(test)]
    pub fn finish_truncated_code_burst(mut self) -> anyhow::Result<Completed> {
        let Some(burst) = self.first_code_burst.take() else {
            return self.finish();
        };
        let complete_ms = self.elapsed_ms();
        self.text = burst;
        self.item_text.clear();
        self.pending.clear();
        self.data = None;
        self.finish_reason = json!("stop");
        self.received_done = true;
        let message = self.message();
        let mut stats = self.stats();
        stats["response_complete_ms"] = json!(complete_ms);
        stats["truncated_on_reasoning_resume"] = json!(true);
        Ok(Completed {
            message,
            stats,
            terminated: true,
        })
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
#[cfg(test)]
mod tests {
    include!("parts/model_stream_tests.inc.rs");
}

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
