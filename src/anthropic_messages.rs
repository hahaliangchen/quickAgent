//! Native Anthropic Messages protocol for `anthropic-messages` providers.
//!
//! Grow assembles one Chat Completions request for every route. This module
//! projects that request onto the Messages wire — system prompt, thinking, tool
//! use and tool results, images — and adds the DeepSeek Messages extensions a
//! model declares in its catalog entry: session affinity, deferred tool loading
//! with in-history `tool_addition` blocks, and in-history system updates.
//! A model that declares nothing keeps the plain protocol, and every other
//! route keeps its own wire untouched.
//!
//! Reference: `deepseek-harness` `llm-deepseek` (`serialize.ts`,
//! `translate.ts`, `messages-api.ts`, `listing.ts`).
use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::agent_service::AgentServiceState;
use crate::model_stream::THINKING_BLOCKS;

/// The version header every Messages request carries.
pub const VERSION: &str = "2023-06-01";
/// Opt-in for `tool_addition` blocks that activate a declared tool later.
pub const TOOL_CHANGES_BETA: &str = "mid-conversation-tool-changes-2026-07-01";
/// Keeps a conversation on the same cache-warm backend shard.
pub const SESSION_HEADER: &str = "x-deepseek-harness-session-id";
/// Names this host's anonymous identity to the Messages service.
pub const USER_HEADER: &str = "x-deepseek-harness-user-id";
/// The only origin that receives the session affinity header.
const DEEPSEEK_HOST: &str = "api.deepseek.com";
/// The anonymous identity's file inside the app's configuration directory.
const USER_ID_FILE: &str = ".anonymous-user-id";
/// Hidden request fields this layer reads: the tools a Messages route with
/// `tool_update` declares once, and the ones earlier turns already activated.
pub const CATALOG: &str = "_tool_catalog";
/// The host message that records one activation of tools. It stays in the
/// window's history as it was written there, so every later request of that
/// window opens with the messages the earlier one sent.
pub const TOOLS_ADDED: &str = "tools_added";
/// On that message: the tool names it activates.
pub const ADDED_TOOLS: &str = "_tools_added";

/// The Messages extensions one model declares in its catalog entry, read from
/// the `api_capabilities.anthropic_messages` shape `/v1/models` publishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `tool_update: "addition-only"`: a fixed `tools` list, activated per turn.
    pub tool_update: bool,
    /// `system_prompt_update: "in-history"`: system text after the first turn.
    pub system_prompt_update: bool,
}

impl Capabilities {
    /// Read the declaration of one catalog entry, in either the listing shape
    /// (`api_capabilities.anthropic_messages`) or a DSH model entry
    /// (`messages.toolUpdate`). An unrecognized or absent mode is no support.
    pub fn from_metadata(metadata: &Value) -> Self {
        let named = |key: &str, camel: &str| -> Option<&Value> {
            metadata["api_capabilities"]["anthropic_messages"]
                .get(key)
                .or_else(|| metadata.get("messages").and_then(|messages| messages.get(camel).or_else(|| messages.get(key))))
                .or_else(|| metadata.get(camel))
                .or_else(|| metadata.get(key))
        };
        Self {
            tool_update: named("tool_update", "toolUpdate")
                .and_then(Value::as_str)
                .is_some_and(|mode| matches!(mode, "addition-only" | "in-history")),
            system_prompt_update: named("system_prompt_update", "systemPromptUpdate")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode == "in-history"),
        }
    }

    /// Whether the entry declares any Messages extension at all.
    pub fn declared(&self) -> bool {
        self.tool_update || self.system_prompt_update
    }
}

/// The Messages endpoint of a configured provider root, without duplicating an
/// explicit `/v1` path segment.
pub fn endpoint(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    let path = reqwest::Url::parse(base)
        .map(|url| url.path().to_owned())
        .unwrap_or_default();
    if path.trim_end_matches('/').ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

/// Whether the root is DeepSeek's own Messages endpoint, the only one that
/// accepts the harness session header.
pub fn on_deepseek(base: &str) -> bool {
    reqwest::Url::parse(base.trim())
        .ok()
        .and_then(|url| url.host_str().map(|host| host.eq_ignore_ascii_case(DEEPSEEK_HOST)))
        .unwrap_or(false)
}

/// Whether the body activates tools in conversation order.
pub fn tool_changes(body: &Value) -> bool {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "system")
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .any(|block| matches!(block["type"].as_str(), Some("tool_addition" | "tool_removal")))
}

/// The route's wire body for one Chat Completions shaped request. Other routes
/// get their own body back unchanged.
pub fn wire_body(state: &AgentServiceState, model: &str, chat: &Value) -> Value {
    let Some(route) = state
        .provider_routes
        .get(&state.provider_name)
        .filter(|route| route.messages)
    else {
        return chat.clone();
    };
    body(chat, route.messages_capabilities(model))
}

/// A request for a body already in the route's wire form. Responses and Chat
/// Completions authenticate with a bearer token; Messages with `x-api-key`
/// and a version, and only a Messages route receives the extensions.
pub fn send(
    state: &AgentServiceState,
    task: &str,
    scope: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    let route = state.provider_routes.get(&state.provider_name);
    if route.is_some_and(|route| route.responses) {
        return crate::model_transport::with_bearer(
            state.model_client.post(format!(
                "{}/responses", state.provider_url.trim().trim_end_matches('/')
            )),
            &state.api_key,
        ).json(body);
    }
    if !route.is_some_and(|route| route.messages) {
        return crate::model_transport::with_bearer(
            state.model_client.post(format!(
                "{}/chat/completions", state.provider_url.trim().trim_end_matches('/')
            )),
            &state.api_key,
        ).json(body);
    }
    let mut request = state
        .model_client
        .post(endpoint(&state.provider_url))
        .header("anthropic-version", VERSION)
        .header("accept", "text/event-stream");
    if !state.api_key.is_empty() {
        request = request.header("x-api-key", state.api_key.as_str());
    }
    if tool_changes(body) {
        request = request.header("anthropic-beta", TOOL_CHANGES_BETA);
    }
    if on_deepseek(&state.provider_url) {
        request = request
            .header(
                SESSION_HEADER,
                crate::model_transport::affinity(task, scope),
            )
            .header(USER_HEADER, anonymous_user_id(state));
    }
    request.json(body)
}

/// The host's anonymous identity, kept beside the configuration that names the
/// route.
fn anonymous_user_id(state: &AgentServiceState) -> String {
    let dir = state
        .config_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(crate::ai_proxy::config_dir);
    anonymous_user_id_in(&dir)
}

/// The anonymous id kept in `dir`: drawn once, then reused by every request and
/// every restart. It is never derived from a user name, an email address or a
/// key, and one process reads the file once.
pub fn anonymous_user_id_in(dir: &std::path::Path) -> String {
    static IDS: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, String>> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());
    if let Some(id) = IDS.lock().unwrap().get(dir) {
        return id.clone();
    }
    let file = dir.join(USER_ID_FILE);
    let id = persisted_id(&file).unwrap_or_else(|| {
        let drawn = new_anonymous_id();
        // An exclusive create settles a concurrent first launch: the loser
        // reads the winner's id. A directory that cannot be written still
        // reports this run's id, so the header never blocks a request.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&file) {
            Ok(mut handle) => {
                use std::io::Write;
                let _ = handle.write_all(format!("{drawn}\n").as_bytes());
                drawn
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                persisted_id(&file).unwrap_or(drawn)
            }
            Err(_) => {
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(&file, format!("{drawn}\n"));
                drawn
            }
        }
    });
    IDS.lock().unwrap().insert(dir.to_path_buf(), id.clone());
    id
}

/// The id the file holds, when it holds a complete one.
fn persisted_id(file: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let value = text.trim();
    is_anonymous_id(value).then(|| value.to_owned())
}

/// Whether the text is a UUID: eight, four, four, four and twelve hex digits.
fn is_anonymous_id(text: &str) -> bool {
    let groups = text.split('-').map(str::len).collect::<Vec<_>>();
    groups == [8, 4, 4, 4, 12] && text.chars().all(|character| character == '-' || character.is_ascii_hexdigit())
}

/// A fresh random id, in the UUID shape the harness uses.
fn new_anonymous_id() -> String {
    let hex = crate::agent_service::uuid_like();
    match hex.len() {
        32 => format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        ),
        _ => hex,
    }
}

/// The Messages body of one Chat Completions shaped request.
pub fn body(chat: &Value, capabilities: Capabilities) -> Value {
    let deferred = capabilities.tool_update;
    let mut system: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    // The conversation's first activation is its baseline: those tools are
    // declared ready to call, as DSH declares the tools a session starts with.
    // DeepSeek refuses a request whose tools are all deferred (2026-11-13:
    // "at least one tool must have defer_loading=false").
    let baseline = baseline_tools(chat);
    let mut seen_baseline = false;
    for message in chat["messages"].as_array().into_iter().flatten() {
        // A later activation becomes the system block that adds its tools,
        // where the history holds it: after the user or tool-result turn it
        // followed, and before the next assistant turn.
        if let Some(names) = message.get(ADDED_TOOLS).and_then(Value::as_array) {
            if !seen_baseline {
                seen_baseline = true;
                continue;
            }
            if deferred {
                let blocks = names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(tool_addition)
                    .collect::<Vec<_>>();
                if !blocks.is_empty() {
                    turn(&mut messages, "system", blocks);
                }
            }
            continue;
        }
        match message["role"].as_str().unwrap_or("") {
            "system" | "developer" => {
                let text = plain(&message["content"]);
                if text.is_empty() {
                    continue;
                }
                if messages.is_empty() {
                    system.push(text);
                } else if capabilities.system_prompt_update {
                    turn(&mut messages, "system", vec![json!({"type":"text","text":text})]);
                } else {
                    // Without the extension the provider reads one prompt, so a
                    // later update stays part of it.
                    system.push(text);
                }
            }
            "user" => {
                let blocks = input_blocks(&message["content"]);
                if !blocks.is_empty() {
                    turn(&mut messages, "user", blocks);
                }
            }
            "assistant" => {
                let mut blocks = Vec::new();
                for block in message[THINKING_BLOCKS].as_array().into_iter().flatten() {
                    let text = block["thinking"].as_str().unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    let mut thinking = json!({"type":"thinking","thinking":text});
                    if let Some(signature) = block["signature"].as_str().filter(|signature| !signature.is_empty()) {
                        thinking["signature"] = json!(signature);
                    }
                    blocks.push(thinking);
                }
                let text = plain(&message["content"]);
                if !text.is_empty() {
                    blocks.push(json!({"type":"text","text":text}));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    blocks.push(json!({"type":"tool_use","id":call["id"],
                        "name":call["function"]["name"],"input":tool_input(&call["function"]["arguments"])}));
                }
                if !blocks.is_empty() {
                    turn(&mut messages, "assistant", blocks);
                }
            }
            "tool" => {
                let id = message["tool_call_id"].as_str().unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                turn(&mut messages, "user", vec![json!({"type":"tool_result",
                    "tool_use_id":id,"content":input_blocks(&message["content"])})]);
            }
            _ => {}
        }
    }
    // A turn's results answer the calls before it, whatever arrived beside them.
    for message in &mut messages {
        if message["role"] != "user" {
            continue;
        }
        let blocks = message["content"].as_array().cloned().unwrap_or_default();
        let (results, rest): (Vec<Value>, Vec<Value>) = blocks
            .into_iter()
            .partition(|block| block["type"] == "tool_result");
        if !results.is_empty() {
            message["content"] = json!(results.into_iter().chain(rest).collect::<Vec<_>>());
        }
    }
    // One declaration set for the whole conversation: which of its tools this
    // step may call is the activation blocks' business, so the declarations
    // stay byte-identical for every scope and every turn.
    let tools = if deferred {
        catalog_tools(chat, &baseline)
    } else {
        chat["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .map(wire_tool)
            .collect()
    };
    let (thinking, output_config) = thinking(chat["reasoning_effort"].as_str());
    let mut body = json!({
        "model": chat["model"],
        "stream": true,
        "max_tokens": chat["max_tokens"].as_u64().unwrap_or(8192),
        "messages": messages,
        "thinking": thinking,
    });
    if let Some(output_config) = output_config {
        body["output_config"] = output_config;
    }
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if let Some(temperature) = chat["temperature"].as_f64() {
        body["temperature"] = json!(temperature);
    }
    let stops = match &chat["stop"] {
        Value::String(stop) => vec![stop.clone()],
        Value::Array(items) => items.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    if !stops.is_empty() {
        body["stop_sequences"] = json!(stops);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

/// The tools of the conversation's first recorded activation.
fn baseline_tools(chat: &Value) -> BTreeSet<String> {
    chat["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|message| message.get(ADDED_TOOLS).and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The fixed catalog a `tool_update` route declares, in one order for every
/// request of the conversation so the cached prefix never changes: its
/// baseline tools ready to call, every other tool deferred until a
/// `tool_addition` block activates it.
fn catalog_tools(chat: &Value, baseline: &BTreeSet<String>) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for definition in chat[CATALOG]
        .as_array()
        .into_iter()
        .flatten()
        .chain(chat["tools"].as_array().into_iter().flatten())
    {
        if let Some(name) = definition["function"]["name"].as_str()
            && seen.insert(name.to_owned())
        {
            ordered.push(definition);
        }
    }
    ordered
        .into_iter()
        .map(|definition| {
            let mut tool = wire_tool(definition);
            if !definition["function"]["name"].as_str().is_some_and(|name| baseline.contains(name)) {
                tool["defer_loading"] = json!(true);
            }
            tool
        })
        .collect()
}

/// One declared tool on the Messages wire.
fn wire_tool(definition: &Value) -> Value {
    let function = &definition["function"];
    let schema = function
        .get("parameters")
        .filter(|schema| schema.is_object())
        .cloned()
        .unwrap_or_else(|| json!({"type":"object","properties":{}}));
    json!({"name":function["name"],"description":function["description"].as_str().unwrap_or_default(),
        "input_schema":schema})
}

/// Thinking settings for the effort a Chat Completions body names. Thinking is
/// on unless the effort is explicitly `off`: an unset effort is high, as
/// `serialize.ts` resolves it.
fn thinking(effort: Option<&str>) -> (Value, Option<Value>) {
    match effort {
        Some("off") => (json!({"type":"disabled"}), None),
        Some("low") => (json!({"type":"enabled"}), Some(json!({"effort":"low"}))),
        Some("max" | "xhigh") => (json!({"type":"enabled"}), Some(json!({"effort":"max"}))),
        _ => (json!({"type":"enabled"}), Some(json!({"effort":"high"}))),
    }
}

/// One activation of a declared tool.
fn tool_addition(name: &str) -> Value {
    json!({"type":"tool_addition","tool":{"type":"tool_reference","name":name}})
}

/// The host message that records one activation, as the window's history keeps
/// it: the names travel in a host field, and the request that carries the
/// message projects them into `tool_addition` blocks instead of this text.
pub fn tools_added(names: Vec<String>) -> Value {
    let mut message = json!({"role":"user","name":TOOLS_ADDED,"content":format!(
        "The host activated these tools for this conversation: {}.",
        names.join(", ")
    )});
    message[ADDED_TOOLS] = json!(names);
    message
}

/// The tools the conversation already activated, read from the messages still
/// in it: a tool whose activation left with folded history is announced again.
pub fn activated_tools(messages: &[Value]) -> BTreeSet<String> {
    messages
        .iter()
        .filter_map(|message| message.get(ADDED_TOOLS).and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Appends blocks to the turn they belong to; consecutive turns of one role are
/// one turn on the wire.
fn turn(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last["role"] == role
        && role != "system"
    {
        if let Some(content) = last["content"].as_array_mut() {
            content.extend(blocks);
            return;
        }
    }
    messages.push(json!({"role":role,"content":blocks}));
}

/// Historical arguments that are not a JSON object read as empty input; the
/// saved arguments stay unchanged.
fn tool_input(arguments: &Value) -> Value {
    arguments
        .as_str()
        .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn plain(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// User and tool-result content, where only text and images are representable.
fn input_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::Null => Vec::new(),
        Value::String(text) if text.is_empty() => Vec::new(),
        Value::String(text) => vec![json!({"type":"text","text":text})],
        Value::Array(parts) => parts.iter().filter_map(part_block).collect(),
        other => vec![json!({"type":"text","text":other.to_string()})],
    }
}

fn part_block(part: &Value) -> Option<Value> {
    match part["type"].as_str() {
        Some("text") => part["text"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(|text| json!({"type":"text","text":text})),
        Some("image_url") => {
            let image = &part["image_url"];
            let url = image["url"].as_str().or_else(|| image.as_str()).unwrap_or_default();
            Some(image_block(url))
        }
        // A route that cannot show an image keeps its handle as text, so the
        // model still knows what the harness attached.
        _ => part
            .as_str()
            .filter(|text| !text.is_empty())
            .map(|text| json!({"type":"text","text":text})),
    }
}

/// Inline image bytes travel as base64; a URL this host cannot fetch stays a
/// text handle rather than a broken block.
fn image_block(url: &str) -> Value {
    let inline = url
        .strip_prefix("data:")
        .and_then(|data| data.split_once(','))
        .and_then(|(header, payload)| header.strip_suffix(";base64").map(|media_type| (media_type, payload)));
    match inline {
        Some((media_type, data)) => json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}}),
        None => json!({"type":"text","text":url}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keyless_openai_models_send_no_authorization_header() {
        use axum::{Json, Router, http::HeaderMap, routing::post};
        async fn reply(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
            assert!(!headers.contains_key("authorization"));
            assert_eq!(body["model"], "customer-qwen3-vl-32b");
            Json(json!({"choices":[{"message":{"role":"assistant","content":"ready"},"finish_reason":"stop"}]}))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/chat/completions", post(reply))).await.unwrap()
        });
        let root = TestRoot::new();
        let mut state = crate::agent_service::tests::test_state(root.path(), address);
        state.provider_name = "qwen-local".into();
        state.provider_url = format!("http://{address}/v1");
        state.api_key.clear();
        let body = json!({"model":"customer-qwen3-vl-32b","stream":false,"messages":[{"role":"user","content":"hello"}]});
        let result = send(&state, "local-test", "", &body).send().await.unwrap();
        assert!(result.status().is_success());
        assert_eq!(result.json::<Value>().await.unwrap()["choices"][0]["message"]["content"], "ready");
        state.api_key = "configured-key".into();
        let request = send(&state, "local-test", "", &body).build().unwrap();
        assert_eq!(request.headers()["authorization"], "Bearer configured-key");
        server.abort();
    }

    fn function(name: &str) -> Value {
        json!({"type":"function","function":{"name":name,"description":format!("{name} description"),
            "parameters":{"type":"object","properties":{}}}})
    }

    fn chat(tools: &[&str], catalog: &[&str], activated: &[&str]) -> Value {
        let mut body = json!({"model":"deepseek-flash","stream":true,"max_tokens":1024,"parallel_tool_calls":false,
            "reasoning_effort":"high",
            "messages":[
                {"role":"system","content":"You are the main AI."},
                {"role":"system","name":"program_tool_headers","content":"declare namespace io {}"},
                {"role":"user","content":"Read a.txt"}]});
        body["tools"] = json!(tools.iter().map(|name| function(name)).collect::<Vec<_>>());
        body[CATALOG] = json!(catalog.iter().map(|name| function(name)).collect::<Vec<_>>());
        // A window records an activation as a host message where it happened;
        // `activated` stands for what its history holds when it is listed.
        if !activated.is_empty() {
            let names = activated.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
            body["messages"].as_array_mut().unwrap().push(tools_added(names));
        }
        body
    }

    /// The tools a wire body activates, in conversation order.
    fn additions(wire: &Value) -> Vec<String> {
        wire["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "system")
            .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
            .filter(|block| block["type"] == "tool_addition")
            .filter_map(|block| block["tool"]["name"].as_str().map(str::to_owned))
            .collect()
    }

    fn tool_names(tools: &Value) -> Vec<(String, bool)> {
        tools
            .as_array()
            .into_iter()
            .flatten()
            .map(|tool| {
                (
                    tool["name"].as_str().unwrap_or_default().to_owned(),
                    tool["defer_loading"] == true,
                )
            })
            .collect()
    }

    #[test]
    fn a_messages_body_carries_one_prompt_thinking_tool_use_and_results() {
        let capabilities = Capabilities::default();
        let mut body = chat(&["read_file"], &[], &[]);
        let wire = body_(&body, capabilities);
        assert_eq!(wire["system"], "You are the main AI.\n\ndeclare namespace io {}");
        assert_eq!(wire["thinking"], json!({"type":"enabled"}));
        assert_eq!(wire["output_config"], json!({"effort":"high"}));
        assert_eq!(wire["messages"][0], json!({"role":"user","content":[{"type":"text","text":"Read a.txt"}]}));
        assert_eq!(wire["tools"][0]["name"], "read_file");
        assert!(wire["tools"][0].get("defer_loading").is_none());

        // A reply with thinking, text and a call replays in block order, and the
        // next turn carries the result it produced.
        let mut reply = json!({"role":"assistant","content":"Reading.","reasoning_content":"Plan",
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]});
        reply[THINKING_BLOCKS] = json!([{"thinking":"Plan","signature":"sig-1"}]);
        body["messages"] = json!([
            {"role":"system","content":"You are the main AI."},
            {"role":"user","content":"Read a.txt"},
            reply,
            {"role":"tool","tool_call_id":"call_1","content":"hello"},
            {"role":"user","content":"And b.txt?"}]);
        let wire = body_(&body, capabilities);
        assert_eq!(wire["messages"][1]["content"], json!([
            {"type":"thinking","thinking":"Plan","signature":"sig-1"},
            {"type":"text","text":"Reading."},
            {"type":"tool_use","id":"call_1","name":"read_file","input":{"path":"a.txt"}}]));
        assert_eq!(wire["messages"][2]["content"], json!([
            {"type":"tool_result","tool_use_id":"call_1","content":[{"type":"text","text":"hello"}]},
            {"type":"text","text":"And b.txt?"}]));

        // Off is the disabled block with no effort field.
        let mut off = chat(&["read_file"], &[], &[]);
        off["reasoning_effort"] = json!("off");
        let wire = body_(&off, capabilities);
        assert_eq!(wire["thinking"], json!({"type":"disabled"}));
        assert!(wire.get("output_config").is_none());
    }

    /// The conversion under test, named so the fixtures above stay readable.
    fn body_(chat: &Value, capabilities: Capabilities) -> Value {
        body(chat, capabilities)
    }

    /// DeepSeek refuses a request whose tools are all deferred (2026-11-13,
    /// real endpoint): as in DSH, the tools a conversation starts with are
    /// declared ready, the rest of the catalog deferred, and only tools added
    /// later enter as `tool_addition` blocks where the history recorded them.
    #[test]
    fn deferred_tool_updates_keep_the_baseline_ready_and_add_later_tools_in_history() {
        let capabilities = Capabilities { tool_update: true, system_prompt_update: true };
        let catalog = ["read_file", "write_file", "shell"];
        let reading = body(&chat(&["read_file"], &catalog, &["read_file"]), capabilities);
        assert_eq!(
            tool_names(&reading["tools"]),
            vec![
                ("read_file".to_owned(), false),
                ("write_file".to_owned(), true),
                ("shell".to_owned(), true),
            ]
        );
        assert!(additions(&reading).is_empty(), "the baseline needs no addition");
        assert!(!tool_changes(&reading));
        // A tool added later enters as one more message after what was sent:
        // the declarations and the earlier messages stay exactly as they were.
        let mut grown = chat(&["read_file", "write_file"], &catalog, &["read_file"]);
        grown["messages"].as_array_mut().unwrap().push(tools_added(vec!["write_file".to_owned()]));
        let later = body(&grown, capabilities);
        assert_eq!(later["tools"], reading["tools"]);
        assert_eq!(additions(&later), ["write_file"]);
        assert_eq!(
            later["messages"].as_array().unwrap().last().unwrap(),
            &json!({"role":"system","content":[{"type":"tool_addition","tool":{"type":"tool_reference","name":"write_file"}}]})
        );
        let (earlier, extended) = (reading["messages"].as_array().unwrap(), later["messages"].as_array().unwrap());
        assert_eq!(&extended[..earlier.len()], &earlier[..], "what was sent before is sent again unchanged");
        assert!(tool_changes(&later));
        assert!(!tool_changes(&body(&chat(&["read_file"], &catalog, &[]), Capabilities::default())));
    }

    #[test]
    fn models_without_tool_update_send_only_their_scope_tools() {
        let plain = body(&chat(&["read_file"], &["read_file", "shell"], &[]), Capabilities::default());
        assert_eq!(tool_names(&plain["tools"]), vec![("read_file".to_owned(), false)]);
        let wire = plain.to_string();
        assert!(!wire.contains("defer_loading"));
        assert!(!wire.contains("tool_addition"));
        assert!(!tool_changes(&plain));
        // An activation recorded for a Messages route never reaches a model
        // that does not declare the extension: its tools are declared outright.
        let kept = body(&chat(&["read_file"], &["read_file", "shell"], &["read_file"]), Capabilities::default());
        assert!(additions(&kept).is_empty());
        assert!(!kept.to_string().contains(ADDED_TOOLS));
        assert!(!tool_changes(&kept));
    }

    /// Thinking is on unless the effort says otherwise, and an unset effort is
    /// the harness default of high.
    #[test]
    fn an_unset_effort_thinks_high_and_only_off_disables_it() {
        let mut unset = chat(&["read_file"], &[], &[]);
        unset.as_object_mut().unwrap().remove("reasoning_effort");
        for wire in [body_(&unset, Capabilities::default()), body_(&chat(&["read_file"], &[], &[]), Capabilities::default())] {
            assert_eq!(wire["thinking"], json!({"type":"enabled"}));
            assert_eq!(wire["output_config"], json!({"effort":"high"}));
        }
        for (effort, expected) in [("low", "low"), ("medium", "high"), ("high", "high"), ("max", "max"), ("xhigh", "max")] {
            let mut named = unset.clone();
            named["reasoning_effort"] = json!(effort);
            let wire = body_(&named, Capabilities::default());
            assert_eq!(wire["thinking"], json!({"type":"enabled"}), "{effort}");
            assert_eq!(wire["output_config"], json!({"effort":expected}), "{effort}");
        }
        // Only an explicit off turns thinking off.
        let mut off = unset.clone();
        off["reasoning_effort"] = json!("off");
        let wire = body_(&off, Capabilities::default());
        assert_eq!(wire["thinking"], json!({"type":"disabled"}));
        assert!(wire.get("output_config").is_none());
    }

    #[test]
    fn capability_bits_come_from_the_declared_catalog_entry() {
        let listing = json!({"api_capabilities":{"anthropic_messages":{"tool_update":"addition-only","system_prompt_update":"in-history"}}});
        assert_eq!(
            Capabilities::from_metadata(&listing),
            Capabilities { tool_update: true, system_prompt_update: true }
        );
        let dsh = json!({"messages":{"toolUpdate":"addition-only","systemPromptUpdate":"in-history"}});
        assert_eq!(Capabilities::from_metadata(&dsh), Capabilities { tool_update: true, system_prompt_update: true });
        // A model that declares no extension keeps the plain protocol.
        for undeclared in [
            json!({"api_capabilities":{"anthropic_messages":{"tool_update":"leading-only"}}}),
            json!({"messages":{"tool_update":"leading-only"}}),
            json!({"name":"deepseek-v4-pro"}),
        ] {
            assert_eq!(Capabilities::from_metadata(&undeclared), Capabilities::default());
            assert!(!Capabilities::from_metadata(&undeclared).declared());
        }
    }

    #[test]
    fn the_endpoint_and_session_header_belong_to_the_deepseek_messages_route() {
        assert_eq!(endpoint("https://api.deepseek.com/anthropic"), "https://api.deepseek.com/anthropic/v1/messages");
        assert_eq!(endpoint("https://api.deepseek.com/anthropic/"), "https://api.deepseek.com/anthropic/v1/messages");
        assert_eq!(endpoint("https://api.deepseek.com/anthropic/v1"), "https://api.deepseek.com/anthropic/v1/messages");
        assert_eq!(endpoint("http://127.0.0.1:8080/v1"), "http://127.0.0.1:8080/v1/messages");
        assert!(on_deepseek("https://api.deepseek.com/anthropic"));
        assert!(!on_deepseek("https://api.deepseek.com.evil.example/anthropic"));
        assert!(!on_deepseek("http://127.0.0.1:8080/v1"));
    }

    #[test]
    fn the_anonymous_user_id_is_stable_and_only_deepseek_receives_it() {
        let root = TestRoot::new();
        let mut state = crate::agent_service::tests::test_state(root.path(), "127.0.0.1:1".parse().unwrap());
        state.provider_name = "deepseek".into();
        state.provider_url = "https://api.deepseek.com/anthropic".into();
        state.api_key = "fixture-key".into();
        // The host's configuration directory, not the workspace.
        state.config_path = Some(root.path().join("providers.yml"));
        state.provider_routes.insert("deepseek".into(), crate::agent_service::AgentProviderRoute {
            prefix_system_messages: false,
            responses: false,
            messages: true,
            messages_capabilities: Default::default(),
            url: state.provider_url.clone(),
            api_key: "fixture-key".into(),
            models: [("deepseek-flash".to_owned(), "deepseek-flash".to_owned())].into_iter().collect(),
            max_output_tokens: Default::default(),
            context_windows: Default::default(),
        });
        let body = json!({"model":"deepseek-flash","stream":true,"messages":[]});
        let header = |state: &crate::agent_service::AgentServiceState| {
            let request = send(state, "task", "scope", &body).build().unwrap();
            request.headers().get(USER_HEADER).map(|value| value.to_str().unwrap().to_owned())
        };
        let id = header(&state).expect("the DeepSeek Messages route names the harness identity");
        assert!(is_anonymous_id(&id), "{id}");
        // Every later request carries the same id, and so does a process that
        // starts again: the id is on disk in the configuration directory.
        assert_eq!(header(&state).as_deref(), Some(id.as_str()));
        let file = root.path().join(USER_ID_FILE);
        assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), id);
        assert_eq!(persisted_id(&file).as_deref(), Some(id.as_str()));
        assert_eq!(anonymous_user_id_in(root.path()), id);
        // No other origin receives it.
        let mut elsewhere = state.clone();
        elsewhere.provider_url = "https://api.example.com/anthropic".into();
        let request = send(&elsewhere, "task", "scope", &body).build().unwrap();
        assert!(request.headers().get(USER_HEADER).is_none());
        assert_eq!(request.url().path(), "/anthropic/v1/messages", "the route still speaks Messages");
    }

    #[test]
    fn another_route_keeps_its_own_wire() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        std::fs::create_dir_all(&root).unwrap();
        let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        let chat = chat(&["read_file"], &["read_file"], &[]);
        // No route at all, and a route that speaks Chat Completions, both keep
        // the body they were given.
        assert_eq!(wire_body(&state, "deepseek-flash", &chat), chat);
        let mut completions = state.clone();
        completions.provider_name = "plain".into();
        completions.provider_routes.insert("plain".into(), crate::agent_service::AgentProviderRoute {
            prefix_system_messages: false,
            responses: false,
            messages: false,
            messages_capabilities: Default::default(),
            url: "http://127.0.0.1:1/v1".into(),
            api_key: "fixture-key".into(),
            models: [("deepseek-flash".into(), "deepseek-flash".into())].into_iter().collect(),
            max_output_tokens: Default::default(),
            context_windows: Default::default(),
        });
        assert_eq!(wire_body(&completions, "deepseek-flash", &chat), chat);
        let _ = std::fs::remove_dir_all(root);
    }

    struct TestRoot(std::path::PathBuf);
    impl TestRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(crate::visual_artifacts::new_id());
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn messages_stream() -> String {
        let events = [
            json!({"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":9,"cache_read_input_tokens":4,"cache_creation_input_tokens":2}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-1"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading."}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call_1","name":"read_file","input":{}}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}),
            json!({"type":"message_stop"}),
        ];
        events
            .iter()
            .map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap()))
            .collect()
    }

    #[tokio::test]
    async fn a_mock_messages_endpoint_streams_text_thinking_signature_tools_and_cache_usage() {
        use axum::{Router, http::HeaderMap, response::IntoResponse, routing::post};
        #[derive(Clone)]
        struct Seen(std::sync::Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>);
        let seen = Seen(Default::default());
        let app = Router::new().route("/anthropic/v1/messages", post({
            let seen = seen.clone();
            move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    seen.0.lock().unwrap().push(
                        headers
                            .iter()
                            .map(|(name, value)| {
                                (name.as_str().to_owned(), value.to_str().unwrap_or_default().to_owned())
                            })
                            .collect(),
                    );
                    ([("content-type", "text/event-stream")], messages_stream()).into_response()
                }
            }
        }))
        .with_state(());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = TestRoot::new();
        let mut state = crate::agent_service::tests::test_state(root.path(), address);
        state.provider_name = "deepseek".into();
        state.provider_url = format!("http://{address}/anthropic");
        state.api_key = "fixture-key".into();
        state.provider_routes.insert("deepseek".into(), crate::agent_service::AgentProviderRoute {
            prefix_system_messages: false,
            responses: false,
            messages: true,
            messages_capabilities: [(
                "deepseek-flash".to_owned(),
                Capabilities { tool_update: true, system_prompt_update: true },
            )]
            .into_iter()
            .collect(),
            url: state.provider_url.clone(),
            api_key: "fixture-key".into(),
            models: [("deepseek-flash".to_owned(), "deepseek-flash".to_owned())].into_iter().collect(),
            max_output_tokens: Default::default(),
            context_windows: Default::default(),
        });
        let conn = crate::agent_service::open_db(root.path()).unwrap();
        conn.execute(
            "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('messages-test','test','deepseek-flash','running',0,0)",
            [],
        )
        .unwrap();

        let mut chat = chat(&["read_file"], &["read_file", "write_file"], &["read_file"]);
        chat["messages"].as_array_mut().unwrap().push(tools_added(vec!["write_file".to_owned()]));
        let wire = wire_body(&state, "deepseek-flash", &chat);
        assert!(tool_changes(&wire), "a tool added after the baseline is activated in the history");
        let response = send(&state, "messages-test", "scope", &wire).send().await.unwrap();
        assert!(response.status().is_success());
        let (message, stats) = crate::agent_service::consume_model_stream_observed(
            response,
            root.path(),
            "messages-test",
            1,
            1,
            None,
            &tokio_util::sync::CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        // Text, replayed thinking and the tool call the model asked for.
        assert_eq!(message["content"], "Reading.");
        assert_eq!(message["reasoning_content"], "Plan");
        assert_eq!(message[THINKING_BLOCKS], json!([{"thinking":"Plan","signature":"sig-1"}]));
        assert_eq!(message["tool_calls"], json!([{"id":"call_1","type":"function",
            "function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]));
        // Cache reads are reported apart from the uncached prompt.
        assert_eq!(stats["finish_reason"], "tool_calls");
        assert_eq!(stats["terminated"], true);
        assert_eq!(stats["usage"]["cache_read_input_tokens"], 4);
        assert_eq!(stats["usage"]["cache_creation_input_tokens"], 2);
        assert_eq!(stats["usage"]["prompt_tokens"], 15);
        assert_eq!(stats["usage"]["completion_tokens"], 12);
        assert_eq!(stats["usage"]["prompt_tokens_details"]["cached_tokens"], 4);
        // The request carried the Messages headers and none of the telemetry.
        let headers = seen.0.lock().unwrap().last().cloned().unwrap();
        let named = |name: &str| headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());
        assert_eq!(named("x-api-key").as_deref(), Some("fixture-key"));
        assert_eq!(named("anthropic-version").as_deref(), Some(VERSION));
        assert_eq!(named("anthropic-beta").as_deref(), Some(TOOL_CHANGES_BETA));
        assert!(named("authorization").is_none(), "Messages authenticates with x-api-key");
        assert!(named(SESSION_HEADER).is_none(), "only the DeepSeek origin receives affinity");

        // The reply goes back as the same conversation, with the result turn.
        let mut next = chat.clone();
        let activations = next["messages"].as_array().unwrap().iter()
            .filter(|m| m.get(ADDED_TOOLS).is_some()).cloned().collect::<Vec<_>>();
        next["messages"] = json!([
            {"role":"system","content":"You are the main AI."},
            {"role":"user","content":"Read a.txt"},
            activations[0], activations[1],
            message,
            {"role":"tool","tool_call_id":"call_1","content":"hello"},
            {"role":"user","content":"Now summarize."}]);
        let round_trip = wire_body(&state, "deepseek-flash", &next);
        assert_eq!(round_trip["messages"][2]["content"], json!([
            {"type":"thinking","thinking":"Plan","signature":"sig-1"},
            {"type":"text","text":"Reading."},
            {"type":"tool_use","id":"call_1","name":"read_file","input":{"path":"a.txt"}}]));
        assert_eq!(round_trip["messages"][3]["content"][0]["type"], "tool_result");
        assert_eq!(round_trip["messages"][3]["content"][0]["tool_use_id"], "call_1");
        // The activation stays where it was recorded, so the request opens with
        // what the earlier one sent and adds only the turns after it.
        let (sent, reopened) = (wire["messages"].as_array().unwrap(), round_trip["messages"].as_array().unwrap());
        assert_eq!(&reopened[..sent.len()], &sent[..]);
        assert_eq!(additions(&round_trip), ["write_file"], "only the later tool is added; the baseline is declared ready");
        assert_eq!(round_trip["tools"], wire["tools"]);
        server.abort();
    }
}
