//! Static source libraries bind to real tools, never to guessed operations.
use super::*;
use std::collections::BTreeSet;

pub type Libraries = BTreeMap<String, BTreeMap<String, String>>;

pub fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || matches!(c, '_' | '$'))
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
}
pub fn reserved(name: &str) -> bool {
    matches!(
        name,
        "await"
            | "yield"
            | "delete"
            | "void"
            | "typeof"
            | "instanceof"
            | "in"
            | "super"
            | "with"
            | "debugger"
            | "extends"
            | "enum"
            | "implements"
            | "interface"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "default"
            | "case"
            | "tools"
            | "JSON"
            | "Promise"
            | "call"
            | "ai"
            | "assert"
            | "function"
            | "var"
            | "let"
            | "const"
            | "if"
            | "else"
            | "while"
            | "for"
            | "do"
            | "return"
            | "throw"
            | "try"
            | "catch"
            | "finally"
            | "class"
            | "new"
            | "this"
            | "true"
            | "false"
            | "null"
            | "switch"
            | "break"
            | "continue"
            | "import"
            | "export"
    ) || name.starts_with("$temp")
}
pub fn namespace(id: &str) -> String {
    let mut name = id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '$') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if name.chars().next().is_some_and(|c| c.is_numeric()) {
        name.insert(0, '_');
    }
    name
}
/// The host's own libraries, one kind of capability each, as Node splits its
/// built-in modules into fs, child_process and http: what code imports is
/// what it needs. Every host tool belongs to one of them; there is no
/// catch-all library. A method drops a prefix that repeats its library's
/// name; `io` keeps its short names.
pub fn builtin() -> Libraries {
    let named = |methods: &[(&str, &str)]| -> BTreeMap<String, String> {
        methods.iter().map(|(method, tool)| ((*method).to_owned(), (*tool).to_owned())).collect()
    };
    let same = |tools: &[&str]| -> BTreeMap<String, String> {
        tools.iter().map(|tool| ((*tool).to_owned(), (*tool).to_owned())).collect()
    };
    BTreeMap::from([
        ("io".into(), named(&[
            ("read", "read_file"),
            ("read_lines", "read_file_lines"),
            ("write", "write_file"),
            ("list", "list_dir"),
            ("search", "search_text"),
            ("edit", "edit_file"),
            ("replace", "replace_range"),
            ("workspace_info", "workspace_info"),
        ])),
        // Published only while a program the user's message paused can be resumed.
        ("previous".into(), named(&[("resume", "resume_previous_program")])),
        ("process".into(), same(&[
            "run_program",
            "run_project_script",
            "install_dependencies",
            "get_project_process",
            "stop_project_process",
        ])),
        ("http".into(), named(&[("probe", "http_probe")])),
        ("browser".into(), named(&[
            ("open", "browser_open"),
            ("read", "browser_read"),
            ("wait", "browser_wait"),
            ("diagnostics", "browser_diagnostics"),
            ("click", "browser_click"),
            ("press_key", "browser_press_key"),
            ("upload", "browser_upload"),
            ("screenshot", "browser_screenshot"),
            ("close", "browser_close"),
        ])),
        ("code".into(), same(&["search_code_map"])),
        ("code_go".into(), same(&["list_go_symbols", "search_go_symbols", "read_go_symbol"])),
        ("code_rust".into(), same(&["list_rust_symbols", "search_rust_symbols", "read_rust_symbol"])),
        ("code_ts".into(), same(&["list_ts_symbols", "search_ts_symbols", "read_ts_symbol"])),
        ("code_python".into(), same(&["list_python_symbols", "search_python_symbols", "read_python_symbol"])),
        ("memory".into(), same(&[
            "record_work_memory",
            "list_work_memory",
            "search_work_memory",
            "record_architecture_memory",
            "list_architecture_memory",
            "search_architecture_memory",
            "record_symbol_business_context",
            "list_symbol_business_context",
            "search_symbol_business_context",
        ])),
        ("vision".into(), same(&["view_image", "analyze_images", "image_analysis_results"])),
        ("attachments".into(), same(&["read_attachment"])),
        // The main conversation's own materials and records: only the main
        // script has them, as a function has only its arguments.
        ("session".into(), same(&[
            "read_session_history",
            "list_project_flows",
            "read_project_flow",
            "read_asset",
            "list_context_assets",
            "refresh_asset",
            "propose_project_note",
        ])),
    ])
}
/// Only tools present in this frame's catalog are published. Colliding plugin
/// namespaces are omitted rather than silently routing to the wrong owner.
pub fn catalog(definitions: &[Value], packages: &Value) -> Libraries {
    let allowed = definitions
        .iter()
        .filter_map(|d| d["function"]["name"].as_str())
        .collect::<BTreeSet<_>>();
    let owned = owned(packages);
    let mut result = builtin();
    for methods in result.values_mut() {
        methods.retain(|_, tool| allowed.contains(tool.as_str()) && !owned.contains(tool.as_str()));
    }
    result.retain(|_, methods| !methods.is_empty());
    let mut candidates: BTreeMap<String, Vec<BTreeMap<String, String>>> = BTreeMap::new();
    for package in packages.as_array().into_iter().flatten() {
        if package["enabled"] != true {
            continue;
        }
        let Some(id) = package["id"].as_str() else {
            continue;
        };
        let library = package["library"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| namespace(id));
        if !valid_namespace(&library) {
            continue;
        }
        let prefix = format!("{}__", id.replace('-', "_"));
        let methods = package["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|name| allowed.contains(name))
            .filter_map(|name| {
                let method = name.strip_prefix(&prefix).unwrap_or(name);
                identifier(method).then(|| (method.to_owned(), name.to_owned()))
            })
            .collect::<BTreeMap<_, _>>();
        if !methods.is_empty() {
            candidates.entry(library).or_default().push(methods);
        }
    }
    for (name, mut methods) in candidates {
        if methods.len() == 1 {
            result.insert(name, methods.remove(0));
        }
    }
    result
}
/// Tools that plugin packages declare, whether or not they got a library.
fn owned(packages: &Value) -> BTreeSet<&str> {
    packages
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["tools"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect()
}
/// Host tools in these definitions that no library has: neither a plugin's
/// nor the AI loop's own. There is no catch-all library, so each one needs
/// a place in `builtin`; a test keeps this list empty.
#[cfg(test)]
pub fn unassigned(definitions: &[Value], packages: &Value) -> Vec<String> {
    let libraries = catalog(definitions, packages);
    let bound = libraries
        .values()
        .flat_map(|methods| methods.values().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let owned = owned(packages);
    definitions
        .iter()
        .filter_map(|d| d["function"]["name"].as_str())
        .filter(|name| !bound.contains(name) && !owned.contains(name) && !control(name))
        .map(str::to_owned)
        .collect()
}

/// The libraries enabled plugin packages contribute: third-party code a
/// program loads with require when it runs, never a host: import.
pub fn third_party(libraries: &Libraries, packages: &Value) -> BTreeSet<String> {
    packages
        .as_array()
        .into_iter()
        .flatten()
        .filter(|package| package["enabled"] == true)
        .filter_map(|package| {
            package["library"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| package["id"].as_str().map(namespace))
        })
        .filter(|name| libraries.contains_key(name))
        .collect()
}
/// Host-owned namespaces cannot be claimed by plugins: the host's own
/// libraries, and `host`, the import prefix.
pub fn valid_namespace(name: &str) -> bool {
    identifier(name) && !reserved(name) && name != "host" && !builtin().contains_key(name)
}
/// The AI loop's own tools, called directly and never a library's method.
#[cfg(test)]
pub fn control(name: &str) -> bool {
    matches!(
        name,
        "read_program_guide"
            | "read_tool_contracts"
            | "define_task_body"
            | "create_task"
            | "list_skills"
            | "read_skill"
            | "unload_skill"
            | "release_assets"
            | "find_library_methods"
            | "ask_user"
            | "write_program"
    )
}
pub fn public_name(libraries: &Libraries, target: &str) -> Option<String> {
    libraries.iter().find_map(|(library, methods)| {
        methods
            .iter()
            .find_map(|(method, tool)| (tool == target).then(|| format!("{library}.{method}")))
    })
}
pub fn target<'a>(libraries: &'a Libraries, name: &str) -> Option<&'a str> {
    let (library, method) = name.split_once('.')?;
    libraries.get(library)?.get(method).map(String::as_str)
}
