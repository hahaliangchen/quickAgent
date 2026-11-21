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
            ("read_chunk", "read_file_chunk"),
            ("write", "write_file"),
            ("list", "list_dir"),
            ("search", "search_text"),
            ("edit", "edit_file"),
            ("replace", "replace_range"),
            ("delete", "delete_path"),
            ("rename", "rename_path"),
            ("mkdir", "create_dir"),
            ("workspace_info", "workspace_info"),
        ])),
        ("git".into(), named(&[
            ("status", "git_status"),
            ("diff", "git_diff"),
            ("log", "git_log"),
        ])),
        ("process".into(), BTreeMap::from([
            ("exec_command".into(), "exec_command".into()),
            ("write_stdin".into(), "write_stdin".into()),
            ("kill_session".into(), "kill_session".into()),
            ("run_program".into(), "run_program".into()),
            ("run_project_script".into(), "run_project_script".into()),
            ("install_dependencies".into(), "install_dependencies".into()),
            ("get_project_process".into(), "get_project_process".into()),
            ("stop_project_process".into(), "stop_project_process".into()),
            ("run_readonly".into(), "run_readonly_command".into()),
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
        ("vision".into(), same(&[
            "view_image",
            "analyze_images",
            "image_analysis_results",
            "generate_image",
            "register_image",
        ])),
        ("attachments".into(), same(&["read_attachment"])),
        // The main conversation's own materials and records: only the main
        // script has them, as a function has only its arguments.
        ("session".into(), same(&[
            "read_session_history",
            "read_asset",
            "snapshot",
        ])),
        // Model-facing control-plane methods are regular host-library calls
        // in the context-as-code ABI. They still resolve to the existing
        // permission-checked tool handlers.
        ("host".into(), named(&[
            ("contracts", "read_tool_contracts"),
            ("find_methods", "find_library_methods"),
            ("guide", "read_program_guide"),
            ("list_skills", "list_skills"),
            ("read_skill", "read_skill"),
            ("ask_user", "ask_user"),
        ])),
    ])
}

/// `host.delegate` is available only in a parent frame. Virtual leaf frames
/// remove `session`, which keeps delegation out of their static host surface.
pub fn allows_delegation(libraries: &Libraries) -> bool {
    libraries.contains_key("session")
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
        // First-party native packages may contribute methods to a host
        // namespace, but external plugins cannot occupy a builtin namespace.
        let first_party_builtin = package["origin"] == "builtin" && builtin().contains_key(&library);
        if !valid_namespace(&library) && !first_party_builtin {
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
            let methods = methods.remove(0);
            if let Some(existing) = result.get_mut(&name) {
                existing.extend(methods);
            } else {
                result.insert(name, methods);
            }
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

/// Host-owned namespaces cannot be claimed by plugins: the host's own
/// libraries, and `host`, the import prefix.
pub fn valid_namespace(name: &str) -> bool {
    identifier(name) && !reserved(name) && name != "host" && !builtin().contains_key(name)
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
