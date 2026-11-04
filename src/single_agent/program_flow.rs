//! Adapter from flow pseudocode to the existing tool/Agent boundary.
//! It synthesizes normal tool exchanges; it never calls filesystem/plugin APIs directly.
use super::*;
use crate::agent_program::{self, Action, Runtime, State, runtime::ActionKind};

pub(super) enum Round {
    Exchange(String, Value),
    /// A program step ran; continue the program.
    Continue,
    /// A function call opened the frame with this scope.
    Entered(String),
    /// The current AI rewrites its failed implementation.
    Rewrite,
    Concurrent(Vec<Action>),
    Waiting,
}
fn packages(s: &Session<'_>) -> Value {
    s.state
        .tool_catalog
        .as_ref()
        .map(|c| c.packages.catalog())
        .unwrap_or_else(|| json!([]))
}
/// Every library a method can belong to: the host's own and third-party ones.
pub(super) fn library_catalog(
    s: &Session<'_>,
    definitions: &[Value],
) -> agent_program::libraries::Libraries {
    agent_program::libraries::catalog(definitions, &packages(s))
}
/// The host's own libraries, which source imports from host: and compiles against.
pub(super) fn static_libraries(
    s: &Session<'_>,
    definitions: &[Value],
) -> agent_program::libraries::Libraries {
    let packages = packages(s);
    let mut libraries = agent_program::libraries::catalog(definitions, &packages);
    for name in agent_program::libraries::third_party(&libraries, &packages) {
        libraries.remove(&name);
    }
    libraries
}
/// The third-party libraries a program can require, each with what it is for: enabled
/// plugin packages, with or without methods, and project skills.
pub(super) fn requirable(s: &Session<'_>, definitions: &[Value]) -> BTreeMap<String, String> {
    let static_names = static_libraries(s, definitions);
    let mut result = super::skill_scope::project_skills(s).into_iter().collect::<BTreeMap<_, _>>();
    for package in packages(s).as_array().into_iter().flatten().filter(|package| package["enabled"] == true) {
        let Some(name) = package["library"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| package["id"].as_str().map(agent_program::libraries::namespace))
        else {
            continue;
        };
        if !static_names.contains_key(&name) && (s.plugin.is_none() || library_catalog(s, definitions).contains_key(&name) || package["id"].as_str() == s.plugin.as_deref()) {
            result.insert(name, package["description"].as_str().unwrap_or("").to_owned());
        }
    }
    result
}

/// Package IDs address skill instructions; library names address require().
/// Project skills take precedence in read_skill, so never advertise a shadowed ID.
pub(super) fn skill_library_names(s: &Session<'_>) -> Vec<(String, String)> {
    if s.plugin.is_some() { return Vec::new(); }
    let project_names = super::skill_scope::project_skills(s).into_iter().map(|(name, _)| name).collect::<BTreeSet<_>>();
    packages(s).as_array().into_iter().flatten()
        .filter(|p| p["enabled"] == true && p["has_skill"] == true)
        .filter_map(|p| {
            let id = p["id"].as_str()?;
            let library = p["library"].as_str().map(str::to_owned).unwrap_or_else(|| agent_program::libraries::namespace(id));
            (!project_names.contains(id) && agent_program::libraries::valid_namespace(&library)).then(|| (id.to_owned(), library))
        }).collect()
}
/// What require gives: the methods of a library as they are when the line
/// runs, or an error that names what can be required. The host's own
/// libraries come first, as Node's built-in modules do.
fn link(s: &Session<'_>, definitions: &[Value], spec: &str) -> (Value, bool) {
    let name = spec.strip_suffix("/promises").unwrap_or(spec);
    if let Some(methods) = library_catalog(s, definitions).get(name) {
        return (json!({"library":spec,"methods":methods}), false);
    }
    // A skill with instructions alone is required for them, as a stylesheet
    // is imported for its effect: its object has no methods.
    if super::skill_scope::exists(s, name) {
        return (json!({"library":spec,"methods":{}}), false);
    }
    (json!({"code":"MODULE_NOT_FOUND","message":format!("Cannot find library '{spec}'; program_tool_headers lists the libraries")}), true)
}
/// The libraries a hook's declaring code requires, the host's own or
/// third-party ones; their methods join its tools.
pub(super) fn leaf_libraries(w: &Window) -> Vec<String> {
    w.program_task
        .as_ref()
        .and_then(|task| task["leaf_libraries"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
/// One call representation for ordinary, virtual and forked functions.
pub(super) fn function_task(
    program: &agent_program::Program,
    name: &str,
    arguments: &[Value],
    captured: &BTreeMap<String, Value>,
) -> Result<Value> {
    let function = program
        .functions
        .get(name)
        .context("called function missing")?;
    ensure!(
        function.params.len() == arguments.len(),
        "function argument count mismatch"
    );
    let description = if function.description.is_empty() {
        format!("Execute {name} using its supplied arguments and concrete function body")
    } else {
        function.description.clone()
    };
    // The declaration decides: a @module is written as code, a @virtual is a
    // tool loop. A program saved before @module existed has no leaf tools on a
    // virtual declared in its main program, which was then written as code.
    let role = if !function.virtual_task {
        "body"
    } else if function.module_task || function.leaf_tools.is_none() {
        "implement"
    } else {
        "leaf"
    };
    // A nested function's id carries its location; the AI sees its own name.
    let shown = if function.virtual_task {
        name.rsplit('/').next().and_then(|n| n.split('@').next()).unwrap_or(name)
    } else {
        name
    };
    let mut task = json!({"name":shown,"description":description,"parameters":function.params,"arguments":arguments,
        "captured":captured,"role":role,"leaf_tools":function.leaf_tools,"async":function.is_async});
    if function.module_task {
        task["module"] = json!(true);
    }
    if !function.leaf_libraries.is_empty() {
        task["leaf_libraries"] = json!(function.leaf_libraries);
    }
    if !function.skills.is_empty() {
        task["skills"] = json!(function.skills);
    }
    Ok(task)
}
/// What the AI does in a function window. Only AI functions call AI: a
/// @module is written as code, a @virtual is a tool loop, and a module that
/// failed is fixed in place by a fixer editing its file.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Role {
    Main,
    Implement,
    Leaf(BTreeSet<String>),
    Fix,
}
pub(super) fn role(w: &Window) -> Role {
    let Some(task) = &w.program_task else { return Role::Main };
    if task["fix"].is_object() {
        return Role::Fix;
    }
    if task["role"] != "leaf" {
        return Role::Implement;
    }
    let mut tools = task["leaf_tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    tools.extend(found_tools(&w.messages));
    Role::Leaf(tools)
}
pub(super) const IMPLEMENT_PROMPT: &str = include_str!("../../prompts/module_author.md");
pub(super) const LEAF_PROMPT: &str = include_str!("../../prompts/virtual_leaf.md");
pub(super) const FIX_PROMPT: &str = include_str!("../../prompts/module_fix.md");
/// A request's role, from the function input its messages carry.
pub(super) fn request_role(messages: &[Value]) -> &'static str {
    if messages.iter().any(|m| m["name"] == super::modules::FIX_INPUT) {
        return "fix";
    }
    let Some(input) = messages.iter().find(|m| m["name"] == "program_task_input") else {
        return "main";
    };
    let view = serde_json::from_str::<Value>(input["content"].as_str().unwrap_or("")).unwrap_or_default();
    if view["role"] == "leaf" { "leaf" } else { "implement" }
}
pub(super) fn prompt(role: &str) -> &'static str {
    match role {
        "main" => MAIN_PROMPT,
        "leaf" => LEAF_PROMPT,
        "fix" => FIX_PROMPT,
        _ => IMPLEMENT_PROMPT,
    }
}

pub(super) const FIND: &str = "find_library_methods";
pub(super) const ASK: &str = "ask_user";
pub(super) const WRITE_PROGRAM: &str = "write_program";
/// The error a function ends with when its AI asks the user. It rises to the
/// main program without being caught by code or rewritten by an author.
pub(super) const WAITING: &str = "WAITING_FOR_USER";
/// A program the user's message paused; its outcome says where.
pub(super) const PAUSED: &str = "PAUSED";
/// The model service could not answer even after retries. No code is at
/// fault, so it rises to the main program without a fixer or a rewrite.
pub(super) const UNAVAILABLE: &str = "MODEL_UNAVAILABLE";
/// The window is a frame of a running program: the main program or a function it called.
pub(super) fn in_program(w: &Window) -> bool {
    w.program.is_some() || w.program_task.is_some()
}
pub(super) fn ask_tool() -> Value {
    function(ASK, "Ask the user for a decision or information this function cannot get otherwise. The program stops here and your question is shown to the user; their answer, whatever it is, starts a new turn in which the work is planned again. Use it only when the work truly depends on the user.",
        json!({"question":{"type":"string","minLength":1}}), json!(["question"]))
}
pub(super) fn find_tool() -> Value {
    function(FIND, "Search the available library methods by keywords when this function needs one it was not given. Matching methods become callable in this function only; permissions are unchanged.",
        json!({"query":{"type":"string","minLength":1}}), json!(["query"]))
}
/// The main AI's finding, while it gets to know the task.
/// How the main AI and a module's author hand over code: the host writes it
/// to the chat's file, compiles it and runs it once the reply ends.
pub(super) fn write_program_tool(module: bool) -> Value {
    let description = if module {
        "Hand over this module's complete implementation. The host saves it as the module's file in this chat's folder and compiles it; when it compiles, this call of the function runs it. Call it as the only call in its reply. Code in reply text is never run."
    } else {
        "Hand over the program to run. The host writes source to a file in this chat's folder, compiles it, and when it compiles runs it once this reply ends; what the program prints and how it ended come back to you. Compile errors come back as this call's result, so call it again with the corrected program. Call it as the only call in its reply. Code in reply text is never run: write code there only to show it to the user."
    };
    function(WRITE_PROGRAM, description,
        json!({"source":{"type":"string","minLength":1,"description":"The complete program source."}}), json!(["source"]))
}
pub(super) fn find_main_tool() -> Value {
    function(FIND, "Search by keywords for a library method needed for this task. Matching methods become callable until your program starts. Permissions are unchanged.",
        json!({"query":{"type":"string","minLength":1}}), json!(["query"]))
}
/// Tools the main AI can call directly, in addition to conversation tools.
/// Other library methods can be discovered without expanding permissions.
pub(super) const FAMILIAR: [&str; 16] = [
    "read_file",
    "read_file_lines",
    "write_file",
    "edit_file",
    "replace_range",
    "list_project_flows",
    "read_project_flow",
    "list_dir",
    "search_text",
    "workspace_info",
    "run_project_script",
    "run_program",
    "view_image",
    "analyze_images",
    "image_analysis_results",
    "read_attachment",
];
/// Those methods and the ones the main AI found, which leave with its
/// context when the program starts.
pub(super) fn familiar(messages: &[Value]) -> BTreeSet<String> {
    let mut tools = FAMILIAR.iter().map(|tool| (*tool).to_owned()).collect::<BTreeSet<_>>();
    tools.extend(found_tools(messages));
    tools
}
/// Whether a reply has a ```js, ```javascript or ```mjs fence anywhere, even
/// one that is not a block the host can read.
pub(super) fn js_marker(message: &Value) -> bool {
    let text = message["content"].as_str().unwrap_or("");
    text.match_indices("```").any(|(at, _)| {
        let tag = text[at + 3..].chars().take_while(char::is_ascii_alphabetic).collect::<String>().to_ascii_lowercase();
        matches!(tag.as_str(), "js" | "javascript" | "mjs")
    })
}
/// The main script started. Getting to know the task was its scope and the
/// script is its return value: the turn's request, what the user added since
/// and the script stay; the exchanges that led to the script leave the
/// context and stay in the record. Skills it read stay loaded, as they are
/// carried across turns.
pub(super) fn end_familiarization(w: &mut Window, reply: &Value, source: &str) {
    let Some(start) = w.messages.iter().rposition(|m| super::task_frames::is_user_original(m) && m.get("name").is_none()) else {
        return;
    };
    let script = w.messages.iter().rposition(|m| m["role"] == "assistant" && written_source(m).and_then(Result::ok).as_deref() == Some(source))
        .filter(|at| *at > start)
        .map(|at| w.messages.remove(at))
        .unwrap_or_else(|| reply.clone());
    for mut message in w.messages.split_off(start + 1) {
        if super::task_frames::is_user_original(&message) {
            w.messages.push(message);
        } else if message["_pinned"] == true && message["role"] == "tool" {
            message["role"] = json!("user");
            message["name"] = json!("saved_asset");
            if let Some(object) = message.as_object_mut() {
                object.remove("tool_call_id");
            }
            w.messages.push(message);
        }
    }
    w.messages.push(script);
}
/// Methods a leaf found earlier in its own loop. They live in its messages and
/// disappear with them when the function returns.
fn found_tools(messages: &[Value]) -> Vec<String> {
    let calls = messages
        .iter()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
        .filter(|c| c["function"]["name"] == FIND)
        .filter_map(|c| c["id"].as_str())
        .collect::<BTreeSet<_>>();
    messages
        .iter()
        .filter(|m| m["role"] == "tool" && m["tool_call_id"].as_str().is_some_and(|id| calls.contains(id)))
        .filter_map(|m| serde_json::from_str::<Value>(m["content"].as_str()?).ok())
        .flat_map(|result| {
            result["methods"].as_array().into_iter().flatten()
                .filter_map(|m| m["tool"].as_str().map(str::to_owned)).collect::<Vec<_>>()
        })
        .collect()
}
pub(super) fn find(s: &Session<'_>, w: &Window, args: &Value) -> Result<Value> {
    let query = args["query"].as_str().context("query is required")?.to_lowercase();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    ensure!(!terms.is_empty(), "query is required");
    let definitions = available_tools(s, w);
    let catalog = library_catalog(s, &definitions);
    let mut matches = definitions
        .iter()
        .filter_map(|d| {
            let tool = d["function"]["name"].as_str()?;
            let method = agent_program::libraries::public_name(&catalog, tool)?;
            let description = d["function"]["description"].as_str().unwrap_or("");
            let text = format!("{method} {tool} {description}").to_lowercase();
            let hits = terms.iter().filter(|term| text.contains(**term)).count();
            (hits > 0).then(|| {
                (hits, json!({"tool":tool,"method":method,"description":description,"parameters":d["function"]["parameters"]}))
            })
        })
        .collect::<Vec<_>>();
    // Methods matching more of the query's words come first; ties keep the
    // catalog's order.
    matches.sort_by(|a, b| b.0.cmp(&a.0));
    let methods = matches.into_iter().take(8).map(|(_, method)| method).collect::<Vec<_>>();
    Ok(json!({"methods":methods}))
}
/// Files a function wrote successfully: write calls recorded under its frame,
/// whether its AI or its program made them, whose results were not errors.
pub(super) fn completed_writes(s: &Session<'_>, w: &Window) -> Result<Vec<Value>> {
    let Some(frame) = w.task_node_id.as_deref() else { return Ok(Vec::new()) };
    let conn = agent_service::open_db(s.state.workspace.root())?;
    let rows = conn
        .prepare("SELECT json_extract(c.data,'$.name'),json_extract(c.data,'$.arguments') FROM agent_task_events c
            WHERE c.task_id=?1 AND c.kind='tool/call' AND json_extract(c.data,'$.task_node_id')=?2
            AND json_extract(c.data,'$.name') IN ('write_file','edit_file','replace_range')
            AND EXISTS (SELECT 1 FROM agent_task_events r WHERE r.task_id=c.task_id AND r.kind='tool/result'
                AND json_extract(r.data,'$.message.toolCallId')=json_extract(c.data,'$.callId')
                AND json_extract(r.data,'$.message.isError')=0)
            ORDER BY c.seq")?
        .query_map(params![s.task, frame], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(tool, arguments)| {
            let path = serde_json::from_str::<Value>(&arguments).map(|a| a["path"].clone()).unwrap_or(Value::Null);
            json!({"tool":tool,"path":path})
        })
        .collect())
}
fn call_arguments(task: &Value) -> Result<(&Vec<Value>, &Vec<Value>)> {
    let parameters = task["parameters"].as_array().context(
        "function parameters missing; legacy call contracts cannot resume under this version",
    )?;
    let arguments = task["arguments"]
        .as_array()
        .context("ordered function arguments missing")?;
    ensure!(
        parameters.len() == arguments.len() && parameters.iter().all(Value::is_string),
        "invalid function argument contract"
    );
    Ok((parameters, arguments))
}
/// Model-facing view: bindings are actual local variable values, not an input envelope.
pub(super) fn call_view(task: &Value) -> Result<Value> {
    let (parameters, arguments) = call_arguments(task)?;
    let names = parameters
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect::<Vec<_>>();
    let bindings = names
        .iter()
        .zip(arguments)
        .map(|(p, v)| ((*p).to_owned(), v.clone()))
        .collect::<serde_json::Map<_, _>>();
    let mut view = json!({"name":task["name"],"signature":format!("{}function {}({})",if task["async"] == true {"async "} else {""},task["name"].as_str().context("function name missing")?,names.join(", ")),
        "role":task["role"],"description":task["description"],"bindings":bindings,"closure":task["captured"],
        "binding_rule":"bindings lists actual local parameter values. Use each parameter directly as in the signature; bindings is display metadata, never another property of a parameter. Arguments bind by position only, never by name or automatic object packing. closure lists the outer variables the description names with @uses, read when this function was called; in code they are variables in scope, as in a JavaScript closure."});
    // The libraries the contract names with @uses, whose instructions it has.
    if task["skills"].as_array().is_some_and(|skills| !skills.is_empty()) {
        view["skills"] = task["skills"].clone();
    }
    Ok(view)
}
pub(super) fn task_runtime(task: &Value) -> Result<Option<Runtime>> {
    if !task["body"].is_object() {
        return Ok(None);
    }
    let (_, arguments) = call_arguments(task)?;
    compiled_runtime(&json!({"program":{"compiled":task["body"]["compiled"],"entry":task["name"],"args":arguments}})).map(Some)
}
pub(super) fn install_scoped(
    s: &Session<'_>,
    w: &mut Window,
    args: &Value,
    body: bool,
) -> Result<Value> {
    ensure!(args.is_object(), "program arguments must be a JSON object");
    ensure!(
        args.get("libraries").is_none(),
        "library names are fixed by the host/plugin declarations; libraries aliases are unsupported"
    );
    let libraries = static_libraries(s, &tools(s, w));
    if body {
        install_body_with(w, args, libraries)
    } else {
        install_with(w, args, libraries)
    }
}
/// The program a reply hands over: the source of its write_program call.
/// Code in reply text is never run, so an example written for the user stays
/// an answer. Err when the call's arguments carry no source.
pub(super) fn written_source(message: &Value) -> Option<std::result::Result<String, String>> {
    let call = message["tool_calls"].as_array()?.iter().find(|call| call["function"]["name"] == WRITE_PROGRAM)?;
    let arguments = call["function"]["arguments"].as_str().unwrap_or("");
    Some(
        serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|args| args["source"].as_str().map(str::to_owned))
            .ok_or_else(|| "write_program needs {\"source\": \"<the complete program>\"}".to_owned()),
    )
}
/// Test fixtures written when programs came in reply text: a fixture reply's
/// ```js block becomes the write_program call that now carries a program, and
/// the words around it stay the reply's text.
#[cfg(test)]
pub(crate) fn fixture_reply(content: &str) -> Value {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let mut words = Vec::new();
    let mut source = None;
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        let program = source.is_none() && line.trim_start().strip_prefix("```")
            .is_some_and(|tag| matches!(tag.trim().to_ascii_lowercase().as_str(), "js" | "javascript" | "mjs"));
        if !program {
            words.push(line);
            continue;
        }
        let mut body = Vec::new();
        for line in lines.by_ref() {
            if line.trim_start().starts_with("```") { break; }
            if let Some(code) = line.trim_end().strip_suffix("```") {
                body.push(code);
                break;
            }
            body.push(line);
        }
        source = Some(body.join("\n"));
    }
    let Some(source) = source else {
        return json!({"role":"assistant","content":content});
    };
    let words = words.join("\n").trim().to_owned();
    let id = format!("write-{}", CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    json!({"role":"assistant","content":if words.is_empty(){Value::Null}else{json!(words)},
        "tool_calls":[{"id":id,"type":"function","function":{"name":WRITE_PROGRAM,"arguments":json!({"source":source}).to_string()}}]})
}
/// A fixture reply as a chat completion response.
#[cfg(test)]
pub(crate) fn fixture_completion(content: &str) -> Value {
    let message = fixture_reply(content);
    let finish = if message["tool_calls"].is_array() { "tool_calls" } else { "stop" };
    json!({"choices":[{"finish_reason":finish,"message":message}]})
}
/// A main reply as the user sees it: the program it handed over is the
/// conversation's own record, read on demand, so the user sees its words.
/// None when the reply wrote no program.
pub(super) fn shown_reply(message: &Value) -> Option<Value> {
    let calls = message["tool_calls"].as_array()?;
    if !calls.iter().any(|call| call["function"]["name"] == WRITE_PROGRAM) {
        return None;
    }
    let mut message = message.clone();
    let kept = calls.iter().filter(|call| call["function"]["name"] != WRITE_PROGRAM).cloned().collect::<Vec<_>>();
    if kept.is_empty() {
        message.as_object_mut()?.remove("tool_calls");
    } else {
        message["tool_calls"] = json!(kept);
    }
    Some(message)
}
/// Repairable source errors stay ordinary tool results for the current AI.
/// No source is auto-edited and no failed install replaces the saved program.
pub(super) fn source_error(operation: &str, args: &Value, error: &anyhow::Error) -> Value {
    let message = format!("{error:#}");
    let at = message.strip_prefix("TS_PROGRAM ").and_then(|rest| {
        let mut parts = rest.splitn(3, ':');
        Some((
            parts.next()?.parse::<usize>().ok()?,
            parts.next()?.parse::<usize>().ok()?,
        ))
    });
    let diagnostic = if let Some((line, column)) = at {
        let lines = args["source"]
            .as_str()
            .unwrap_or("")
            .lines()
            .collect::<Vec<_>>();
        let start = line.saturating_sub(2);
        let excerpt = lines.iter().enumerate().skip(start).take(3)
            .map(|(index, text)|json!({"line":index+1,"text":text.chars().take(400).collect::<String>()}))
            .collect::<Vec<_>>();
        json!({"message":message,"line":line,"column":column,"source_excerpt":excerpt})
    } else {
        json!({"message":message})
    };
    json!({"error":message,"code":"PROGRAM_SOURCE_INVALID","phase":"source_validation",
        "retryable":true,"executed":false,"operation":operation,"diagnostics":[diagnostic],
        "recovery":"Nothing ran. Rewrite the complete source using these diagnostics and the published tool contracts, and hand it over again with write_program. Keep the goal, the current function name and parameters, and prior results. Do not repeat completed operations. Never report a rejected program as completed."})
}

#[cfg(test)]
pub(super) fn install(w: &mut Window, args: &Value) -> Result<Value> {
    install_with(w, args, agent_program::libraries::builtin())
}
fn install_with(
    w: &mut Window,
    args: &Value,
    libraries: agent_program::libraries::Libraries,
) -> Result<Value> {
    ensure!(
        args.get("libraries").is_none(),
        "model-defined libraries are unsupported"
    );
    let source = args["source"].as_str().context("source is required")?;
    let program = agent_program::compile_with_libraries(source, libraries)?;
    ensure!(
        args.as_object().is_some_and(|args| args.keys().all(|key| key == "source")),
        "a program is its source alone; put its data in the code or read it with the imported tools"
    );
    let entry = agent_program::ENTRY;
    let runtime = Runtime::task_program(program, entry, vec![])?;
    w.program = Some(runtime);
    w.program_value = None;
    Ok(
        json!({"status":"started","entry":entry,"version":agent_program::VERSION,"source_hash":crate::visual_artifacts::hash(source.as_bytes())}),
    )
}
#[cfg(test)]
pub(super) fn install_body(w: &mut Window, args: &Value) -> Result<Value> {
    install_body_with(w, args, agent_program::libraries::builtin())
}
pub(super) fn install_body_with(
    w: &mut Window,
    args: &Value,
    libraries: agent_program::libraries::Libraries,
) -> Result<Value> {
    ensure!(
        args.get("libraries").is_none(),
        "model-defined libraries are unsupported"
    );
    ensure!(
        w.program.is_none(),
        "the current task body is already executing"
    );
    let task = w
        .program_task
        .as_ref()
        .context("define_task_body requires a current task function")?;
    let name = task["name"]
        .as_str()
        .context("current function name missing")?;
    let source = args["source"].as_str().context("source is required")?;
    let scope = super::function_scope::values(w);
    let program = agent_program::compile_body_with_scope(
        source,
        name,
        libraries,
        scope.keys().cloned().collect(),
    )?;
    let params = &program.functions[name].params;
    let (_, arguments) = call_arguments(task)?;
    ensure!(
        params.len() == arguments.len(),
        "keep the current function's parameter count ({}); arguments bind by position, with no automatic object packing",
        arguments.len()
    );
    let values = arguments.clone();
    let entry = name.to_owned();
    w.program = Some(Runtime::task_program_with_scope(
        program, &entry, values, scope,
    )?);
    w.program_value = None;
    Ok(
        json!({"status":"started","function":entry,"version":agent_program::VERSION,"source_hash":crate::visual_artifacts::hash(source.as_bytes())}),
    )
}
pub(super) fn compiled_runtime(args: &Value) -> Result<Runtime> {
    compiled_runtime_with_scope(args, BTreeMap::new())
}
pub(super) fn compiled_runtime_with_scope(
    args: &Value,
    scope: BTreeMap<String, Value>,
) -> Result<Runtime> {
    let p = args
        .get("program")
        .filter(|v| v.is_object())
        .context("compiled flow function required")?;
    let program: agent_program::Program =
        serde_json::from_value(p["compiled"].clone()).context("compiled host program required")?;
    let entry = p["entry"].as_str().context("program.entry is required")?;
    let arguments = p["args"]
        .as_array()
        .context("program.args must be an array")?
        .clone();
    Runtime::task_program_with_scope(program, entry, arguments, scope)
}
/// Private scheduling effects have no model tool schema. Only a pending
/// compiled instruction may cross this dispatch boundary.
pub(super) fn internal_call(w: &Window, call: &crate::model_stream::ToolCall) -> bool {
    w.program
        .as_ref()
        .and_then(|p| p.pending.as_ref())
        .is_some_and(|action| {
            action.id == call.call_id
                && matches!(
                    (&action.kind, call.name.as_str()),
                    (ActionKind::ParallelReads { .. }, "parallel_read_batch")
                )
        })
}

fn recorded(s: &Session<'_>, id: &str) -> Result<Option<(i64, Value)>> {
    let conn = agent_service::open_db(s.state.workspace.root())?;
    let sql = "SELECT seq,data FROM agent_task_events WHERE task_id=?1 AND ((kind='tool/result' AND json_extract(data,'$.message.toolCallId')=?2) OR (kind='subagent/event' AND json_extract(data,'$.event_type')='tool/result' AND json_extract(data,'$.event.message.toolCallId')=?2)) ORDER BY seq DESC LIMIT 1";
    let row: Option<(i64, String)> = conn
        .query_row(sql, params![s.task, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    row.map(|(seq, raw)| {
        let v: Value = serde_json::from_str(&raw)?;
        Ok((
            seq,
            if v["event"].is_object() {
                v["event"].clone()
            } else {
                v
            },
        ))
    })
    .transpose()
}
fn resolve(
    _s: &Session<'_>,
    w: &mut Window,
    action: &Action,
    mut value: Value,
    mut failed: bool,
    _seq: i64,
) -> Result<()> {
    if !failed {
        match &action.kind {
            ActionKind::Fork { .. } | ActionKind::Join { .. } => anyhow::bail!("saved fork/join calls are retired"),
            ActionKind::Tool { .. } | ActionKind::Require { .. } | ActionKind::ParallelReads { .. } => {}
            ActionKind::Task { .. } | ActionKind::Call { .. } => {
                match value["result"]
                    .as_str()
                    .context("comment task returned no value")
                    .and_then(|s| {
                        serde_json::from_str::<Value>(s)
                            .context("comment task must return a JSON value")
                    }) {
                    Ok(v) if v.get("$task_error").is_some() => {
                        failed = true;
                        value = v["$task_error"].clone();
                    }
                    Ok(v) => value = v,
                    Err(e) => {
                        failed = true;
                        value = json!({"message":format!("{e:#}"),"receipt":value});
                    }
                }
            }
        }
    }
    if failed {
        locate_question(action, &mut value);
    }
    w.program.as_mut().unwrap().resume(value, failed)
}
/// A question rising through a call gains the caller's step, so its outcome
/// says where the program stopped, as a pause's does.
fn locate_question(action: &Action, error: &mut Value) {
    if error["code"] != WAITING {
        return;
    }
    let step = super::paused_programs::step_label(action);
    error["stopped_at"] = json!(match error["stopped_at"].as_str().filter(|at| !at.is_empty()) {
        Some(inner) => format!("{step} → {inner}"),
        None => step,
    });
}
pub(super) async fn recover(s: &Session<'_>, w: &mut Window) -> Result<()> {
    // Version rejection must precede receipt consumption or dispatch.
    if w.program
        .as_ref()
        .is_some_and(|p| p.program.version != agent_program::VERSION)
    {
        return Ok(());
    }
    let inflight = w.program.as_ref().map(|p| p.inflight_actions()).unwrap_or_default();
    let mut completed = Vec::new();
    for action in inflight {
        if let Some((seq, event)) = recorded(s, &action.id)? { completed.push((seq, action, event)); }
    }
    completed.sort_by_key(|(seq, _, _)| *seq);
    for (_, action, event) in completed {
        let mut value = event["meta"]["result"].clone();
        let mut failed = event["message"]["isError"] == true;
        if !failed {
            match value["result"].as_str().and_then(|text| serde_json::from_str::<Value>(text).ok()) {
                Some(result) if result.get("$task_error").is_some() => { failed = true; value = result["$task_error"].clone(); }
                Some(result) => value = result,
                None => { failed = true; value = json!({"message":"async function returned no valid JSON value"}); }
            }
        }
        if failed {
            locate_question(&action, &mut value);
        }
        w.program.as_mut().unwrap().resume_action(&action.id, value, failed)?;
        save(s, w).await?;
    }
    let Some(action) = w.program.as_ref().and_then(|p| p.pending.clone()) else {
        return Ok(());
    };
    if let Some((seq, event)) = recorded(s, &action.id)? {
        resolve(
            s,
            w,
            &action,
            event["meta"]["result"].clone(),
            event["message"]["isError"] == true,
            seq,
        )?;
        save(s, w).await?;
    }
    Ok(())
}
pub(super) async fn interrupt_unfinished(s: &Session<'_>, w: &mut Window) -> Result<()> {
    let actions = w.program.as_ref().map(|runtime| runtime.inflight_actions()).unwrap_or_default();
    for action in actions {
        task_frames::interrupt_parallel_function(s, &action.id).await?;
        w.program.as_mut().unwrap().resume_action(&action.id, json!({"message":"async function was interrupted; its external effects were not replayed"}), true)?;
    }
    save(s, w).await
}
/// How many times a turn's main program may be rewritten after an error.
const MAIN_REWRITES: i64 = 3;
fn main_rewrites(s: &Session<'_>) -> Result<i64> {
    let conn = agent_service::open_db(s.state.workspace.root())?;
    Ok(conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id=?1 AND kind='program/runtime_error'
        AND json_extract(data,'$.main')=1 AND json_extract(data,'$.turn')=?2", params![s.task, s.turn as i64], |r| r.get(0))?)
}
/// Files written this turn by any of its programs or AIs, for its outcome.
fn turn_writes(s: &Session<'_>) -> Result<Vec<String>> {
    let conn = agent_service::open_db(s.state.workspace.root())?;
    let paths = conn
        .prepare("SELECT DISTINCT json_extract(json_extract(c.data,'$.arguments'),'$.path') FROM agent_task_events c
            WHERE c.task_id=?1 AND c.kind='tool/call' AND json_extract(c.data,'$.turn')=?2
            AND json_extract(c.data,'$.name') IN ('write_file','edit_file','replace_range')
            AND EXISTS (SELECT 1 FROM agent_task_events r WHERE r.task_id=c.task_id AND r.kind='tool/result'
                AND json_extract(r.data,'$.message.toolCallId')=json_extract(c.data,'$.callId')
                AND json_extract(r.data,'$.message.isError')=0)
            ORDER BY c.seq")?
        .query_map(params![s.task, s.turn as i64], |r| r.get::<_, Option<String>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(paths.into_iter().flatten().collect())
}
/// The program's latest console lines, short enough for an outcome.
fn printed(w: &Window) -> Vec<String> {
    const SHOWN: usize = 20;
    let logs = w.program.as_ref().map(|program| program.logs()).unwrap_or_default();
    let mut shown = logs.iter().rev().take(SHOWN).rev().cloned().collect::<Vec<_>>();
    if logs.len() > SHOWN {
        shown.insert(0, format!("（前面还有 {} 行）", logs.len() - SHOWN));
    }
    shown
}
/// How a turn's main program ended, in a few lines: what it returned or where
/// it stopped, and the files it wrote. It is the turn's answer and what later
/// turns see of it; the code itself stays in the conversation record.
fn outcome(s: &Session<'_>, printed: &[String], ended: std::result::Result<&Value, &Value>) -> Result<String> {
    let clip = |text: &str, limit: usize| {
        if text.chars().count() > limit { format!("{}…", text.chars().take(limit).collect::<String>()) } else { text.to_owned() }
    };
    let mut lines = match ended {
        Ok(value) if value.is_null() => vec!["程序已完成。".to_owned()],
        Ok(value) => {
            let text = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
            vec!["程序已完成。".to_owned(), format!("返回：{}", clip(&text, 300))]
        }
        Err(error) if error["code"] == WAITING => {
            let question = error["question"].as_str().unwrap_or("");
            let at = error["stopped_at"].as_str().filter(|at| !at.is_empty()).map(str::to_owned)
                .unwrap_or_else(|| error["function"].as_str().unwrap_or("程序").to_owned());
            vec![
                format!("程序停在 {at}，等你回答：{question}"),
                "之后的步骤没有执行。".to_owned(),
            ]
        }
        Err(error) if error["code"] == PAUSED => {
            let at = error["stopped_at"].as_str().filter(|at| !at.is_empty())
                .map(|at| format!("停在 {at}，这一步还没完成")).unwrap_or_else(|| "停在两步之间".into());
            vec![
                format!("程序被你的新消息暂停，{at}。"),
                "它的状态都保留着：要继续，在新程序里 import * as previous from \"host:previous/promises\"，previous.resume() 会从停下的地方接着跑，返回它的结果（Promise）；不调用，它会在这一轮结束时丢弃。".to_owned(),
            ]
        }
        Err(error) => {
            let message = error["message"].as_str().map(str::to_owned).unwrap_or_else(|| error.to_string());
            let at = error["location"]["line"].as_u64().map(|line| format!("第 {line} 行")).unwrap_or_default();
            vec![format!("程序在{at}出错停止：{}", clip(&message, 300)), "之后的步骤没有执行。".to_owned()]
        }
    };
    // What the program printed reaches the AI that wrote it.
    if !printed.is_empty() {
        lines.push(format!("输出：\n{}", printed.join("\n")));
    }
    let writes = turn_writes(s)?;
    if !writes.is_empty() {
        let shown = writes.iter().take(20).cloned().collect::<Vec<_>>().join("、");
        let more = if writes.len() > 20 { format!(" 等 {} 个文件", writes.len()) } else { String::new() };
        lines.push(format!("写入：{shown}{more}"));
    }
    Ok(lines.join("\n"))
}
/// Pause the main program for the user's new message; its outcome is the
/// reply the message then follows, and the main AI decides what runs next.
pub(super) async fn pause_main(s: &Session<'_>, w: &mut Window, step: usize) -> Result<()> {
    let console = printed(w);
    let stopped_at = super::paused_programs::pause_main(s, w).await?;
    let paused = json!({"code":PAUSED,"stopped_at":stopped_at});
    let (text, shown) = (outcome(s, &console, Err(&paused))?, outcome(s, &[], Err(&paused))?);
    end_program(s, w, step, Value::Null, text, Some(shown)).await?;
    // The turn goes on: the main AI may answer the message with a new program.
    w.program_value = None;
    save(s, w).await
}
async fn return_value(s: &Session<'_>, w: &mut Window, step: usize, value: Value) -> Result<Round> {
    if w.program_task.is_none() {
        // A successful main program is one piece of work, not necessarily the
        // end of the user's task. Give its actual result back to its author.
        let text = outcome(s, &printed(w), Ok(&value))?;
        w.program = None;
        w.program_value = None;
        let seq = append(s, "program/result", json!({"turn":s.turn,"step":step,"task_node_id":w.task_node_id,"result":value,"output":text}), false).await?;
        w.messages.push(sourced(json!({"role":"user","name":"program_result","content":text}), s.task, seq));
        save(s, w).await?;
        return Ok(Round::Continue);
    }
    let (text, shown) = if w.program_task.is_some() {
        (serde_json::to_string(&value)?, None)
    } else {
        (outcome(s, &printed(w), Ok(&value))?, Some(outcome(s, &[], Ok(&value))?))
    };
    end_program(s, w, step, value, text, shown).await
}
/// End the window's program with this reply: a function returns its value,
/// a main program its outcome. The frame then finishes like any reply.
/// `shown` is the outcome as the user reads it, without what the program
/// printed for the AI; the AI's context and memory keep the full text.
async fn end_program(s: &Session<'_>, w: &mut Window, step: usize, value: Value, text: String, shown: Option<String>) -> Result<Round> {
    w.program_value = Some(value);
    w.program = None;
    let message = json!({"role":"assistant","content":text});
    let seq = append(
        s,
        "assistant/message",
        json!({"actor":"program","turn":s.turn,"step":step,"message":{"role":"assistant","content":shown.unwrap_or_else(|| text.clone())}}),
        s.is_primary(),
    )
    .await?;
    w.messages.push(sourced(message.clone(), s.task, seq));
    save(s, w).await?;
    Ok(Round::Exchange(
        format!("program_return_{}", agent_service::uuid_like()),
        message,
    ))
}
pub(super) async fn next(s: &Session<'_>, w: &mut Window, step: usize) -> Result<Round> {
    recover(s, w).await?;
    let state = w.program.as_mut().unwrap().next_concurrent();
    save(s, w).await?;
    let state = match state {
        agent_program::runtime::ConcurrentState::Actions(actions) if actions.first().is_some_and(|action| w.program.as_ref().unwrap().async_scope(&action.id).is_some()) => return Ok(Round::Concurrent(actions)),
        agent_program::runtime::ConcurrentState::Actions(mut actions) => State::Action(actions.remove(0)),
        agent_program::runtime::ConcurrentState::Waiting => return Ok(Round::Waiting),
        agent_program::runtime::ConcurrentState::Returned(value) => State::Returned(value),
        agent_program::runtime::ConcurrentState::Failed(error) => State::Failed(error),
    };
    match state {
        State::Waiting => unreachable!(),
        State::Failed(error) => {
            append(s, "program/error", json!({"error":error}), true).await?;
            // A question for the user is not a bug to fix, nor is a model
            // service that could not answer: both rise unchanged.
            let waiting = error["code"] == WAITING;
            let rises = waiting || error["code"] == UNAVAILABLE;
            // Read before the program is dropped below.
            let console = printed(w);
            let module_version = w.program_task.as_ref().is_some_and(|task| task["module_version"].is_number());
            if !rises && super::modules::is_module(w) && module_version {
                // A module is shared by every call of its contract: a fixer
                // edits its file, or this call waits for the fix already under way.
                match super::modules::failed(s, w, &error, console).await? {
                    super::modules::AfterFailure::Rerun | super::modules::AfterFailure::Fixing => Ok(Round::Rewrite),
                    super::modules::AfterFailure::Fail => return_value(s, w, step, json!({"$task_error":error})).await,
                }
            } else if !rises && role(w) == Role::Implement && w.program_task.as_ref().is_some_and(|task| task["role"] == "implement") {
                // AI-written code is not right every time: its author gets the
                // error and what already ran, and rewrites it or gives up.
                w.program = None;
                let diagnostic = json!({"code":"FUNCTION_RUNTIME_ERROR","executed":true,"error":error,
                    "completed_writes":completed_writes(s, w)?,"console":console,
                    "instruction":"Your implementation stopped with this error. Reply with a corrected complete ```js block; it runs from its start, so do not redo the listed writes unless the contract needs them redone. If the contract cannot be met, reply with {\"$task_error\":{\"message\":\"concrete cause\"}} instead."});
                let seq = append(s, "program/runtime_error", json!({"diagnostic":diagnostic}), false).await?;
                w.messages.push(sourced(json!({"role":"user","name":"program_diagnostic","content":diagnostic.to_string()}), s.task, seq));
                save(s, w).await?;
                Ok(Round::Rewrite)
            } else if w.program_task.is_some() {
                // A runtime failure throws to the caller, as in JavaScript.
                return_value(s, w, step, json!({"$task_error":error})).await
            } else if !rises && main_rewrites(s)? < MAIN_REWRITES {
                // The main program heals the same way, a few times a turn: the
                // main AI gets the error and the files already written.
                w.program = None;
                let diagnostic = json!({"code":"PROGRAM_RUNTIME_ERROR","executed":true,"error":error,
                    "completed_writes":turn_writes(s)?,"console":console,
                    "instruction":"Your program stopped with this error. Reply with a corrected complete ```js block; it runs from its start, so do not redo the listed writes unless the task needs them redone. If the task cannot be done, tell the user why in plain text instead."});
                let seq = append(s, "program/runtime_error", json!({"diagnostic":diagnostic,"turn":s.turn,"main":true}), false).await?;
                w.messages.push(sourced(json!({"role":"user","name":"program_diagnostic","content":diagnostic.to_string()}), s.task, seq));
                save(s, w).await?;
                Ok(Round::Rewrite)
            } else {
                // A question, or a program that keeps failing: its outcome ends
                // the turn, so the next message starts fresh instead of
                // resuming the failure.
                let (text, shown) = (outcome(s, &console, Err(&error))?, outcome(s, &[], Err(&error))?);
                end_program(s, w, step, Value::Null, text, Some(shown)).await
            }
        }
        State::Returned(value) => return_value(s, w, step, value).await,
        State::Action(action) => {
            let at = json!(action.at);
            let (name, args) = match &action.kind {
                ActionKind::Call { name, args, captured } => {
                    let program = &w.program.as_ref().unwrap().program;
                    let function = &program.functions[name];
                    let mut task = function_task(program, name, args, captured)?;
                    let description = task["description"].clone();
                    if !function.virtual_task {
                        task["body"] = json!({"compiled":program});
                    }
                    (
                        "create_task".into(),
                        json!({"goal":name,"briefing":description,"program_task":task,"call_site":at}),
                    )
                }
                ActionKind::Task {
                    name,
                    description,
                    input,
                } => {
                    let program = &w.program.as_ref().unwrap().program;
                    let arguments = program.functions[name]
                        .params
                        .iter()
                        .map(|parameter| {
                            input
                                .get(parameter)
                                .cloned()
                                .context("virtual argument missing")
                        })
                        .collect::<Result<Vec<_>>>()?;
                    // A nested virtual's captured variables ride in its input.
                    let captured = program.functions[name].uses.iter()
                        .map(|used| Ok((used.clone(), input.get(used).cloned().context("captured variable missing")?)))
                        .collect::<Result<BTreeMap<_, _>>>()?;
                    let task = function_task(program, name, &arguments, &captured)?;
                    (
                        "create_task".into(),
                        json!({"goal":name,"briefing":description,"program_task":task,"call_site":at}),
                    )
                }
                ActionKind::Tool { name, args } => (name.clone(), args.clone()),
                // The host links the library itself; no tool runs and no AI sees it.
                ActionKind::Require { library } => {
                    let (value, failed) = link(s, &super::tools(s, w), library);
                    if failed && s.plugin.as_ref().is_some_and(|owner| packages(s).as_array().into_iter().flatten().any(|package| {
                        package["id"] == *owner && package["tools"].as_array().is_some_and(|tools| !tools.is_empty())
                            && package["library"].as_str().map(str::to_owned).unwrap_or_else(|| agent_program::libraries::namespace(owner)) == library.trim_end_matches("/promises")
                    })) {
                        anyhow::bail!("PLUGIN_UNAVAILABLE: declared plugin library {library} is unavailable in this execution");
                    }
                    append(s, "program/require", json!({"library":library,"result":value,"failed":failed,"program_location":at}), false).await?;
                    w.program.as_mut().unwrap().resume(value, failed)?;
                    save(s, w).await?;
                    return Ok(Round::Continue);
                }
                ActionKind::ParallelReads { operations } => (
                    "parallel_read_batch".into(),
                    json!({"operations":operations.iter().map(|(name,args)|json!({"name":name,"args":args})).collect::<Vec<_>>(), "settled": w.program.as_ref().unwrap().is_pending_async_batch()}),
                ),
                ActionKind::Fork { .. } | ActionKind::Join { .. } => anyhow::bail!("saved fork/join calls are retired"),
            };
            let node = format!("program_{}", agent_service::uuid_like());
            let call = crate::model_stream::ToolCall { call_id: action.id.clone(), name, arguments: args.to_string() };
            // A function call opens its frame; a tool step runs and its result is
            // the value of the program's expression. Neither enters an AI window.
            if call.name == "create_task" {
                let scope = task_frames::create_task(s, w, &call, &node, step).await?;
                return Ok(Round::Entered(scope));
            }
            // Resuming runs the paused program first; when the user speaks
            // again it pauses once more and this step stays pending.
            if call.name == super::paused_programs::RESUME && w.resumed.is_none() {
                let Some(resumed) = super::paused_programs::resume(s, w).await? else {
                    return Ok(Round::Continue);
                };
                w.resumed = Some(resumed);
            }
            let definitions = super::tools(s, w);
            let done = super::execute_call(s, w, &definitions, &node, step, &call, 1, Some(&at)).await?;
            super::stop_on_conflict(s, &done.result).await?;
            if done.result["host_denied"] == true && done.result["code"] == WAITING && w.program_task.is_none() {
                // A plugin's explicit user-confirmation boundary stops the
                // main program even if its JavaScript would catch errors.
                let mut waiting = done.result.clone();
                locate_question(&action, &mut waiting);
                let (text, shown) = (outcome(s, &printed(w), Err(&waiting))?, outcome(s, &[], Err(&waiting))?);
                return end_program(s, w, step, Value::Null, text, Some(shown)).await;
            }
            // A resumed program's error rises as it was, so a question keeps its code.
            let (result, is_error) = match w.resumed.take() {
                Some((error, true)) => (error, true),
                _ => (done.result, done.is_error),
            };
            resolve(s, w, &action, result, is_error, done.seq)?;
            save(s, w).await?;
            Ok(Round::Continue)
        }
    }
}

/// The output pages a published example declares inline.
#[cfg(test)]
pub(super) fn example_pages(source: &str) -> Vec<&str> {
    source.split("path: \"").skip(1).filter_map(|rest| rest.split('"').next()).filter(|path| path.ends_with(".html")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct TestRoot(std::path::PathBuf);
    impl TestRoot {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("ts-program-test-{}", agent_service::uuid_like()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn path(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[tokio::test]
    async fn unavailable_write_fails_compilation_before_any_tool_effect() {
        let root = TestRoot::new();
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let app=Router::new().route("/v1/chat/completions",post(move |Json(_):Json<Value>|{
            let n=requests.fetch_add(1,Ordering::SeqCst);
            async move {
            if n==2 {return Json(json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"The write is unavailable in read-only mode; no file operation ran."}}]}));}
            let (name,args)=match n {
                0=>("create_task",json!({"goal":"Attempt denied write"})),
                1=>("execute_program",json!({"source":"function main(){io.write({path:'denied.txt',content:'x'});io.read({path:'next.txt'});}"})),
                _=>panic!("runtime error must stop, not ask model to reschedule"),
            };
            Json(json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[{"id":format!("m{n}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut state =
            agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        state.permission_mode = agent_service::PermissionMode::ReadOnly;
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)",[]).unwrap();
        run_task(
            state,
            "task".into(),
            "fake".into(),
            "Attempt a write".into(),
            8,
            CancellationToken::new(),
            1,
            false,
        )
        .await
        .unwrap();
        server.abort();
        assert_eq!(
            conn.query_row("SELECT status FROM agent_tasks", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "completed"
        );
        assert!(!root.path().join("denied.txt").exists());
        assert_eq!(count.load(Ordering::SeqCst), 3);
        let calls:i64=conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='tool/call' AND json_extract(data,'$.name')='read_file'",[],|r|r.get(0)).unwrap();
        assert_eq!(calls, 0, "operation after denied write must not execute");
    }



    #[test]
    fn source_adapters_reject_extensions_without_installing_a_program() {
        for source in [
            "function* work(){return 1;}",
            "function work(input: string){return input;}",
            "function work(){return ai({instruction:'value'});}",
            "function work(){return io.read({});} export function unused(){return 1;}",
        ] {
            let mut main = Window::default();
            assert!(install(&mut main, &json!({"source":source.replace("work", "main")})).is_err());
            assert!(main.program.is_none());
            let mut child = Window {
                program_task: Some(json!({"name":"work","parameters":[],"arguments":[]})),
                ..Default::default()
            };
            assert!(install_body(&mut child, &json!({"source":source})).is_err());
            assert!(child.program.is_none());
            assert!(
                compiled_runtime(&json!({"program":{"source":source,"entry":"work","args":[]}}))
                    .is_err()
            );
        }
    }


    #[test]
    fn refinement_uses_only_positional_arguments_and_never_packs_them() {
        let shared = json!({"rule":"R"});
        let item = json!({"path":"a"});
        for source in [
            "function work(shared,item){return {shared,item};}",
            "function work(rules,part){return {shared:rules,item:part};}",
        ] {
            let mut w = Window {
                program_task: Some(
                    json!({"name":"work","parameters":["shared","item"],"arguments":[shared,item]}),
                ),
                ..Default::default()
            };
            install_body(&mut w, &json!({"source":source})).unwrap();
            assert!(
                matches!(w.program.as_mut().unwrap().next(),State::Returned(value) if value==json!({"shared":shared,"item":item}))
            );
        }
        let mut w = Window {
            program_task: Some(
                json!({"name":"work","parameters":["shared","item"],"arguments":[shared,item]}),
            ),
            ..Default::default()
        };
        assert!(
            install_body(
                &mut w,
                &json!({"source":"function work(input){return input;}"})
            )
            .is_err()
        );
        assert!(w.program.is_none());
    }

    #[test]
    fn forked_functions_use_the_host_compiled_program() {
        let program=agent_program::compile("function work(input){return io.read({path:input.path});} function main(){return null;}").unwrap();
        let mut fork = compiled_runtime(
            &json!({"program":{"compiled":program,"entry":"work","args":[{"path":"source.txt"}]}}),
        )
        .unwrap();
        assert!(
            matches!(fork.next(),State::Action(Action{kind:ActionKind::Tool{name,args},..}) if name=="read_file" && args["path"]=="source.txt")
        );
    }


    #[test]
    fn fixed_libraries_respect_frame_permissions_and_reject_model_aliases() {
        let root = TestRoot::new();
        let mut state =
            agent_service::tests::test_state(root.path(), "127.0.0.1:9".parse().unwrap());
        state.permission_mode = agent_service::PermissionMode::ReadOnly;
        let cancel = CancellationToken::new();
        let s = Session {
            agent_id: None,
            task_node_id: None,
            plugin: None,
            state: &state,
            task: "task",
            model: "fake",
            turn: 1,
            cancel: &cancel,
            ban_execution: false,
            scope: "main".into(),
            parent: None,
            input_budget: INPUT_BUDGET,
        };
        let mut w = Window::default();
        let source = "function main(){io.read({path:'source'});io.write({path:'must-not-write'});}";
        assert!(install_scoped(&s, &mut w, &json!([]), false).is_err());
        assert!(install_scoped(&s, &mut w, &json!({"source":source}), false).is_err());
        assert!(w.program.is_none());
        assert!(install_scoped(&s,&mut w,&json!({"source":"function main(){return null;}","libraries":{"io2":{"write":"write_file"}}}),false).is_err());
    }




    #[test]
    fn source_diagnostics_preserve_the_rejected_program_and_existing_window() {
        let broken = include_str!("../agent_program/fixtures/virtual-body-missing-brace.js");
        let mut window = Window {
            program_task: Some(
                json!({"name":"author_pages","parameters":["args"],"arguments":[{}]}),
            ),
            ..Default::default()
        };
        let before = serde_json::to_value(&window).unwrap();
        let args = json!({"source":broken});
        let error = install_body(&mut window, &args).unwrap_err();
        let diagnostic = source_error("define_task_body", &args, &error);
        assert_eq!(diagnostic["code"], "PROGRAM_SOURCE_INVALID");
        assert_eq!(diagnostic["executed"], false);
        assert_eq!(diagnostic["retryable"], true);
        assert_eq!(diagnostic["diagnostics"][0]["line"], 17);
        assert!(diagnostic["diagnostics"][0]["column"].as_u64().unwrap() > 0);
        assert!(
            !diagnostic["diagnostics"][0]["source_excerpt"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(serde_json::to_value(&window).unwrap(), before);
        let fixed = format!("{broken}\n}}");
        install_body(&mut window, &json!({"source":fixed})).unwrap();
        assert!(window.program.is_some());

        let mut root = Window::default();
        let original = serde_json::to_value(&root).unwrap();
        assert!(
            install(
                &mut root,
                &json!({"source":"function main(){io.write({path:'never.txt'});"})
            )
            .is_err()
        );
        assert_eq!(serde_json::to_value(&root).unwrap(), original);
    }


    #[test]
    fn call_bindings_are_identical_for_every_parameter_name_and_original_order() {
        let value = json!({"pages":[{"path":"one.html"}],"input":{"legitimate":true}});
        for parameter in ["input", "args", "context", "bindings"] {
            let task = json!({"name":"work","description":"Use the supplied value","parameters":[parameter],"arguments":[value]});
            let view = call_view(&task).unwrap();
            assert_eq!(view["signature"], format!("function work({parameter})"));
            assert!(view.get("input").is_none());
            assert!(
                view.get("arguments").is_none(),
                "the view must not duplicate whole input bodies"
            );
            assert_eq!(view["bindings"][parameter], value);
            let mut window = Window {
                program_task: Some(task),
                ..Default::default()
            };
            install_body(
                &mut window,
                &json!({"source":format!("function work({parameter}){{return {parameter};}}") }),
            )
            .unwrap();
            assert!(
                matches!(window.program.as_mut().unwrap().next(),State::Returned(actual) if actual==value)
            );
        }
        let task =
            json!({"name":"work","parameters":["zeta","alpha"],"arguments":["first","second"]});
        let mut window = Window {
            program_task: Some(task),
            ..Default::default()
        };
        install_body(
            &mut window,
            &json!({"source":"function work(a,z){return [a,z];}"}),
        )
        .unwrap();
        assert!(
            matches!(window.program.as_mut().unwrap().next(),State::Returned(actual) if actual==json!(["first","second"]))
        );
        assert!(
            call_view(&json!({"name":"work","input":{"input":value}})).is_err(),
            "never guess the old envelope"
        );
    }

    #[test]
    fn refined_function_can_read_caller_variables_without_adding_parameters() {
        let mut window = Window {
            scope_values: BTreeMap::from([
                ("rules".into(), json!({"prefix":"from parent"})),
                ("unused".into(), json!("NOT_NEEDED")),
            ]),
            program_task: Some(json!({"name":"work","parameters":["item"],"arguments":["value"]})),
            ..Default::default()
        };
        install_body(
            &mut window,
            &json!({"source":"function work(item){return rules.prefix + ':' + item;}"}),
        )
        .unwrap();
        // Inherited names form the body's outermost scope.
        let runtime = window.program.as_mut().unwrap();
        assert!(matches!(runtime.next(), State::Returned(value) if value == "from parent:value"));
    }





    fn role_of(body: &Value) -> String {
        if body["messages"].as_array().unwrap().iter().any(|m| m["name"] == super::super::modules::FIX_INPUT) {
            return "fix".into();
        }
        body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_task_input")
            .map(|m| serde_json::from_str::<Value>(m["content"].as_str().unwrap()).unwrap()["role"].as_str().unwrap().to_owned())
            .unwrap_or_else(|| "main".into())
    }
    fn tool_names(body: &Value) -> Vec<String> {
        body["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_owned()).collect()
    }
    fn reply(content: String) -> Value {
        fixture_completion(&content)
    }
    const TWO_LEVEL_MAIN: &str = "```js\nimport * as io from \"host:io\";\nconst result = await summarize([\"a.txt\", \"b.txt\"]);\nio.write({path: \"out.txt\", content: result.summary});\n\n/** @module */\nasync function summarize(paths) {\n  // Summarize the files at paths. Returns {summary:string}.\n}\n```";

    #[tokio::test]
    async fn a_main_module_is_written_as_code_and_its_virtual_loops_with_imported_tools() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();
        std::fs::write(root.path().join("b.txt"), "beta").unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<String>, Value)>::new()));
        let log = seen.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let role = role_of(&body);
            let tools = tool_names(&body);
            let prompt = body["messages"][0]["content"].as_str().unwrap_or("").to_owned();
            let text = body.to_string();
            assert!(!text.contains("\"task_status\"") && !text.contains("read_scope"), "no task-tree status or scope reads reach the model");
            if role == "implement" {
                let headers = body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_tool_headers").unwrap()["content"].as_str().unwrap().to_owned();
                for main_only in ["read_asset(", "read_task_board(", "list_context_assets(", "read_session_history("] {
                    assert!(!headers.contains(main_only), "an implementation's libraries hold no {main_only}");
                }
                let input = body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_task_input").unwrap();
                let input: Value = serde_json::from_str(input["content"].as_str().unwrap()).unwrap();
                assert_eq!(input["signature"], "async function summarize(paths)", "the declared async is shown");
            }
            if role != "main" {
                for message in body["messages"].as_array().unwrap() {
                    assert!(message["role"] != "user" || message["name"] == "program_task_input",
                        "a virtual function sees only its own input: {message}");
                }
            }
            log.lock().unwrap().push((role.clone(), tools, json!(prompt)));
            let answer = match role.as_str() {
                "main" => reply(TWO_LEVEL_MAIN.into()),
                "implement" => reply("```js\nimport * as io from \"host:io\";\nasync function summarize(paths) {\n  const texts = [];\n  for (const path of paths) { texts.push(io.read({path: path}).content); }\n  return judge(texts);\n}\n/** @virtual */\nfunction judge(texts) {\n  // Join the texts into one summary. Returns {summary:string}.\n}\n```".into()),
                "leaf" => {
                    let input = body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_task_input").unwrap();
                    let input: Value = serde_json::from_str(input["content"].as_str().unwrap()).unwrap();
                    assert_eq!(input["bindings"]["texts"], json!(["alpha", "beta"]));
                    reply(json!({"summary":"alpha+beta"}).to_string())
                }
                other => panic!("unexpected role {other}"),
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state, "task".into(), "fake".into(), "Summarize a and b".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
        server.abort();
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "alpha+beta");
        // The program's reads and writes ran as its own steps: no AI window holds them.
        let conn = agent_service::open_db(root.path()).unwrap();
        let windows = conn.prepare("SELECT state FROM agent_runtime_windows WHERE task_id='task'").unwrap()
            .query_map([], |r| r.get::<_, String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        for window in windows {
            let window: Value = serde_json::from_str(&window).unwrap();
            // Only the AIs' own hand-overs of code are exchanges in a window.
            let messages = window["messages"].as_array().unwrap();
            let handed = messages.iter().flat_map(|m| m["tool_calls"].as_array().cloned().unwrap_or_default())
                .inspect(|call| assert_eq!(call["function"]["name"], WRITE_PROGRAM, "no synthesized program exchange in {window}"))
                .map(|call| call["id"].clone()).collect::<Vec<_>>();
            assert!(messages.iter().filter(|m| m["role"] == "tool").all(|m| handed.contains(&m["tool_call_id"])),
                "no synthesized program exchange in {window}");
        }
        let program_steps: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='tool/call' AND json_extract(data,'$.program_location') IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert!(program_steps >= 3, "the program's steps are still recorded, with their source location");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.iter().map(|(role, _, _)| role.as_str()).collect::<Vec<_>>(), ["main", "implement", "leaf"]);
        let (_, implement, prompt) = &seen[1];
        assert_eq!(implement, &["read_tool_contracts", "read_program_guide", "read_skill", "write_program"], "an implementation is code: it reads contracts, the guide and the skills its code will require, and hands its code over");
        assert!(prompt.as_str().unwrap().starts_with("You write one @module"));
        let (_, leaf, prompt) = &seen[2];
        assert!(prompt.as_str().unwrap().starts_with("You perform one @virtual function"));
        assert!(leaf.contains(&"read_file".to_owned()) && leaf.contains(&FIND.to_owned()));
        for retired in ["read_tool_contracts", "define_task_body", "invoke_library", "create_task", "read_scope", "read_asset"] {
            assert!(!leaf.contains(&retired.to_owned()), "leaf must not see {retired}");
        }
        let definitions = crate::agent_service::local_tools(&crate::tools::Workspace::new(root.path().to_path_buf()).unwrap(), None, false);
        let io = agent_program::libraries::catalog(&definitions, &json!([]))["io"].values().cloned().collect::<BTreeSet<_>>();
        assert_eq!(leaf.iter().filter(|name| *name != FIND && *name != ASK).cloned().collect::<BTreeSet<_>>(), io,
            "leaf tools are exactly the implementation's io import, plus finding methods and asking the user");
    }

    #[tokio::test]
    async fn a_leaf_that_outgrows_its_context_throws_to_its_coded_caller() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "alpha ".repeat(4000)).unwrap();
        std::fs::write(root.path().join("b.txt"), "beta").unwrap();
        let leaf_rounds = Arc::new(AtomicUsize::new(0));
        let rounds = leaf_rounds.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let answer = match role_of(&body).as_str() {
                "main" => reply(TWO_LEVEL_MAIN.into()),
                "implement" => reply("```js\nimport * as io from \"host:io\";\nasync function summarize(paths) {\n  try {\n    return judge(paths);\n  } catch (error) {\n    return {summary: error.code + \":\" + error.completed_writes.length};\n  }\n}\n/** @virtual */\nfunction judge(paths) {\n  // Read every file at paths and summarize them. Returns {summary:string}.\n}\n```".into()),
                "leaf" => {
                    let n = rounds.fetch_add(1, Ordering::SeqCst);
                    assert!(n < 40, "the host must end the leaf instead of folding its context");
                    let call = json!({"id":format!("read-{n}"),"type":"function","function":{"name":"read_file","arguments":json!({"path":"a.txt"}).to_string()}});
                    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[call]}}]})
                }
                other => panic!("unexpected role {other}"),
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        state.provider_routes.insert(state.provider_name.clone(), agent_service::AgentProviderRoute {
            prefix_system_messages: false, responses: false, url: state.provider_url.clone(), api_key: state.api_key.clone(),
            models: Default::default(), max_output_tokens: Default::default(),
            context_windows: [("fake".to_owned(), 64_000)].into_iter().collect(),
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state, "task".into(), "fake".into(), "Summarize a and b".into(), 60, CancellationToken::new(), 1, false).await.unwrap();
        server.abort();
        assert!(leaf_rounds.load(Ordering::SeqCst) > 1);
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "CONTEXT_OVERFLOW:0");
        assert_eq!(conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='context/step_fold' OR kind LIKE 'context/fold%'",[],|r|r.get::<_,i64>(0)).unwrap(), 0);
    }

    async fn run_scripted(root: &std::path::Path, answer: impl Fn(&Value) -> Value + Clone + Send + Sync + 'static) {
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let answer = answer(&body);
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root, listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state, "task".into(), "fake".into(), "Summarize a and b".into(), 20, CancellationToken::new(), 1, false).await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn main_program_results_go_back_to_the_model_before_the_next_program() {
        let root = TestRoot::new();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        run_scripted(root.path(), move |body| {
            match count.fetch_add(1, Ordering::SeqCst) {
                0 => reply("```js\nconsole.log(\"discovered: alpha\");\n```".into()),
                1 => {
                    assert!(body["messages"].as_array().unwrap().iter().any(|m|
                        m["name"] == "program_result" && m["content"].as_str().unwrap_or("").contains("discovered: alpha")));
                    reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"result.txt\", content: \"alpha\"});\n```".into())
                }
                2 => {
                    assert!(body["messages"].to_string().contains("result.txt"));
                    reply("Done after both programs.".into())
                }
                _ => panic!("unexpected additional request"),
            }
        }).await;
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        assert_eq!(std::fs::read_to_string(root.path().join("result.txt")).unwrap(), "alpha");
        let conn = agent_service::open_db(root.path()).unwrap();
        let count: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='program/result'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
        let answer: String = conn.query_row("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(answer, "Done after both programs.");
    }

    #[tokio::test]
    async fn a_long_console_line_reaches_the_next_model_request() {
        let root = TestRoot::new();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        run_scripted(root.path(), move |body| {
            match count.fetch_add(1, Ordering::SeqCst) {
                0 => reply("```js\nconsole.log('A'.repeat(7000) + 'END_OF_PAGE');\n```".into()),
                1 => {
                    let result = body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_result").unwrap()["content"].as_str().unwrap();
                    assert!(result.contains("END_OF_PAGE"));
                    assert!(!result.ends_with('…'));
                    reply("Done.".into())
                }
                _ => panic!("unexpected additional request"),
            }
        }).await;
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_leaf_finds_a_missing_method_for_its_own_loop_only() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();
        let leaf_tools = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let log = leaf_tools.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply(TWO_LEVEL_MAIN.into()),
            "implement" => reply("```js\nasync function summarize(paths) {\n  return judge(paths[0]);\n}\n/** @virtual */\nfunction judge(path) {\n  // Read the file at path. Returns {summary:string} with its text.\n}\n```".into()),
            _ => {
                let tools = tool_names(body);
                let mut log = log.lock().unwrap();
                log.push(tools);
                let call = |id: &str, name: &str, args: Value| json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                    "tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]});
                match log.len() {
                    1 => call("find", FIND, json!({"query":"read file"})),
                    2 => call("read", "read_file", json!({"path":"a.txt"})),
                    _ => reply(json!({"summary":"alpha"}).to_string()),
                }
            }
        }).await;
        let leaf_tools = leaf_tools.lock().unwrap();
        assert_eq!(leaf_tools[0], [FIND, ASK], "nothing imported: only finding methods and asking the user");
        assert!(leaf_tools[1].contains(&"read_file".to_owned()), "a found method becomes callable");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "alpha");
    }

    #[test]
    fn finding_methods_puts_the_best_described_first() {
        let root = TestRoot::new();
        let state = agent_service::tests::test_state(root.path(), "127.0.0.1:9".parse().unwrap());
        let cancel = CancellationToken::new();
        let s = Session { agent_id: None, task_node_id: None, plugin: None, state: &state, task: "task", model: "fake", turn: 1,
            cancel: &cancel, ban_execution: false, scope: "main".into(), parent: None, input_budget: INPUT_BUDGET };
        let leaf = Window { program_task: Some(json!({"role":"leaf","leaf_tools":[]})), ..Default::default() };
        // What a real leaf asked when it looked for a way to run its tests;
        // "and" and "code" match most methods.
        let found = find(&s, &leaf, &json!({"query":"read_skill and execute python code run tests"})).unwrap();
        assert_eq!(found["methods"][0]["method"], "process.run_program", "{found}");
    }

    #[test]
    fn the_main_ai_finds_methods_to_read_run_and_write() {
        let root = TestRoot::new();
        let state = agent_service::tests::test_state(root.path(), "127.0.0.1:9".parse().unwrap());
        let cancel = CancellationToken::new();
        let s = Session { agent_id: None, task_node_id: None, plugin: None, state: &state, task: "task", model: "fake", turn: 1,
            cancel: &cancel, ban_execution: false, scope: "main".into(), parent: None, input_budget: INPUT_BUDGET };
        let methods = |w: &Window| find(&s, w, &json!({"query":"write edit replace file"})).unwrap()["methods"].as_array().unwrap()
            .iter().map(|m| m["method"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        let main = methods(&Window::default());
        assert!(main.contains(&"io.write".to_owned()), "main can find writing tools: {main:?}");
        let leaf = methods(&Window { program_task: Some(json!({"role":"leaf","leaf_tools":[]})), ..Default::default() });
        assert!(leaf.contains(&"io.write".to_owned()), "a function can still find them: {leaf:?}");
        let marked = |text: &str| js_marker(&json!({"content":text}));
        assert!(marked("</thinking>```js\nx\n```") && marked("```JavaScript\n"));
        assert!(!marked("```json\n{}\n```") && !marked("plain text"));
    }

    #[tokio::test]
    async fn getting_to_know_the_task_reads_directly_and_ends_when_the_program_starts() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        run_scripted(root.path(), move |body| {
            let mut log = log.lock().unwrap();
            log.push(body.clone());
            let call = |id: &str, name: &str, args: Value| json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                "tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]});
            match log.len() {
                1 => call("write", "write_file", json!({"path":"early.txt","content":"x"})),
                2 => call("read", "read_file", json!({"path":"a.txt"})),
                3 => reply("```js\nconst broken = ;\n```".into()),
                4 => reply("```js\nthrow new Error(\"boom\");\n```".into()),
                _ => reply("The program failed: boom.".into()),
            }
        }).await;
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 5);
        let tools = tool_names(&bodies[0]);
        for name in FAMILIAR.iter().copied().chain([FIND, "read_session_history", "read_asset"]) {
            assert!(tools.contains(&name.to_owned()), "{name} while getting to know the task: {tools:?}");
        }
        let text = |body: &Value| body["messages"].to_string();
        let calls = |body: &Value| body["messages"].as_array().unwrap().iter()
            .flat_map(|m| m["tool_calls"].as_array().cloned().unwrap_or_default())
            .map(|c| c["function"]["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(std::fs::read_to_string(root.path().join("early.txt")).unwrap(), "x", "the main loop can write directly");
        assert_eq!(calls(&bodies[2]), ["write_file", "read_file"], "its own calls keep their names");
        // A program that does not compile has not started: the context stays.
        assert!(text(&bodies[3]).contains("PROGRAM_SOURCE_INVALID") && text(&bodies[3]).contains("alpha"));
        // Once a program started, what led to it is gone; the request and the program stay.
        let after = &bodies[4];
        assert_eq!(calls(after), [WRITE_PROGRAM], "only the program's own hand-over is left: {}", text(after));
        assert!(!text(after).contains("alpha") && !text(after).contains("PROGRAM_SOURCE_INVALID"), "{}", text(after));
        assert!(text(after).contains("Summarize a and b") && text(after).contains("throw new Error"), "{}", text(after));
        assert!(text(after).contains("PROGRAM_RUNTIME_ERROR"), "and how it ended");
        assert!(tool_names(after).contains(&"read_file".to_owned()), "it can look again before it rewrites");
    }

    #[tokio::test]
    async fn write_program_with_other_calls_does_not_run_and_the_host_says_so() {
        let root = TestRoot::new();
        let program = "```js\nimport * as io from \"host:io\";\nio.write({path: \"x.txt\", content: \"1\"});\n```";
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        run_scripted(root.path(), move |body| {
            let mut log = log.lock().unwrap();
            log.push(body.clone());
            match log.len() {
                1 => {
                    let mut message = fixture_reply(program);
                    message["tool_calls"].as_array_mut().unwrap().insert(0,
                        json!({"id":"look","type":"function","function":{"name":"list_dir","arguments":json!({"path":"."}).to_string()}}));
                    json!({"choices":[{"finish_reason":"tool_calls","message":message}]})
                }
                _ => reply(program.into()),
            }
        }).await;
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        let second = bodies[1]["messages"].to_string();
        assert!(second.contains("PROGRAM_NOT_RUN") && second.contains("only call in its reply"), "{second}");
        assert_eq!(std::fs::read_to_string(root.path().join("x.txt")).unwrap(), "1", "the program alone ran");
        let conn = agent_service::open_db(root.path()).unwrap();
        let started: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='program/start'", [], |r| r.get(0)).unwrap();
        assert_eq!(started, 1);
    }

    #[tokio::test]
    async fn a_written_program_is_a_file_in_the_projects_host_folder_and_runs_from_it() {
        let root = TestRoot::new();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        run_scripted(root.path(), move |body| {
            let mut log = log.lock().unwrap();
            log.push(body.clone());
            match log.len() {
                1 => reply("```js\nconst broken = ;\n```".into()),
                _ => reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"z.txt\", content: \"3\"});\n```".into()),
            }
        }).await;
        let name = root.path().file_name().unwrap().to_string_lossy().into_owned();
        let project = std::fs::read_dir(root.path().join(".codex-workspace-mcp/projects")).unwrap()
            .map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
        assert_eq!(project.len(), 1);
        let folder = project[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(folder.starts_with(&format!("{name}-")), "the folder is named after the project: {folder}");
        let chat = project[0].join("chats/task");
        // Each hand-over is its own file, the one that did not compile included.
        assert_eq!(std::fs::read_to_string(chat.join("turn-1.js")).unwrap(), "const broken = ;");
        assert!(std::fs::read_to_string(chat.join("turn-1-2.js")).unwrap().contains("z.txt"));
        assert_eq!(std::fs::read_to_string(root.path().join("z.txt")).unwrap(), "3");
        let second = bodies.lock().unwrap()[1]["messages"].to_string();
        assert!(second.contains("PROGRAM_SOURCE_INVALID"), "the compile error is the call's result: {second}");
        let conn = agent_service::open_db(root.path()).unwrap();
        let path: String = conn.query_row("SELECT json_extract(data,'$.path') FROM agent_task_events WHERE kind='program/start'", [], |r| r.get(0)).unwrap();
        assert!(path.ends_with("/chats/task/turn-1-2.js") && root.path().join(&path).exists(), "{path}");
        // The hand-over is recorded as a call and its result, like any tool.
        let (calls, results): (i64, i64) = conn.query_row(
            "SELECT (SELECT count(*) FROM agent_task_events WHERE kind='tool/call' AND json_extract(data,'$.name')='write_program'),
                    (SELECT count(*) FROM agent_task_events e WHERE kind='tool/result' AND EXISTS(SELECT 1 FROM agent_task_events c
                       WHERE c.kind='tool/call' AND json_extract(c.data,'$.callId')=json_extract(e.data,'$.message.toolCallId')
                       AND json_extract(c.data,'$.name')='write_program'))", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((calls, results), (2, 2));
    }

    /// The 2026-11-02 Qwen run: a write_program call cut off mid-string, with
    /// the service reporting an ordinary tool-call finish.
    #[tokio::test]
    async fn a_cut_off_call_runs_nothing_and_the_ai_is_told_so() {
        let root = TestRoot::new();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        run_scripted(root.path(), move |body| {
            let mut log = log.lock().unwrap();
            log.push(body.clone());
            match log.len() {
                1 => json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[
                    {"id":"early","type":"function","function":{"name":"write_file","arguments":json!({"path":"early.txt","content":"x"}).to_string()}},
                    {"id":"cut","type":"function","function":{"name":"write_program","arguments":"{\"source\": \"import * as io from \\\"host:io\\\";\\nio.write({path: \\\"<h3><"}}]}}]}),
                _ => reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"w.txt\", content: \"4\"});\n```".into()),
            }
        }).await;
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        let second = &bodies[1]["messages"];
        assert!(second.to_string().contains("TOOL_ARGUMENTS_TRUNCATED") && second.to_string().contains("NOT_RUN"), "{second}");
        // What goes back to the service is valid JSON in every call.
        for call in second.as_array().unwrap().iter().flat_map(|m| m["tool_calls"].as_array().cloned().unwrap_or_default()) {
            assert!(serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).is_ok(), "{call}");
        }
        assert!(!root.path().join("early.txt").exists(), "nothing in the cut-off reply ran");
        assert_eq!(std::fs::read_to_string(root.path().join("w.txt")).unwrap(), "4", "the AI called again and the program ran");
        let conn = agent_service::open_db(root.path()).unwrap();
        let kept: String = conn.query_row("SELECT json_extract(data,'$.arguments') FROM agent_task_events WHERE kind='tool/call' AND json_extract(data,'$.callId')='cut'", [], |r| r.get(0)).unwrap();
        assert!(kept.ends_with("<h3><"), "the record keeps what the model sent: {kept}");
    }

    /// A service that reports the output limit as it is: the reply runs
    /// nothing, the turn goes on, and the AI is told it was cut off.
    #[tokio::test]
    async fn a_length_finish_runs_nothing_and_the_turn_goes_on() {
        let root = TestRoot::new();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        run_scripted(root.path(), move |body| {
            let mut log = log.lock().unwrap();
            log.push(body.clone());
            match log.len() {
                1 => json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","content":null,"tool_calls":[
                    {"id":"whole","type":"function","function":{"name":"write_file","arguments":json!({"path":"early.txt","content":"x"}).to_string()}}]}}]}),
                2 => json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","content":"The five pages share one lay"}}]}),
                _ => reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"w.txt\", content: \"4\"});\n```".into()),
            }
        }).await;
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3);
        assert!(bodies[1]["messages"].to_string().contains("NOT_RUN"), "{}", bodies[1]["messages"]);
        assert!(bodies[2]["messages"].to_string().contains("REPLY_TRUNCATED"), "{}", bodies[2]["messages"]);
        assert!(!root.path().join("early.txt").exists(), "nothing in the cut-off reply ran");
        assert_eq!(std::fs::read_to_string(root.path().join("w.txt")).unwrap(), "4", "the AI called again and the program ran");
    }

    #[tokio::test]
    async fn long_cut_off_replies_leave_room_for_a_retry() {
        let root = TestRoot::new();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        run_scripted(root.path(), move |body| {
            let mut log = log.lock().unwrap();
            log.push(body.clone());
            match log.len() {
                1 => json!({"choices":[{"finish_reason":"length","message":{"role":"assistant",
                    "content":"X".repeat(750_000),"reasoning_content":"R".repeat(750_000)}}]}),
                2 => json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","content":null,
                    "tool_calls":[{"id":"whole","type":"function","function":{"name":"write_file",
                    "arguments":json!({"path":"early.txt","content":"Y".repeat(750_000)}).to_string()}}]}}]}),
                _ => reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"w.txt\", content: \"4\"});\n```".into()),
            }
        }).await;
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3, "both retries reached the model");
        assert!(bodies[1]["messages"].to_string().contains("REPLY_TRUNCATED"));
        assert!(bodies[2]["messages"].to_string().contains("NOT_RUN"));
        assert!(bodies[2]["messages"].to_string().len() < 50_000, "large failed output was not replayed");
        assert!(!root.path().join("early.txt").exists());
        assert_eq!(std::fs::read_to_string(root.path().join("w.txt")).unwrap(), "4");
        let conn = agent_service::open_db(root.path()).unwrap();
        let content_len: i64 = conn.query_row("SELECT length(json_extract(data,'$.message.content')) FROM agent_task_events WHERE kind='assistant/message' AND length(json_extract(data,'$.message.content')) > 700000 LIMIT 1", [], |r| r.get(0)).unwrap();
        let args_len: i64 = conn.query_row("SELECT length(json_extract(data,'$.arguments')) FROM agent_task_events WHERE kind='tool/call' AND json_extract(data,'$.callId')='whole'", [], |r| r.get(0)).unwrap();
        assert_eq!(content_len, 750_000, "the full reply remains in the record");
        assert!(args_len > 750_000, "the full tool arguments remain in the record");
    }

    #[tokio::test]
    async fn code_in_a_reply_is_an_answer_and_never_runs() {
        let root = TestRoot::new();
        let example = "Here is an example:\n```js\nimport * as io from \"host:io\";\nio.write({path: \"y.txt\", content: \"2\"});\n```";
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        run_scripted(root.path(), move |body| {
            log.lock().unwrap().push(body.clone());
            json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":example}}]})
        }).await;
        assert_eq!(bodies.lock().unwrap().len(), 1, "the reply ended the turn");
        assert!(!root.path().join("y.txt").exists(), "the example did not run");
        let conn = agent_service::open_db(root.path()).unwrap();
        let (started, shown): (i64, String) = conn.query_row(
            "SELECT (SELECT count(*) FROM agent_task_events WHERE kind='program/start'),
                    (SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' ORDER BY seq DESC LIMIT 1)",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(started, 0);
        assert_eq!(shown, example, "the user sees the code as written");
    }

    #[test]
    fn every_host_tool_belongs_to_a_named_library() {
        let root = TestRoot::new();
        let state = agent_service::tests::test_state(root.path(), "127.0.0.1:9".parse().unwrap());
        let cancel = CancellationToken::new();
        let s = Session { agent_id: None, task_node_id: None, plugin: None, state: &state, task: "task", model: "fake", turn: 1,
            cancel: &cancel, ban_execution: false, scope: "main".into(), parent: None, input_budget: INPUT_BUDGET };
        // The main window with a program to resume publishes the most tools;
        // a leaf adds its own finding and asking.
        let main = Window { resumable: Some("paused".into()), ..Default::default() };
        let leaf = Window { program_task: Some(json!({"role":"leaf","leaf_tools":[]})), ..Default::default() };
        for w in [&main, &leaf] {
            let unassigned = agent_program::libraries::unassigned(&tools(&s, w), &json!([]));
            assert!(unassigned.is_empty(), "no catch-all library: give these a library in libraries::builtin: {unassigned:?}");
        }
        let new_tool = [json!({"function":{"name":"brand_new_tool"}})];
        assert_eq!(agent_program::libraries::unassigned(&new_tool, &json!([])), ["brand_new_tool"], "a tool without a library is reported");
    }

    const CATCHING_MAIN: &str = "```js\nimport * as io from \"host:io\";\nlet text;\ntry {\n  text = (await summarize([\"a.txt\"])).summary;\n} catch (error) {\n  text = \"caught:\" + error.message;\n}\nio.write({path: \"out.txt\", content: text});\n\n/** @module */\nasync function summarize(paths) {\n  // Summarize the files at paths. Returns {summary:string}.\n}\n```";

    #[tokio::test]
    async fn a_module_must_be_code_and_a_fixer_edits_its_file_after_a_runtime_error() {
        let root = TestRoot::new();
        let authored = Arc::new(AtomicUsize::new(0));
        let rounds = authored.clone();
        let fixer = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = fixer.clone();
        run_scripted(root.path(), move |body| {
            let messages = body["messages"].as_array().unwrap();
            if let Some(input) = messages.iter().find(|m| m["name"] == super::super::modules::FIX_INPUT) {
                log.lock().unwrap().push(body.clone());
                let input: Value = serde_json::from_str(input["content"].as_str().unwrap()).unwrap();
                let path = input["module"]["path"].as_str().unwrap().to_owned();
                let results = messages.iter().filter(|m| m["role"] == "tool").count();
                let edit = |args: Value| json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                    "tool_calls":[{"id":format!("edit-{results}"),"type":"function","function":{"name":"edit_file","arguments":args.to_string()}}]}}]});
                return match results {
                    0 => edit(json!({"path":"elsewhere.js","edits":[{"old_text":"a","new_text":"b"}]})),
                    1 => edit(json!({"path":path,"edits":[{"old_text":"return {summary: missing.text};","new_text":"return {summary: \"fixed\";"}]})),
                    2 => edit(json!({"path":path,"edits":[{"old_text":"\"fixed\";","new_text":"\"fixed\"};"}]})),
                    _ => reply("Return a summary instead of reading a missing value.".into()),
                };
            }
            match role_of(body).as_str() {
                "main" => reply(CATCHING_MAIN.into()),
                _ => match rounds.fetch_add(1, Ordering::SeqCst) {
                    0 => reply("{\"summary\":\"answered without code\"}".into()),
                    1 => {
                        assert!(messages.iter().any(|m| m["name"] == "program_diagnostic" && m["content"].as_str().unwrap().contains("FUNCTION_IMPLEMENTATION_REQUIRED")));
                        reply("```js\nimport * as io from \"host:io\";\nasync function summarize(paths) {\n  io.write({path: \"partial.txt\", content: \"done\"});\n  const missing = null;\n  return {summary: missing.text};\n}\n```".into())
                    }
                    n => panic!("the module is written once and fixed in place: {n}"),
                },
            }
        }).await;
        assert_eq!(authored.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "fixed");
        assert_eq!(std::fs::read_to_string(root.path().join("partial.txt")).unwrap(), "done");
        let fixer = fixer.lock().unwrap();
        assert_eq!(fixer.len(), 4, "three edits and the reply that it is done");
        // The fixer has the module file and this call's problem, nothing else.
        let first = &fixer[0];
        assert_eq!(first["messages"][0]["content"], FIX_PROMPT, "the fixer's own prompt");
        assert_eq!(tool_names(first).into_iter().collect::<BTreeSet<_>>(), ["edit_file", "read_program_guide", "read_tool_contracts"].map(str::to_owned).into_iter().collect());
        let users = first["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "user").collect::<Vec<_>>();
        assert_eq!(users.len(), 1, "only the fix input: {users:?}");
        let input: Value = serde_json::from_str(users[0]["content"].as_str().unwrap()).unwrap();
        assert!(input["module"]["source"].as_str().unwrap().contains("missing.text"));
        assert_eq!(input["module"]["version"], 1);
        assert_eq!(input["call"]["bindings"]["paths"], json!(["a.txt"]));
        assert_eq!(input["completed_writes"], json!([{"tool":"write_file","path":"partial.txt"}]));
        assert!(input["error"]["message"].as_str().is_some_and(|text| !text.is_empty()), "{input}");
        let last_result = |body: &Value| body["messages"].as_array().unwrap().iter().rev().find(|m| m["role"] == "tool").unwrap()["content"].as_str().unwrap().to_owned();
        assert!(last_result(&fixer[1]).contains("only the module file"), "another file cannot be edited");
        assert!(last_result(&fixer[2]).contains("\"compiles\":false"), "an edit that breaks the code comes back with its diagnostic");
        assert!(last_result(&fixer[3]).contains("\"compiles\":true"));
        // The fix is the module's next version, and its file holds it.
        let conn = agent_service::open_db(root.path()).unwrap();
        let versions = conn.prepare("SELECT version,fixed FROM agent_modules WHERE task_id='task' ORDER BY version").unwrap()
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(versions, [(1, 0), (2, 1)]);
        let path = input["module"]["path"].as_str().unwrap();
        assert!(path.starts_with(".codex-workspace-mcp/projects/") && path.contains("/chats/task/modules/summarize-") && path.ends_with(".js"), "{path}");
        assert!(std::fs::read_to_string(root.path().join(path)).unwrap().contains("return {summary: \"fixed\"};"));
    }

    #[tokio::test]
    async fn a_module_that_cannot_be_fixed_gives_up_to_the_caller() {
        let root = TestRoot::new();
        let implement = Arc::new(AtomicUsize::new(0));
        let rounds = implement.clone();
        let fixes = Arc::new(AtomicUsize::new(0));
        let fixed = fixes.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply(CATCHING_MAIN.into()),
            "fix" => {
                fixed.fetch_add(1, Ordering::SeqCst);
                reply(json!({"$task_error":{"message":"no source files"}}).to_string())
            }
            _ => match rounds.fetch_add(1, Ordering::SeqCst) {
                0 => reply("```js\nasync function summarize(paths) {\n  throw new Error(\"boom\");\n}\n```".into()),
                n => panic!("the module is written once: {n}"),
            },
        }).await;
        assert_eq!(implement.load(Ordering::SeqCst), 1);
        assert_eq!(fixes.load(Ordering::SeqCst), 1, "its fixer gave up");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "caught:no source files");
        // The function's frame shows where it was called and that it failed.
        let conn = agent_service::open_db(root.path()).unwrap();
        let frame: Value = serde_json::from_str(&conn.query_row(
            "SELECT data FROM agent_task_events WHERE kind='task/node_returned' AND json_extract(data,'$.inputs.program_task.name')='summarize'",
            [], |r| r.get::<_, String>(0)).unwrap()).unwrap();
        assert_eq!(frame["status"], "failed");
        assert_eq!(frame["metadata"]["call_site"]["line"], 4, "the await summarize(...) line");
        assert!(frame["metadata"]["caller"]["task"].is_string(), "the frame knows its caller");
    }

    #[tokio::test]
    async fn a_nested_virtual_helper_runs_as_a_leaf_under_its_own_name() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();
        std::fs::write(root.path().join("b.txt"), "beta").unwrap();
        let leaf_names = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = leaf_names.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply(TWO_LEVEL_MAIN.into()),
            "implement" => reply("```js\nimport * as io from \"host:io\";\nasync function summarize(paths) {\n  /** @virtual */\n  function join(texts) {\n    // Join the texts. Returns {summary:string}.\n  }\n  const texts = [];\n  for (const path of paths) { texts.push(io.read({path: path}).content); }\n  return join(texts);\n}\n```".into()),
            _ => {
                let input = body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_task_input").unwrap();
                let input: Value = serde_json::from_str(input["content"].as_str().unwrap()).unwrap();
                log.lock().unwrap().push(input["name"].as_str().unwrap().to_owned());
                assert!(tool_names(body).contains(&"read_file".to_owned()), "the implementation's io import");
                reply(json!({"summary":input["bindings"]["texts"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect::<Vec<_>>().join("+")}).to_string())
            }
        }).await;
        assert_eq!(*leaf_names.lock().unwrap(), ["join"]);
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "alpha+beta");
    }

    #[test]
    fn only_a_write_program_call_carries_a_program_and_the_user_sees_its_words() {
        // Code in reply text is an answer: nothing in it is taken as a program.
        assert!(written_source(&json!({"role":"assistant","content":"```js\nconsole.log(1);\n```"})).is_none());
        assert!(shown_reply(&json!({"role":"assistant","content":"```js\nconsole.log(1);\n```"})).is_none());
        let message = json!({"role":"assistant","content":"Running it.","tool_calls":[
            {"id":"a","type":"function","function":{"name":"read_file","arguments":"{}"}},
            {"id":"b","type":"function","function":{"name":WRITE_PROGRAM,"arguments":json!({"source":"console.log(1);"}).to_string()}}]});
        assert_eq!(written_source(&message).unwrap().unwrap(), "console.log(1);");
        let shown = shown_reply(&message).unwrap();
        assert_eq!(shown["content"], "Running it.");
        assert_eq!(shown["tool_calls"].as_array().unwrap().len(), 1);
        assert!(!shown.to_string().contains("console.log"));
        let empty = json!({"tool_calls":[{"id":"c","type":"function","function":{"name":WRITE_PROGRAM,"arguments":"{}"}}]});
        assert!(written_source(&empty).unwrap().is_err());
    }

    #[tokio::test]
    async fn a_later_turn_reads_earlier_programs_from_the_conversation_history() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();
        std::fs::write(root.path().join("b.txt"), "beta").unwrap();
        let read_back = Arc::new(std::sync::Mutex::new(None::<Value>));
        let log = read_back.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let turn2 = body["messages"].to_string().contains("What did the last program do");
            let answer = match role_of(&body).as_str() {
                "main" if !turn2 => reply(TWO_LEVEL_MAIN.into()),
                "main" => {
                    let messages = body["messages"].as_array().unwrap();
                    match messages.iter().rev().find(|m| m["role"] == "tool") {
                        None => {
                            let text = body.to_string();
                            assert!(!text.contains("texts.push(io.read") && !text.contains("await summarize("), "no earlier code sits in the new turn's context");
                            let listed = messages.iter().find(|m| m["name"] == "chat_modules").expect("the chat's modules are listed");
                            assert!(listed["content"].as_str().unwrap().contains("/** @module */\nasync function summarize(paths) {\n  // Summarize the files at paths. Returns {summary:string}.\n}"), "{listed}");
                            assert!(tool_names(&body).contains(&"read_session_history".to_owned()), "the main AI reads history itself");
                            json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                                "tool_calls":[{"id":"history","type":"function","function":{"name":"read_session_history","arguments":json!({"programs":true,"turn":1}).to_string()}}]}}]})
                        }
                        Some(result) => {
                            *log.lock().unwrap() = Some(serde_json::from_str(result["content"].as_str().unwrap()).unwrap());
                            reply("It summarized a.txt and b.txt into out.txt.".into())
                        }
                    }
                }
                "implement" => reply("```js\nimport * as io from \"host:io\";\nasync function summarize(paths) {\n  const texts = [];\n  for (const path of paths) { texts.push(io.read({path: path}).content); }\n  return judge(texts);\n}\n/** @virtual */\nfunction judge(texts) {\n  // Join the texts into one summary. Returns {summary:string}.\n}\n```".into()),
                _ => reply(json!({"summary":"alpha+beta"}).to_string()),
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state.clone(), "task".into(), "fake".into(), "Summarize a and b".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
        run_task(state, "task".into(), "fake".into(), "What did the last program do?".into(), 10, CancellationToken::new(), 2, false).await.unwrap();
        server.abort();
        let read_back = read_back.lock().unwrap().clone().expect("the second turn read the history");
        let programs = read_back["programs"].as_array().unwrap();
        assert_eq!(programs.len(), 2, "the main program and the implementation it led to: {read_back}");
        assert!(programs[0]["function"].is_null() && programs[0]["source"].as_str().unwrap().contains("await summarize("));
        assert_eq!(programs[1]["function"], "summarize");
        assert!(programs[1]["source"].as_str().unwrap().contains("function judge("));
    }

    fn memory_of(body: &Value) -> Option<Value> {
        body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "conversation_memory")
            .map(|m| serde_json::from_str(m["content"].as_str().unwrap()).unwrap())
    }

    #[tokio::test]
    async fn a_program_outcome_is_the_turns_answer_and_what_the_next_turn_sees() {
        let root = TestRoot::new();
        let memory = Arc::new(std::sync::Mutex::new(None::<Value>));
        let log = memory.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let answer = if body["messages"].to_string().contains("And now?") {
                *log.lock().unwrap() = memory_of(&body);
                reply("Noted.".into())
            } else {
                reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"one.txt\", content: \"1\"});\nio.write({path: \"two.txt\", content: \"2\"});\n```".into())
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state.clone(), "task".into(), "fake".into(), "Write two files".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
        run_task(state, "task".into(), "fake".into(), "And now?".into(), 10, CancellationToken::new(), 2, false).await.unwrap();
        server.abort();
        let memory = memory.lock().unwrap().clone().expect("the next turn has the conversation memory");
        let entry = memory["entries"].as_array().unwrap().iter().find(|e| e["kind"] == "task").unwrap().clone();
        assert_eq!(entry["answer"], "程序已完成。\n写入：one.txt、two.txt", "{memory}");
    }

    #[tokio::test]
    async fn a_main_program_that_keeps_failing_ends_its_turn_and_the_conversation_goes_on() {
        let root = TestRoot::new();
        let main = Arc::new(AtomicUsize::new(0));
        let rounds = main.clone();
        let memory = Arc::new(std::sync::Mutex::new(None::<Value>));
        let log = memory.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let answer = if body["messages"].to_string().contains("What happened?") {
                *log.lock().unwrap() = memory_of(&body);
                reply("It stopped with an error.".into())
            } else {
                rounds.fetch_add(1, Ordering::SeqCst);
                reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"done.txt\", content: \"1\"});\nthrow new Error(\"boom\");\n```".into())
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state.clone(), "task".into(), "fake".into(), "Do it".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
        run_task(state, "task".into(), "fake".into(), "What happened?".into(), 10, CancellationToken::new(), 2, false).await.unwrap();
        server.abort();
        assert_eq!(main.load(Ordering::SeqCst), 4, "the first program and three rewrites");
        let memory = memory.lock().unwrap().clone().expect("the next turn reached the model instead of re-running the failure");
        let entry = memory["entries"].as_array().unwrap().iter().find(|e| e["kind"] == "task").unwrap().clone();
        assert_eq!(entry["answer"], "程序在第 3 行出错停止：boom\n之后的步骤没有执行。\n写入：done.txt", "{memory}");
        assert_eq!(conn.query_row("SELECT status FROM agent_tasks WHERE id='task'", [], |r| r.get::<_, String>(0)).unwrap(), "completed");
    }

    #[tokio::test]
    async fn a_leaf_question_stops_the_program_and_the_answer_starts_a_new_turn() {
        let root = TestRoot::new();
        let implement = Arc::new(AtomicUsize::new(0));
        let rounds = implement.clone();
        let memory = Arc::new(std::sync::Mutex::new(None::<Value>));
        let log = memory.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let answer = match role_of(&body).as_str() {
                "main" if body["messages"].to_string().contains("Yes, keep both") => {
                    *log.lock().unwrap() = memory_of(&body);
                    reply("Keeping both.".into())
                }
                "main" => reply(TWO_LEVEL_MAIN.into()),
                "implement" => {
                    assert_eq!(rounds.fetch_add(1, Ordering::SeqCst), 0, "a question is not handed back to the author as a bug");
                    reply("```js\nasync function summarize(paths) {\n  return judge(paths);\n}\n/** @virtual */\nfunction judge(paths) {\n  // Decide which files to keep. Returns {summary:string}.\n}\n```".into())
                }
                _ => {
                    assert!(tool_names(&body).contains(&ASK.to_owned()), "a leaf can ask the user");
                    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                        "tool_calls":[{"id":"ask","type":"function","function":{"name":ASK,"arguments":json!({"question":"Keep both files?"}).to_string()}}]}}]})
                }
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state.clone(), "task".into(), "fake".into(), "Summarize a and b".into(), 20, CancellationToken::new(), 1, false).await.unwrap();
        let shown: String = conn.query_row("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' AND json_extract(data,'$.actor')='program' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(shown, "程序停在 第 2 行 summarize([\"a.txt\",\"b.txt\"]) → 第 2 行 judge([\"a.txt\",\"b.txt\"])，等你回答：Keep both files?\n之后的步骤没有执行。");
        assert!(!root.path().join("out.txt").exists(), "nothing after the question ran");
        run_task(state, "task".into(), "fake".into(), "Yes, keep both".into(), 10, CancellationToken::new(), 2, false).await.unwrap();
        server.abort();
        let memory = memory.lock().unwrap().clone().expect("the answer reached the main AI in a new turn");
        assert!(memory.to_string().contains("等你回答：Keep both files?"), "the new turn knows what was asked: {memory}");
    }

    #[tokio::test]
    async fn code_that_catches_errors_can_tell_a_question_apart_and_rethrow_it() {
        let root = TestRoot::new();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply(TWO_LEVEL_MAIN.into()),
            "implement" => reply("```js\nasync function summarize(paths) {\n  try {\n    return judge(paths);\n  } catch (error) {\n    if (error.name === \"WaitingForUser\") throw error;\n    return {summary: \"fallback\"};\n  }\n}\n/** @virtual */\nfunction judge(paths) {\n  // Decide which files to keep. Returns {summary:string}.\n}\n```".into()),
            _ => json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                "tool_calls":[{"id":"ask","type":"function","function":{"name":ASK,"arguments":json!({"question":"Keep both files?"}).to_string()}}]}}]}),
        }).await;
        let conn = agent_service::open_db(root.path()).unwrap();
        let shown: String = conn.query_row("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' AND json_extract(data,'$.actor')='program' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(shown, "程序停在 第 2 行 summarize([\"a.txt\",\"b.txt\"]) → 第 3 行 judge([\"a.txt\",\"b.txt\"])，等你回答：Keep both files?\n之后的步骤没有执行。", "the catch-all let the question through");
        assert!(!root.path().join("out.txt").exists(), "the fallback did not run");
    }

    const PAUSED_AT: &str = "程序被你的新消息暂停，停在 第 2 行 summarize([\"a.txt\",\"b.txt\"]) → 第 3 行 judge([\"a.txt\",\"b.txt\"])（AI 处理中），这一步还没完成。";
    const RESUMING_MAIN: &str = "```js\nimport * as io from \"host:io\";\nimport * as previous from \"host:previous/promises\";\nconst old = previous.resume();\nio.write({path: \"extra.txt\", content: \"extra\"});\nawait old;\n```";
    const CATCHING_IMPLEMENT: &str = "```js\nasync function summarize(paths) {\n  try {\n    return judge(paths);\n  } catch (error) {\n    return {summary: \"fallback\"};\n  }\n}\n/** @virtual */\nfunction judge(paths) {\n  // Decide which files to keep. Returns {summary:string}.\n}\n```";
    fn has_tool_result(body: &Value) -> bool {
        body["messages"].as_array().is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"))
    }
    fn find_call() -> Value {
        json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"find","type":"function","function":{"name":FIND,"arguments":json!({"query":"read"}).to_string()}}]}}]})
    }
    fn program_lines(conn: &rusqlite::Connection) -> Vec<String> {
        conn.prepare("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' AND json_extract(data,'$.actor')='program' ORDER BY seq").unwrap()
            .query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
    }
    fn paused_nodes(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM agent_task_nodes WHERE task_id='task' AND status IN ('paused','running','suspended') AND json_extract(data,'$.metadata.parallel')=1", [], |r| r.get(0)).unwrap()
    }

    #[tokio::test]
    async fn a_user_message_pauses_the_program_and_the_new_one_resumes_it() {
        let root = TestRoot::new();
        let path = root.path().to_owned();
        let leaf = Arc::new(AtomicUsize::new(0));
        let leaf_rounds = leaf.clone();
        let heard = Arc::new(std::sync::Mutex::new(None::<String>));
        let log = heard.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" if body["messages"].to_string().contains("程序被你的新消息暂停") => {
                *log.lock().unwrap() = Some(body["messages"].to_string());
                reply(RESUMING_MAIN.into())
            }
            "main" => reply(TWO_LEVEL_MAIN.into()),
            "implement" => reply(CATCHING_IMPLEMENT.into()),
            _ if has_tool_result(body) => {
                leaf_rounds.fetch_add(1, Ordering::SeqCst);
                assert!(!body["messages"].to_string().contains("Also write extra"), "the leaf sees only its own arguments");
                reply(json!({"summary":"kept both"}).to_string())
            }
            _ => {
                leaf_rounds.fetch_add(1, Ordering::SeqCst);
                crate::interjections::submit(&path, "task", "Also write extra").unwrap();
                find_call()
            }
        }).await;
        let heard = heard.lock().unwrap().clone().expect("the main AI heard the message after the outcome");
        assert!(heard.contains("Also write extra"), "{heard}");
        assert_eq!(leaf.load(Ordering::SeqCst), 2, "the leaf went on from its next step");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "kept both", "the paused program finished as planned");
        assert!(root.path().join("extra.txt").exists(), "the new work ran too");
        let conn = agent_service::open_db(root.path()).unwrap();
        let lines = program_lines(&conn);
        assert!(lines.iter().any(|line| line.starts_with(PAUSED_AT)), "{lines:?}");
        assert!(lines.last().unwrap().starts_with("程序已完成。"), "{lines:?}");
        assert_eq!(paused_nodes(&conn), 0, "nothing is left paused");
    }

    #[tokio::test]
    async fn a_resumed_program_can_rewrite_a_file_it_wrote_before_the_pause() {
        const DRAFTING_MAIN: &str = "```js\nimport * as io from \"host:io\";\nio.write({path: \"out.txt\", content: \"draft\"});\nconst result = await summarize([\"a.txt\", \"b.txt\"]);\nio.write({path: \"out.txt\", content: result.summary});\n\n/** @module */\nasync function summarize(paths) {\n  // Summarize the files at paths. Returns {summary:string}.\n}\n```";
        const WRITE_THEN_RESUME: &str = "```js\nimport * as io from \"host:io\";\nimport * as previous from \"host:previous/promises\";\nio.write({path: \"extra.txt\", content: \"extra\"});\nawait previous.resume();\n```";
        let root = TestRoot::new();
        let path = root.path().to_owned();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" if body["messages"].to_string().contains("程序被你的新消息暂停") => reply(WRITE_THEN_RESUME.into()),
            "main" => reply(DRAFTING_MAIN.into()),
            "implement" => reply(CATCHING_IMPLEMENT.into()),
            _ if has_tool_result(body) => reply(json!({"summary":"kept both"}).to_string()),
            _ => {
                crate::interjections::submit(&path, "task", "Also write extra").unwrap();
                find_call()
            }
        }).await;
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "kept both", "its own earlier write does not block it");
        assert!(root.path().join("extra.txt").exists());
        let conn = agent_service::open_db(root.path()).unwrap();
        let version = |file: &str| -> i64 {
            conn.query_row("SELECT MIN(seq) FROM agent_file_writer_versions WHERE task_id='task' AND path=?1", [file], |r| r.get(0)).unwrap()
        };
        let paused: i64 = conn.query_row("SELECT json_extract(data,'$.metadata.file_write_clock') FROM agent_task_nodes WHERE task_id='task' AND json_extract(data,'$.metadata.previous')=1", [], |r| r.get(0)).unwrap();
        assert!(version("out.txt") <= paused && paused < version("extra.txt"), "the paused program starts from the moment it paused: {paused}");
    }

    #[tokio::test]
    async fn a_paused_program_not_resumed_is_discarded_with_the_turn() {
        let root = TestRoot::new();
        let path = root.path().to_owned();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" if body["messages"].to_string().contains("程序被你的新消息暂停") => reply("Stopped as you asked.".into()),
            "main" => reply(TWO_LEVEL_MAIN.into()),
            "implement" => reply(CATCHING_IMPLEMENT.into()),
            _ => {
                assert!(!has_tool_result(body), "a discarded leaf does not run on");
                crate::interjections::submit(&path, "task", "Stop").unwrap();
                find_call()
            }
        }).await;
        assert!(!root.path().join("out.txt").exists(), "neither the catch-all nor the rest of the program ran");
        let conn = agent_service::open_db(root.path()).unwrap();
        let answer: String = conn.query_row("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(answer, "Stopped as you asked.");
        assert!(program_lines(&conn).iter().any(|line| line.starts_with(PAUSED_AT)));
        assert_eq!(paused_nodes(&conn), 0, "the paused frames were discarded");
        let discarded: i64 = conn.query_row("SELECT count(*) FROM agent_task_nodes WHERE task_id='task' AND status='interrupted'", [], |r| r.get(0)).unwrap();
        assert!(discarded >= 3, "the paused program, its function and its leaf: {discarded}");
        assert_eq!(conn.query_row("SELECT status FROM agent_tasks WHERE id='task'", [], |r| r.get::<_, String>(0)).unwrap(), "completed");
    }

    #[tokio::test]
    async fn paused_concurrent_calls_resume_where_they_stopped() {
        let root = TestRoot::new();
        let path = root.path().to_owned();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" if body["messages"].to_string().contains("程序被你的新消息暂停") =>
                reply("```js\nimport * as previous from \"host:previous/promises\";\nawait previous.resume();\n```".into()),
            "main" => reply("```js\nimport * as io from \"host:io\";\nconst kept = await Promise.all([judge(\"a.txt\"), judge(\"b.txt\")]);\nio.write({path: \"out.txt\", content: JSON.stringify(kept)});\n\n/** @module */\nasync function judge(path) {\n  // Decide whether to keep the file at path. Returns {keep:boolean}.\n}\n```".into()),
            "implement" => reply("```js\nasync function judge(path) {\n  return decide(path);\n}\n/** @virtual */\nfunction decide(path) {\n  // Decide whether to keep the file. Returns {keep:boolean}.\n}\n```".into()),
            _ if has_tool_result(body) => reply(json!({"keep":true}).to_string()),
            _ => {
                crate::interjections::submit(&path, "task", "Carry on").unwrap();
                find_call()
            }
        }).await;
        let conn = agent_service::open_db(root.path()).unwrap();
        let lines = program_lines(&conn);
        assert!(lines.iter().any(|line| line.starts_with("程序被你的新消息暂停，停在 第 2 行 judge(\"a.txt\") → 第 2 行 decide(\"a.txt\")（AI 处理中）、第 2 行 judge(\"b.txt\") → 第 2 行 decide(\"b.txt\")（AI 处理中），这一步还没完成。")), "{lines:?}");
        let concurrent: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE data LIKE '%parallel_call_id%'", [], |r| r.get(0)).unwrap();
        assert!(concurrent > 0, "the calls ran concurrently");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), r#"[{"keep":true},{"keep":true}]"#, "{lines:?}");
        assert_eq!(paused_nodes(&conn), 0);
    }

    #[tokio::test]
    async fn a_resumed_program_pauses_again_and_resumes_inside_the_next_one() {
        let root = TestRoot::new();
        let path = root.path().to_owned();
        let leaf = Arc::new(AtomicUsize::new(0));
        let leaf_rounds = leaf.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" if body["messages"].to_string().matches("程序被你的新消息暂停").count() >= 2 =>
                reply("```js\nimport * as previous from \"host:previous/promises\";\nawait previous.resume();\n```".into()),
            "main" if body["messages"].to_string().contains("程序被你的新消息暂停") => reply(RESUMING_MAIN.into()),
            "main" => reply(TWO_LEVEL_MAIN.into()),
            "implement" => reply(CATCHING_IMPLEMENT.into()),
            _ => match leaf_rounds.fetch_add(1, Ordering::SeqCst) {
                0 => { crate::interjections::submit(&path, "task", "Also write extra").unwrap(); find_call() }
                1 => { crate::interjections::submit(&path, "task", "Keep going").unwrap(); find_call() }
                _ => reply(json!({"summary":"kept both"}).to_string()),
            },
        }).await;
        assert_eq!(leaf.load(Ordering::SeqCst), 3, "the leaf went on after each pause");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "kept both");
        assert!(root.path().join("extra.txt").exists());
        let conn = agent_service::open_db(root.path()).unwrap();
        let lines = program_lines(&conn);
        assert_eq!(lines.iter().filter(|line| line.starts_with("程序被你的新消息暂停")).count(), 2, "{lines:?}");
        assert!(lines.iter().any(|line| line.starts_with("程序被你的新消息暂停，停在 第 3 行 previous.resume() → 第 2 行 summarize([\"a.txt\",\"b.txt\"]) → 第 3 行 judge([\"a.txt\",\"b.txt\"])（AI 处理中），这一步还没完成。")), "the second pause is in the new program: {lines:?}");
        assert_eq!(paused_nodes(&conn), 0);
    }

    #[tokio::test]
    async fn a_question_asked_after_resuming_reaches_the_user() {
        let root = TestRoot::new();
        let path = root.path().to_owned();
        let leaf = Arc::new(AtomicUsize::new(0));
        let leaf_rounds = leaf.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" if body["messages"].to_string().contains("程序被你的新消息暂停") =>
                reply("```js\nimport * as previous from \"host:previous/promises\";\nawait previous.resume();\n```".into()),
            "main" => reply(TWO_LEVEL_MAIN.into()),
            "implement" => reply("```js\nasync function summarize(paths) {\n  return judge(paths);\n}\n/** @virtual */\nfunction judge(paths) {\n  // Decide which files to keep. Returns {summary:string}.\n}\n```".into()),
            _ => match leaf_rounds.fetch_add(1, Ordering::SeqCst) {
                0 => { crate::interjections::submit(&path, "task", "Carry on").unwrap(); find_call() }
                _ => json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                    "tool_calls":[{"id":"ask","type":"function","function":{"name":ASK,"arguments":json!({"question":"Keep both files?"}).to_string()}}]}}]}),
            },
        }).await;
        let conn = agent_service::open_db(root.path()).unwrap();
        let lines = program_lines(&conn);
        assert_eq!(lines.last().unwrap(), "程序停在 第 2 行 previous.resume() → 第 2 行 summarize([\"a.txt\",\"b.txt\"]) → 第 2 行 judge([\"a.txt\",\"b.txt\"])，等你回答：Keep both files?\n之后的步骤没有执行。", "{lines:?}");
        assert!(!root.path().join("out.txt").exists());
        assert_eq!(paused_nodes(&conn), 0);
    }

    #[tokio::test]
    async fn calls_cut_off_by_a_crash_are_not_run_again() {
        let root = TestRoot::new();
        let leaf = Arc::new(AtomicUsize::new(0));
        let leaf_requests = leaf.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let leaf_requests = leaf_requests.clone();
            async move {
                Json(match role_of(&body).as_str() {
                    "main" if body["messages"].to_string().contains("PROGRAM_RUNTIME_ERROR") => reply("Noted.".into()),
                    "main" => reply("```js\nconst kept = await Promise.all([judge(\"a.txt\"), judge(\"b.txt\")]);\n\n/** @module */\nasync function judge(path) {\n  // Decide whether to keep the file at path. Returns {keep:boolean}.\n}\n```".into()),
                    "implement" => reply("```js\nasync function judge(path) {\n  return decide(path);\n}\n/** @virtual */\nfunction decide(path) {\n  // Decide whether to keep the file. Returns {keep:boolean}.\n}\n```".into()),
                    _ => {
                        // The process dies while the leaves wait for the model.
                        leaf_requests.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        reply(json!({"keep":true}).to_string())
                    }
                })
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        // Cut the first turn off once both leaves wait for the model.
        let waiting = leaf.clone();
        tokio::select! {
            _ = run_task(state.clone(), "task".into(), "fake".into(), "Judge a and b".into(), 20, CancellationToken::new(), 1, false) => panic!("the first turn was not cut off"),
            _ = async { while waiting.load(Ordering::SeqCst) < 2 { tokio::time::sleep(Duration::from_millis(20)).await; } } => {}
        }
        assert_eq!(leaf.load(Ordering::SeqCst), 2);
        run_task(state, "task".into(), "fake".into(), "Go on".into(), 20, CancellationToken::new(), 2, false).await.unwrap();
        server.abort();
        assert_eq!(leaf.load(Ordering::SeqCst), 2, "a call cut off mid-step is not run again");
        let error: String = conn.query_row("SELECT data FROM agent_task_events WHERE kind='program/runtime_error' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert!(error.contains("interrupted"), "the cut-off calls failed instead: {error}");
    }

    #[tokio::test]
    async fn a_question_from_a_concurrent_call_says_where_it_stopped() {
        let root = TestRoot::new();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply("```js\nimport * as io from \"host:io\";\nconst kept = await Promise.all([judge(\"a.txt\"), judge(\"b.txt\")]);\nio.write({path: \"out.txt\", content: JSON.stringify(kept)});\n\n/** @module */\nasync function judge(path) {\n  // Decide whether to keep the file at path. Returns {keep:boolean}.\n}\n```".into()),
            "implement" => reply("```js\nasync function judge(path) {\n  return decide(path);\n}\n/** @virtual */\nfunction decide(path) {\n  // Decide whether to keep the file. Returns {keep:boolean}.\n}\n```".into()),
            _ if body["messages"].to_string().contains("a.txt") => json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                "tool_calls":[{"id":"ask","type":"function","function":{"name":ASK,"arguments":json!({"question":"Keep a.txt?"}).to_string()}}]}}]}),
            _ => reply(json!({"keep":true}).to_string()),
        }).await;
        let conn = agent_service::open_db(root.path()).unwrap();
        let lines = program_lines(&conn);
        assert_eq!(lines.last().unwrap(), "程序停在 第 2 行 judge(\"a.txt\") → 第 2 行 decide(\"a.txt\")，等你回答：Keep a.txt?\n之后的步骤没有执行。", "{lines:?}");
        assert!(!root.path().join("out.txt").exists());
    }

    #[tokio::test]
    async fn a_failed_main_program_heals_in_the_same_turn() {
        let root = TestRoot::new();
        run_scripted(root.path(), move |body| {
            let messages = body["messages"].to_string();
            if messages.contains("PROGRAM_RUNTIME_ERROR") {
                assert!(messages.contains("draft.txt"), "the main AI sees what was already written: {messages}");
                reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"done.txt\", content: \"fixed\"});\n```".into())
            } else {
                reply("```js\nimport * as io from \"host:io\";\nio.write({path: \"draft.txt\", content: \"1\"});\nthrow new Error(\"boom\");\n```".into())
            }
        }).await;
        assert_eq!(std::fs::read_to_string(root.path().join("done.txt")).unwrap(), "fixed");
        let conn = agent_service::open_db(root.path()).unwrap();
        let answer: String = conn.query_row("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(answer, "程序已完成。\n写入：draft.txt、done.txt");
    }

    #[tokio::test]
    async fn a_virtual_sees_what_its_contract_names_with_uses() {
        let root = TestRoot::new();
        let seen = Arc::new(std::sync::Mutex::new(None::<String>));
        let log = seen.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply("```js\nimport * as io from \"host:io\";\nconst brief = \"Only summarize a\";\nconst result = await summarize([\"a.txt\", \"b.txt\"]);\nio.write({path: \"out.txt\", content: result.summary});\n\n/**\n * @virtual\n * Summarize the files at paths as the brief asks.\n * @uses brief the user's request, verbatim\n * Returns {summary:string}.\n */\nasync function summarize(paths) {}\n```".into()),
            role => {
                assert_eq!(role, "leaf", "a @virtual in the main program is a hook, not code");
                *log.lock().unwrap() = Some(body["messages"].to_string());
                reply(json!({"summary":"Only summarize a: a.txt"}).to_string())
            }
        }).await;
        let seen = seen.lock().unwrap().clone().expect("the hook was asked");
        assert!(seen.contains("closure") && seen.contains("Only summarize a"), "the AI sees the captured value: {seen}");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "Only summarize a: a.txt");
    }

    #[tokio::test]
    async fn console_output_reaches_the_ai_that_wrote_the_program() {
        let root = TestRoot::new();
        let healed = Arc::new(std::sync::Mutex::new(None::<String>));
        let log = healed.clone();
        run_scripted(root.path(), move |body| {
            let messages = body["messages"].to_string();
            if messages.contains("PROGRAM_RUNTIME_ERROR") {
                *log.lock().unwrap() = Some(messages);
                reply("```js\nconsole.log(\"checked\", 2, \"files\");\nconsole.warn(\"b.txt is empty\");\n```".into())
            } else {
                reply("```js\nconsole.log(\"found\", {files: 2});\nthrow new Error(\"boom\");\n```".into())
            }
        }).await;
        let healed = healed.lock().unwrap().clone().expect("the error came back to heal");
        assert!(healed.contains("found {") && healed.contains("files"), "the diagnostic carries what it printed: {healed}");
        let conn = agent_service::open_db(root.path()).unwrap();
        let answer: String = conn.query_row("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE kind='assistant/message' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(answer, "程序已完成。", "the user sees the outcome without what the program printed for the AI");
        let kept: String = conn.query_row("SELECT json_extract(data,'$.result.result') FROM agent_task_nodes WHERE task_id='task' AND json_extract(data,'$.result.result') LIKE '%输出%'", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, "程序已完成。\n输出：\nchecked 2 files\n[warn] b.txt is empty", "the AI's record of the turn keeps it");
        let shown_code: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='assistant/message' AND json_extract(data,'$.message.content') LIKE '%```js%'", [], |r| r.get(0)).unwrap();
        assert_eq!(shown_code, 0, "programs are records, not shown replies");
    }

    const SHOUT: &str = "/** @module */\nasync function shout(word) {\n  // Return {text:string}: word in capitals.\n}";
    const SHOUT_CODE: &str = "```js\nasync function shout(word) {\n  return {text: word.toUpperCase()};\n}\n```";

    fn module_files(root: &std::path::Path) -> Vec<String> {
        let dir = super::super::host_folder::chat_dir(root, "task").unwrap().join("modules");
        let mut names = std::fs::read_dir(&dir).map(|entries| entries.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect::<Vec<_>>()).unwrap_or_default();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_module_is_written_once_and_later_calls_run_its_code() {
        let root = TestRoot::new();
        let authors = Arc::new(AtomicUsize::new(0));
        let count = authors.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply(format!("```js\nimport * as io from \"host:io\";\nconst a = await shout(\"a\");\nconst b = await shout(\"b\");\nio.write({{path: \"out.txt\", content: a.text + b.text}});\n\n{SHOUT}\n```")),
            "implement" => {
                count.fetch_add(1, Ordering::SeqCst);
                reply(SHOUT_CODE.into())
            }
            other => panic!("unexpected request: {other}"),
        }).await;
        assert_eq!(authors.load(Ordering::SeqCst), 1, "the second call runs the saved code");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "AB");
        let conn = agent_service::open_db(root.path()).unwrap();
        let loaded: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='module/loaded'", [], |r| r.get(0)).unwrap();
        assert_eq!(loaded, 1);
        let (version, source): (i64, String) = conn.query_row("SELECT version,source FROM agent_modules WHERE task_id='task'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(version, 1);
        let files = module_files(root.path());
        assert!(files.len() == 1 && files[0].starts_with("shout-") && files[0].ends_with(".js"), "{files:?}");
        let dir = super::super::host_folder::chat_dir(root.path(), "task").unwrap().join("modules");
        assert!(dir.starts_with(root.path().join(".codex-workspace-mcp/projects")), "modules live in the host's project folder: {dir:?}");
        assert_eq!(std::fs::read_to_string(dir.join(&files[0])).unwrap(), source);
    }

    #[tokio::test]
    async fn concurrent_first_calls_write_a_module_once() {
        let root = TestRoot::new();
        let authors = Arc::new(AtomicUsize::new(0));
        let count = authors.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply(format!("```js\nimport * as io from \"host:io\";\nconst [a, b] = await Promise.all([shout(\"a\"), shout(\"b\")]);\nio.write({{path: \"out.txt\", content: a.text + b.text}});\n\n{SHOUT}\n```")),
            "implement" => {
                count.fetch_add(1, Ordering::SeqCst);
                reply(SHOUT_CODE.into())
            }
            other => panic!("unexpected request: {other}"),
        }).await;
        assert_eq!(authors.load(Ordering::SeqCst), 1, "one call writes the module and the other waits for it");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "AB");
    }

    #[tokio::test]
    async fn calls_that_fail_on_the_same_version_wait_for_one_fix() {
        let root = TestRoot::new();
        let fixes = Arc::new(AtomicUsize::new(0));
        let fixer_requests = fixes.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let fixer_requests = fixer_requests.clone();
            async move {
                let messages = body["messages"].as_array().unwrap().clone();
                let answer = match role_of(&body).as_str() {
                    "main" => reply(format!("```js\nimport * as io from \"host:io\";\nconst [a, b] = await Promise.all([shout(\"a\"), shout(\"b\")]);\nio.write({{path: \"out.txt\", content: a.text + b.text}});\n\n{SHOUT}\n```")),
                    "implement" => reply("```js\nasync function shout(word) {\n  const missing = null;\n  return {text: missing.text};\n}\n```".into()),
                    "fix" => {
                        fixer_requests.fetch_add(1, Ordering::SeqCst);
                        // Slower than one wait, so the other call checks back while this fix is under way.
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        let input = messages.iter().find(|m| m["name"] == super::super::modules::FIX_INPUT).unwrap();
                        let input: Value = serde_json::from_str(input["content"].as_str().unwrap()).unwrap();
                        let path = input["module"]["path"].as_str().unwrap();
                        if messages.iter().any(|m| m["role"] == "tool") {
                            reply("Return the word in capitals instead of reading a missing value.".into())
                        } else {
                            let edit = json!({"path":path,"edits":[{"old_text":"  const missing = null;\n  return {text: missing.text};","new_text":"  return {text: word.toUpperCase()};"}]});
                            json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                                "tool_calls":[{"id":"edit","type":"function","function":{"name":"edit_file","arguments":edit.to_string()}}]}}]})
                        }
                    }
                    other => panic!("unexpected request: {other}"),
                };
                Json(answer)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state, "task".into(), "fake".into(), "Shout a and b".into(), 20, CancellationToken::new(), 1, false).await.unwrap();
        server.abort();
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "AB");
        assert_eq!(fixes.load(Ordering::SeqCst), 2, "one fixer: its edit and its reply that it is done");
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT count(*) FROM agent_task_events WHERE kind='module/fix_started'"), 1, "the version was fixed once");
        assert_eq!(count("SELECT count(*) FROM agent_task_events WHERE kind='module/loaded' AND json_extract(data,'$.after_fix')=1"), 1,
            "the other failed call waited for that fix and ran the new version");
        let versions = conn.prepare("SELECT version FROM agent_modules WHERE task_id='task' ORDER BY version").unwrap()
            .query_map([], |r| r.get::<_, i64>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(versions, [1, 2]);
    }

    #[tokio::test]
    async fn a_virtual_in_the_main_program_is_a_hook_with_its_imports() {
        let root = TestRoot::new();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let log = seen.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply("```js\nimport * as io from \"host:io\";\nconst result = await judge(\"a.txt\");\nio.write({path: \"out.txt\", content: String(result.keep)});\n\n/** @virtual */\nasync function judge(path) {\n  // Decide whether to keep the file at path. Returns {keep:boolean}.\n}\n```".into()),
            "leaf" => {
                log.lock().unwrap().push(tool_names(body));
                reply(json!({"keep":true}).to_string())
            }
            other => panic!("a @virtual is a hook, not code: {other}"),
        }).await;
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "true");
        let seen = seen.lock().unwrap();
        let definitions = crate::agent_service::local_tools(&crate::tools::Workspace::new(root.path().to_path_buf()).unwrap(), None, false);
        let io = agent_program::libraries::catalog(&definitions, &json!([]))["io"].values().cloned().collect::<BTreeSet<_>>();
        assert_eq!(seen[0].iter().filter(|name| *name != FIND && *name != ASK).cloned().collect::<BTreeSet<_>>(), io, "the main program's io import");
        assert!(seen[0].contains(&FIND.to_owned()) && seen[0].contains(&ASK.to_owned()));
        assert!(module_files(root.path()).is_empty(), "a hook writes no module");
    }

    #[tokio::test]
    async fn a_virtual_that_changes_files_can_return_nothing() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "draft").unwrap();
        let prompts = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = prompts.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply("```js\nimport * as io from \"host:io\";\nconst changed = await polish(\"a.txt\");\nio.write({path: \"out.txt\", content: String(changed === null) + \":\" + io.read({path: \"a.txt\"}).content});\n\n/** @virtual */\nasync function polish(path) {\n  // Rewrite the file at path in its final form. Changes the file; returns nothing.\n}\n```".into()),
            "leaf" => {
                log.lock().unwrap().push(body["messages"][0]["content"].as_str().unwrap_or("").to_owned());
                if body["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool") {
                    reply("null".into())
                } else {
                    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                        "tool_calls":[{"id":"w","type":"function","function":{"name":"write_file","arguments":json!({"path":"a.txt","content":"final"}).to_string()}}]}}]})
                }
            }
            other => panic!("a @virtual is a hook, not code: {other}"),
        }).await;
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "true:final", "the caller got null and read the file it changed");
        assert!(prompts.lock().unwrap()[0].contains("when the contract returns nothing, reply null"), "the function is told how to finish");
    }

    #[tokio::test]
    async fn a_responses_provider_gets_the_same_work_and_its_reasoning_back() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        let app = Router::new().route("/v1/responses", post(move |Json(body): Json<Value>| {
            let mut log = log.lock().unwrap();
            log.push(body);
            let output = match log.len() {
                1 => json!([{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"Look first"}],"encrypted_content":"enc-1"},
                    {"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}]),
                _ => json!([{"type":"function_call","id":"fc_2","call_id":"call_2","name":"write_program",
                    "arguments":json!({"source":"import * as io from \"host:io\";\nio.write({path: \"out.txt\", content: \"done\"});"}).to_string()}]),
            };
            let reply = json!({"object":"response","status":"completed","output":output,"usage":{"input_tokens":1,"output_tokens":1}});
            async move { Json(reply) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        state.provider_name = "sub2api".into();
        state.provider_routes.insert(state.provider_name.clone(), agent_service::AgentProviderRoute {
            prefix_system_messages: false, responses: true, url: state.provider_url.clone(), api_key: state.api_key.clone(),
            models: Default::default(), max_output_tokens: Default::default(), context_windows: Default::default(),
        });
        state.reasoning_effort = Some("none".into());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state, "task".into(), "fake".into(), "Summarize a".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
        server.abort();
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "both requests went to /v1/responses");
        let first = &bodies[0];
        assert!(first["instructions"].as_str().unwrap().starts_with("Complete the user's authorized task"), "the role prompt is the instructions");
        assert_eq!(first["store"], false);
        assert_eq!(first["reasoning"]["effort"], "low", "thinking is never off");
        assert!(first["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == "read_file"));
        let input = bodies[1]["input"].as_array().unwrap();
        let at = |kind: &str| input.iter().position(|item| item["type"] == kind).unwrap_or_else(|| panic!("no {kind}: {input:?}"));
        assert_eq!(input[at("reasoning")]["encrypted_content"], "enc-1", "its reasoning came back");
        assert!(at("reasoning") < at("function_call") && at("function_call") < at("function_call_output"));
        assert!(input[at("function_call_output")]["output"].as_str().unwrap().contains("alpha"));
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "done", "the program it handed over ran");
    }

    #[tokio::test]
    async fn a_virtual_gets_only_the_libraries_its_code_imports() {
        let root = TestRoot::new();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let log = seen.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply("```js\nimport * as io from \"host:io\";\nimport * as process from \"host:process\";\nconst result = await judge(\"a.txt\");\nio.write({path: \"out.txt\", content: String(result.keep)});\n\n/** @virtual */\nasync function judge(path) {\n  // Run the project's checks on the file at path. Returns {keep:boolean}.\n}\n```".into()),
            "leaf" => {
                log.lock().unwrap().push(tool_names(body));
                reply(json!({"keep":true}).to_string())
            }
            other => panic!("a @virtual is a hook, not code: {other}"),
        }).await;
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "true");
        let seen = seen.lock().unwrap();
        let definitions = crate::agent_service::local_tools(&crate::tools::Workspace::new(root.path().to_path_buf()).unwrap(), None, false);
        let catalog = agent_program::libraries::catalog(&definitions, &json!([]));
        let imported = ["io", "process"].into_iter().flat_map(|library| catalog[library].values().cloned()).collect::<BTreeSet<_>>();
        assert_eq!(seen[0].iter().filter(|name| *name != FIND && *name != ASK).cloned().collect::<BTreeSet<_>>(), imported,
            "only io and process: nothing from browser, code or memory");
        assert!(seen[0].contains(&FIND.to_owned()) && seen[0].contains(&ASK.to_owned()));
    }

    #[tokio::test]
    async fn rewinding_a_turn_takes_back_the_modules_it_wrote() {
        let root = TestRoot::new();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let function = body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_task_input")
                .map(|m| serde_json::from_str::<Value>(m["content"].as_str().unwrap()).unwrap()["name"].as_str().unwrap().to_owned());
            let answer = match function.as_deref() {
                None if body["messages"].to_string().contains("Whisper too") => reply("```js\nconst a = await whisper(\"A\");\n\n/** @module */\nasync function whisper(word) {\n  // Return {text:string}: word in small letters.\n}\n```".into()),
                None => reply(format!("```js\nconst a = await shout(\"a\");\n\n{SHOUT}\n```")),
                Some("whisper") => reply("```js\nasync function whisper(word) {\n  return {text: word.toLowerCase()};\n}\n```".into()),
                Some(_) => reply(SHOUT_CODE.into()),
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state.clone(), "task".into(), "fake".into(), "Shout".into(), 10, CancellationToken::new(), 1, false).await.unwrap();
        run_task(state, "task".into(), "fake".into(), "Whisper too".into(), 10, CancellationToken::new(), 2, false).await.unwrap();
        server.abort();
        let files = module_files(root.path());
        assert!(files.len() == 2 && files[0].starts_with("shout-") && files[1].starts_with("whisper-"), "{files:?}");
        crate::rewind::rewind(root.path(), "task", 2).unwrap();
        let files = module_files(root.path());
        assert!(files.len() == 1 && files[0].starts_with("shout-"), "the turn's module went back with it: {files:?}");
        let rows: i64 = conn.query_row("SELECT count(*) FROM agent_modules WHERE task_id='task'", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);
    }

    /// A model service whose hook requests fail `failures` times with a 503
    /// that asks for no wait; main requests always answer.
    async fn flaky_service(root: &std::path::Path, failures: usize) -> (Vec<Value>, usize) {
        let main = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let hook_tries = Arc::new(AtomicUsize::new(0));
        let (log, tries) = (main.clone(), hook_tries.clone());
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let (log, tries) = (log.clone(), tries.clone());
            async move {
                use axum::response::IntoResponse;
                if role_of(&body) == "leaf" {
                    if tries.fetch_add(1, Ordering::SeqCst) < failures {
                        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "0")], "upstream down").into_response();
                    }
                    return Json(reply(json!({"ok":true}).to_string())).into_response();
                }
                log.lock().unwrap().push(body);
                Json(reply("```js\nimport * as io from \"host:io\";\nconst verdict = await judge(\"x\");\nio.write({path: \"out.txt\", content: String(verdict.ok)});\n\n/** @virtual */\nasync function judge(x) {\n  // Decide whether x is fine. Returns {ok:boolean}.\n}\n```".into())).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = agent_service::tests::test_state(root, listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state, "task".into(), "fake".into(), "Judge x".into(), 20, CancellationToken::new(), 1, false).await.unwrap();
        server.abort();
        let main = main.lock().unwrap().clone();
        (main, hook_tries.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_model_request_that_got_no_answer_is_tried_again() {
        let root = TestRoot::new();
        let (main, tries) = flaky_service(root.path(), 2).await;
        assert_eq!(tries, 3, "two failures, then the answer");
        assert_eq!(main.len(), 1, "the program never noticed");
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "true");
    }

    #[tokio::test]
    async fn a_service_that_stays_down_is_named_and_reaches_neither_a_fixer_nor_a_rewrite() {
        let root = TestRoot::new();
        let (main, tries) = flaky_service(root.path(), usize::MAX).await;
        assert_eq!(tries, MODEL_ATTEMPTS as usize, "the host tried the request a few times");
        assert_eq!(main.len(), 1, "the main AI was not asked to rewrite a program that did nothing wrong");
        let conn = agent_service::open_db(root.path()).unwrap();
        let error: String = conn.query_row("SELECT data FROM agent_task_events WHERE task_id='task' AND kind='program/error' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        let error: Value = serde_json::from_str(&error).unwrap();
        assert_eq!(error["error"]["code"], UNAVAILABLE, "{error}");
        assert_eq!(error["error"]["name"], "ModelUnavailable");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("server error (HTTP 503)") && !message.contains("127.0.0.1") && !message.contains("http"), "plain, without the service's address: {message}");
    }

    #[tokio::test]
    async fn a_program_may_require_the_hosts_own_libraries_as_node_requires_fs() {
        let root = TestRoot::new();
        // A project skill of the same name does not hide the host's library.
        std::fs::create_dir_all(root.path().join(".agents/skills/io")).unwrap();
        std::fs::write(root.path().join(".agents/skills/io/SKILL.md"), "---\nname: io\ndescription: A project skill named like the host's library\n---\nNotes.\n").unwrap();
        let leaf_tools = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let log = leaf_tools.clone();
        run_scripted(root.path(), move |body| match role_of(body).as_str() {
            "main" => reply("```js\nconst io = require(\"io\");\nconst { write } = require(\"io\");\nwrite({path: \"out.txt\", content: \"hello\"});\nconst back = io.read({path: \"out.txt\"});\nconst verdict = await judge(back.content);\nio.write({path: \"verdict.txt\", content: String(verdict.ok) + \" \" + String(require(\"io\") === io)});\n\n/** @virtual */\nasync function judge(text) {\n  // Decide whether text says hello. Returns {ok:boolean}.\n}\n```".into()),
            "leaf" => {
                log.lock().unwrap().push(tool_names(body));
                reply(json!({"ok":true}).to_string())
            }
            other => panic!("unexpected {other} request"),
        }).await;
        assert_eq!(std::fs::read_to_string(root.path().join("out.txt")).unwrap(), "hello", "a destructured method of a required library runs its tool");
        assert_eq!(std::fs::read_to_string(root.path().join("verdict.txt")).unwrap(), "true true", "requiring it again gives the same object");
        let conn = agent_service::open_db(root.path()).unwrap();
        let links = conn.prepare("SELECT data FROM agent_task_events WHERE task_id='task' AND kind='program/require'").unwrap()
            .query_map([], |r| r.get::<_, String>(0)).unwrap().map(|row| serde_json::from_str::<Value>(&row.unwrap()).unwrap()).collect::<Vec<_>>();
        assert_eq!(links.len(), 1, "a run links a library once: {links:?}");
        assert_eq!((links[0]["failed"].as_bool(), links[0]["result"]["methods"]["read"].as_str()), (Some(false), Some("read_file")), "the host's io, not the project skill: {links:?}");
        let leaf_tools = leaf_tools.lock().unwrap();
        assert!(leaf_tools[0].contains(&"read_file".to_owned()) && leaf_tools[0].contains(&"write_file".to_owned()),
            "a hook gets the methods of the host library its declaring code requires: {:?}", leaf_tools[0]);
    }

    #[tokio::test]
    async fn a_program_requires_a_plugin_library_where_it_needs_it_without_loading_its_skill() {
        let root = TestRoot::new();
        std::fs::write(root.path().join("code-flow.json"), r#"{"version":1,"features":[]}"#).unwrap();
        let (plugins, catalog) = crate::skill_packages::test_runtime();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = bodies.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            log.lock().unwrap().push(body.clone());
            let answer = match role_of(&body).as_str() {
                // A static import of a plugin library is refused before it runs.
                "main" if !body["messages"].to_string().contains("PROGRAM_SOURCE_INVALID") => reply("```js\nimport * as flow from \"host:flow\";\nflow.read_code_flow({});\n```".into()),
                "main" => reply("```js\nconst flow = require(\"flow\");\nconst graph = flow.read_code_flow({});\nlet missing = \"\";\ntry { require(\"absent\"); } catch (error) { missing = error.code; }\nconst verdict = await check(graph);\nconsole.log(missing, verdict.ok);\n\n/** @virtual */\nasync function check(graph) {\n  // Look at the code flow graph. Returns {ok:boolean}.\n}\n```".into()),
                "leaf" if !body["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool") =>
                    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
                        "tool_calls":[{"id":"find","type":"function","function":{"name":FIND,"arguments":json!({"query":"code flow"}).to_string()}}]}}]}),
                "leaf" => reply(json!({"ok":true}).to_string()),
                other => panic!("unexpected {other} request"),
            };
            async move { Json(answer) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut state = agent_service::tests::test_state(root.path(), listener.local_addr().unwrap());
        state.tool_catalog = Some(catalog);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let conn = agent_service::open_db(root.path()).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", []).unwrap();
        run_task(state, "task".into(), "fake".into(), "Check the code flow".into(), 20, CancellationToken::new(), 1, false).await.unwrap();
        server.abort();
        let bodies = bodies.lock().unwrap();
        let headers = |body: &Value| body["messages"].as_array().unwrap().iter().find(|m| m["name"] == "program_tool_headers").unwrap()["content"].as_str().unwrap().to_owned();
        let first = headers(&bodies[0]);
        assert!(first.contains("/** 分析、修改、重构和测试项目代码的工作流程") && first.contains("declare module \"flow\";"), "the index names the library to require and what it is for: {first}");
        assert!(!first.contains("declare namespace flow"), "its methods are read on demand, not listed: {first}");
        assert!(!tool_names(&bodies[0]).contains(&"read_code_flow".to_owned()), "the main AI reaches the method only through its program");
        let refused = bodies[1]["messages"].as_array().unwrap().iter().filter_map(|m| m["content"].as_str())
            .find(|text| text.contains("PROGRAM_SOURCE_INVALID")).expect("the refusal reached the main AI");
        assert!(refused.contains("const flow = require(\\\"flow\\\")"), "the diagnostic says how to load it: {refused}");
        let leaf = bodies.iter().find(|body| role_of(body) == "leaf").expect("the hook ran");
        assert!(tool_names(leaf).contains(&"read_code_flow".to_owned()), "a hook gets the methods of the libraries its declaring code requires");
        let found = bodies.iter().filter(|body| role_of(body) == "leaf").last().unwrap()["messages"].as_array().unwrap().iter()
            .find(|m| m["role"] == "tool").unwrap()["content"].as_str().unwrap().to_owned();
        assert!(found.contains("\"method\":\"flow.read_code_flow\""), "finding methods searches third-party libraries too: {found}");
        let links = conn.prepare("SELECT data FROM agent_task_events WHERE task_id='task' AND kind='program/require' ORDER BY seq").unwrap()
            .query_map([], |r| r.get::<_, String>(0)).unwrap().map(|row| serde_json::from_str::<Value>(&row.unwrap()).unwrap()).collect::<Vec<_>>();
        assert_eq!(links.len(), 2, "{links:?}");
        assert_eq!(links[0]["library"], "flow");
        assert_eq!(links[0]["result"]["methods"], json!({"read_code_flow":"read_code_flow"}));
        assert_eq!(links[1]["library"], "absent");
        assert_eq!((links[1]["failed"].as_bool(), links[1]["result"]["code"].as_str()), (Some(true), Some("MODULE_NOT_FOUND")));
        let result: String = conn.query_row("SELECT r.data FROM agent_task_events c JOIN agent_task_events r ON r.task_id=c.task_id AND r.kind='tool/result'
            AND json_extract(r.data,'$.message.toolCallId')=json_extract(c.data,'$.callId') WHERE c.task_id='task' AND c.kind='tool/call' AND json_extract(c.data,'$.name')='read_code_flow'", [], |r| r.get(0)).unwrap();
        assert!(!result.contains("SKILL_NOT_ACTIVE") && serde_json::from_str::<Value>(&result).unwrap()["message"]["isError"] == false, "the method ran without loading the skill: {result}");
        let saved: String = conn.query_row("SELECT state FROM agent_runtime_windows WHERE task_id='task' AND scope='main'", [], |r| r.get(0)).unwrap();
        assert!(serde_json::from_str::<Window>(&saved).unwrap().active_skills.is_empty(), "requiring a library activates no skill");
        let outcome = conn.query_row("SELECT data FROM agent_task_events WHERE task_id='task' AND data LIKE '%MODULE_NOT_FOUND true%' LIMIT 1", [], |r| r.get::<_, String>(0));
        assert!(outcome.is_ok(), "the program printed what require threw and what the hook decided");
        plugins.shutdown().unwrap();
    }
}
