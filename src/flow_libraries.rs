//! Read-only JavaScript orchestration libraries exposed via `require("<name>")`.
//! Sources live in a project skill's `modules/*.js`, an enabled plugin
//! package's `modules/*.js`, or built-in host presets, and execute in isolated
//! read-only Layer-1 module frames.
use crate::agent_program::{self, Program};
use crate::single_agent::{Session, Window};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone)]
pub(crate) struct MethodSpec {
    pub name: String,
    pub params: Vec<String>,
    pub description: String,
    pub is_async: bool,
    pub contract_key: String,
    pub source: String,
}

#[derive(Clone)]
pub(crate) struct LibrarySource {
    pub name: String,
    pub description: String,
    pub files: Vec<(String, String)>,
}

/// Host built-in libraries (`io`, `process`, ...) remain reserved and can never
/// be shadowed by skill or plugin orchestration modules.
fn is_host_builtin(name: &str) -> bool {
    agent_program::libraries::builtin().contains_key(name)
}

fn valid_library_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphanumeric() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        && name != "host"
        && !is_host_builtin(name)
        && !agent_program::libraries::reserved(name)
}

/// Parse `<name>-<16hex>.js` or `<name>.js` into its exported function name.
fn method_name_from_file(stem: &str) -> Option<&str> {
    let base = match stem.rsplit_once('-') {
        Some((name, key)) if key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit()) => name,
        _ => stem,
    };
    (agent_program::libraries::identifier(base) && !agent_program::libraries::reserved(base))
        .then_some(base)
}

fn collect_dir_files(dir: &Path) -> Vec<(String, String)> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths = read_dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "js"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let file_name = path.file_name()?.to_string_lossy().into_owned();
            let source = std::fs::read_to_string(&path).ok()?;
            Some((file_name, source))
        })
        .collect()
}

fn task_project(s: &Session<'_>) -> Option<std::path::PathBuf> {
    crate::project_notes::task_project(s.state.workspace.root(), s.task).ok().flatten()
}

fn enabled_package_sources(s: &Session<'_>) -> Vec<LibrarySource> {
    let Some(catalog) = s.state.tool_catalog.as_ref() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for item in catalog.packages.catalog().as_array().into_iter().flatten() {
        if item["enabled"] != true {
            continue;
        }
        let Some(id) = item["id"].as_str() else {
            continue;
        };
        if let Some(owner) = s.plugin.as_deref()
            && owner != id
        {
            continue;
        }
        let lib_name = item["library"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| agent_program::libraries::namespace(id));
        if !valid_library_name(&lib_name) {
            continue;
        }
        let Some(root) = item["package_root"].as_str() else {
            continue;
        };
        let modules_dir = Path::new(root).join("modules");
        if modules_dir.is_dir() {
            let files = collect_dir_files(&modules_dir);
            if !files.is_empty() {
                found.push(LibrarySource {
                    name: lib_name,
                    description: item["description"].as_str().unwrap_or("").to_owned(),
                    files,
                });
            }
        }
    }
    found
}

fn builtin_preset_sources() -> Vec<LibrarySource> {
    vec![
        LibrarySource {
            name: "batch-files".to_owned(),
            description: "Batch file reading and focused multi-file synthesis without polluting the main window.".to_owned(),
            files: vec![
                (
                    "readMany.js".to_owned(),
                    include_str!("flow_presets/batch_files_readMany.js").to_owned(),
                ),
                (
                    "summarizeFiles.js".to_owned(),
                    include_str!("flow_presets/batch_files_summarizeFiles.js").to_owned(),
                ),
            ],
        },
        LibrarySource {
            name: "repo-verify".to_owned(),
            description: "Workspace verification helpers: run deterministic command checks and summarize git changes.".to_owned(),
            files: vec![
                (
                    "runChecks.js".to_owned(),
                    include_str!("flow_presets/repo_verify_runChecks.js").to_owned(),
                ),
                (
                    "reviewChanges.js".to_owned(),
                    include_str!("flow_presets/repo_verify_reviewChanges.js").to_owned(),
                ),
            ],
        },
    ]
}

/// Locate a read-only JS orchestration library visible to `s`.
/// Precedence in non-plugin sessions: project skill > enabled plugin > built-in preset.
/// Plugin sessions keep strictly to the owning plugin unless the package is enabled.
pub(crate) fn find_library(s: &Session<'_>, name: &str) -> Option<LibrarySource> {
    if !valid_library_name(name) {
        return None;
    }
    if s.plugin.is_none()
        && let Some(project) = task_project(s)
    {
        let skill_dir = project.join(crate::flow_skills::DIR).join(name);
        let modules_dir = skill_dir.join("modules");
        if modules_dir.is_dir() {
            let files = collect_dir_files(&modules_dir);
            if !files.is_empty() {
                let description = crate::flow_skills::list(&project)
                    .into_iter()
                    .find(|skill| skill.name == name)
                    .map(|skill| skill.description)
                    .unwrap_or_default();
                return Some(LibrarySource {
                    name: name.to_owned(),
                    description,
                    files,
                });
            }
        }
    }
    if let Some(pkg) = enabled_package_sources(s)
        .into_iter()
        .find(|source| source.name == name)
    {
        return Some(pkg);
    }
    if s.plugin.is_none() {
        return builtin_preset_sources()
            .into_iter()
            .find(|source| source.name == name);
    }
    None
}

/// All requirable JS orchestration libraries in `s` (excluding host built-ins).
pub(crate) fn list_libraries(s: &Session<'_>) -> Vec<(String, String)> {
    let mut found = BTreeMap::new();
    if s.plugin.is_none() {
        for preset in builtin_preset_sources() {
            found.insert(preset.name, preset.description);
        }
    }
    for source in enabled_package_sources(s) {
        found.insert(source.name, source.description);
    }
    if s.plugin.is_none()
        && let Some(project) = task_project(s)
        && let Ok(entries) = std::fs::read_dir(project.join(crate::flow_skills::DIR))
    {
        let skills = crate::flow_skills::list(&project)
            .into_iter()
            .map(|skill| (skill.name, skill.description))
            .collect::<BTreeMap<_, _>>();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().join("modules").is_dir()
                && !collect_dir_files(&entry.path().join("modules")).is_empty()
                && valid_library_name(&name)
            {
                let desc = skills.get(&name).cloned().unwrap_or_default();
                found.insert(name, desc);
            }
        }
    }
    found.into_iter().collect()
}

pub(crate) fn contract_key(name: &str, params: &[String], is_async: bool, description: &str) -> String {
    let signature = format!(
        "{}function {name}({})",
        if is_async { "async " } else { "" },
        params.join(", ")
    );
    crate::visual_artifacts::hash(format!("{signature}\n{description}").as_bytes())[..16]
        .to_owned()
}

fn compile_with(
    source: &str,
    entry: &str,
    libraries: agent_program::libraries::Libraries,
) -> Result<Program> {
    let program = agent_program::compile_body_with_scope(source, entry, libraries, BTreeSet::new())?;
    agent_program::uncalled_functions(source, Some(entry))?;
    ensure!(
        !program
            .functions
            .values()
            .any(|function| function.virtual_task && function.module_task),
        "SECOND_LAYER_VIRTUAL_REQUIRED: declare judgment helpers with @virtual inside library modules"
    );
    Ok(program)
}

fn parse_method_with(
    file_name: &str,
    source: &str,
    libraries: agent_program::libraries::Libraries,
) -> Result<(MethodSpec, Program)> {
    let stem = file_name
        .strip_suffix(".js")
        .context("library module must end with .js")?;
    let name = method_name_from_file(stem)
        .with_context(|| format!("invalid library module filename '{file_name}'"))?;
    let program = compile_with(source, name, libraries)?;
    let function = program
        .functions
        .get(name)
        .with_context(|| format!("module file '{file_name}' must define function '{name}'"))?;
    let key = contract_key(
        name,
        &function.params,
        function.is_async,
        &function.description,
    );
    let spec = MethodSpec {
        name: name.to_owned(),
        params: function.params.clone(),
        description: function.description.clone(),
        is_async: function.is_async,
        contract_key: key,
        source: source.to_owned(),
    };
    Ok((spec, program))
}

fn parse_method(file_name: &str, source: &str) -> Result<MethodSpec> {
    parse_method_with(file_name, source, agent_program::libraries::builtin())
        .map(|(spec, _)| spec)
}

pub(crate) fn validate_module_source(file_name: &str, source: &str) -> Result<MethodSpec> {
    ensure!(
        !file_name.contains(['/', '\\']) && !file_name.starts_with('.'),
        "invalid module filename '{file_name}'"
    );
    parse_method(file_name, source)
}

/// Validate a bundle of `(file_name, source)` modules before persisting them as
/// a read-only orchestration skill library.
pub(crate) fn validate_module_set(files: &[(String, String)]) -> Result<Vec<MethodSpec>> {
    let mut specs = Vec::with_capacity(files.len());
    let mut seen_names = BTreeSet::new();
    for (file_name, source) in files {
        let spec = validate_module_source(file_name, source)?;
        ensure!(
            seen_names.insert(spec.name.clone()),
            "duplicate exported module entry '{}'",
            spec.name
        );
        specs.push(spec);
    }
    Ok(specs)
}

pub(crate) fn methods_of(s: &Session<'_>, library: &str) -> Option<Vec<MethodSpec>> {
    let source = find_library(s, library)?;
    let mut parsed = Vec::new();
    for (file_name, content) in source.files {
        if let Ok(spec) = parse_method(&file_name, &content) {
            parsed.push(spec);
        }
    }
    Some(parsed)
}

/// Publicly exported callable method names for `require("<library>")`.
pub(crate) fn public_method_names(s: &Session<'_>, library: &str) -> Option<Vec<String>> {
    let specs = methods_of(s, library)?;
    let mut counts = BTreeMap::<String, usize>::new();
    for spec in &specs {
        *counts.entry(spec.name.clone()).or_default() += 1;
    }
    let names = counts
        .into_iter()
        .filter_map(|(name, count)| (count == 1).then_some(name))
        .collect::<Vec<_>>();
    Some(names)
}

/// Compile and return an exported entry method from `library` for execution in
/// an isolated Layer-1 library module frame.
pub(crate) fn compile_entry(
    s: &Session<'_>,
    library: &str,
    method: &str,
    args: &[Value],
    static_libraries: agent_program::libraries::Libraries,
) -> Result<(MethodSpec, Program)> {
    let source = find_library(s, library)
        .with_context(|| format!("Cannot find orchestration library '{library}'"))?;
    let mut matched_files = Vec::new();
    for (file_name, content) in source.files {
        let Some(stem) = file_name.strip_suffix(".js") else {
            continue;
        };
        if method_name_from_file(stem) == Some(method) {
            matched_files.push((file_name, content));
        }
    }
    ensure!(
        !matched_files.is_empty(),
        "TypeError: {library}.{method} is not a function"
    );
    ensure!(
        matched_files.len() == 1,
        "AMBIGUOUS_LIBRARY_ENTRY: {library}.{method} has multiple implementations in modules/; call an unambiguous entry name"
    );
    let (file_name, content) = matched_files.remove(0);
    let (spec, program) = parse_method_with(&file_name, &content, static_libraries)?;
    ensure!(
        spec.params.len() == args.len(),
        "{library}.{method} expected {} argument(s) ({}), got {}",
        spec.params.len(),
        spec.params.join(", "),
        args.len()
    );
    Ok((spec, program))
}

/// When code inside `library` calls an internal `@virtual` function `name`,
/// resolve a matching pre-saved sibling module from the same library if present.
pub(crate) fn compile_sibling(
    s: &Session<'_>,
    library: &str,
    caller_program: &Program,
    fn_name: &str,
    args: &[Value],
    captured: &BTreeMap<String, Value>,
    static_libraries: agent_program::libraries::Libraries,
) -> Result<Option<(MethodSpec, Program)>> {
    let Some(source) = find_library(s, library) else {
        return Ok(None);
    };
    let declaration = caller_program
        .functions
        .get(fn_name)
        .with_context(|| format!("called function '{fn_name}' missing"))?;
    ensure!(
        declaration.params.len() == args.len(),
        "function argument count mismatch"
    );
    let target_key = contract_key(
        fn_name,
        &declaration.params,
        declaration.is_async,
        &declaration.description,
    );
    let mut candidates = Vec::new();
    for (file_name, content) in source.files {
        let Some(stem) = file_name.strip_suffix(".js") else {
            continue;
        };
        if method_name_from_file(stem) != Some(fn_name) {
            continue;
        }
        if let Ok(spec) = parse_method(&file_name, &content) {
            candidates.push(spec);
        }
    }
    let chosen = candidates
        .iter()
        .find(|spec| spec.contract_key == target_key)
        .cloned()
        .or_else(|| {
            let matching_arity = candidates
                .into_iter()
                .filter(|spec| spec.params.len() == args.len())
                .collect::<Vec<_>>();
            (matching_arity.len() == 1).then(|| matching_arity.into_iter().next().unwrap())
        });
    let Some(spec) = chosen else {
        return Ok(None);
    };
    ensure!(
        captured.is_empty(),
        "FIRST_LAYER_ARGUMENTS_REQUIRED: pass inputs to {fn_name} as parameters; library module helpers do not capture caller variables"
    );
    let program = compile_with(&spec.source, &spec.name, static_libraries)?;
    Ok(Some((spec, program)))
}

/// Compact method signatures shown in `program_tool_headers` so callers know
/// exact entry signatures without an exploratory probe cell or `read_tool_contracts`.
pub(crate) fn compact_exports(s: &Session<'_>, library: &str) -> Option<String> {
    let specs = methods_of(s, library)?;
    let mut by_name = BTreeMap::<String, Option<MethodSpec>>::new();
    for spec in specs {
        by_name
            .entry(spec.name.clone())
            .and_modify(|slot| *slot = None)
            .or_insert(Some(spec));
    }
    let unique = by_name.into_values().flatten().collect::<Vec<_>>();
    if unique.is_empty() {
        return None;
    }
    const LIMIT: usize = 6;
    let shown = unique
        .iter()
        .take(LIMIT)
        .map(|spec| format!("{}({})", spec.name, spec.params.join(", ")))
        .collect::<Vec<_>>()
        .join(", ");
    if unique.len() > LIMIT {
        Some(format!("{shown}, +{} more via read_tool_contracts", unique.len() - LIMIT))
    } else {
        Some(shown)
    }
}

pub(crate) fn format_overview(library: &str, specs: &[MethodSpec]) -> String {
    let mut out = format!("\n{library} (orchestration library — require(\"{library}\") or require(\"{library}/promises\"))\n");
    for spec in specs {
        let first = spec
            .description
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('@'))
            .unwrap_or("");
        let summary = first.split(['.', '。', '\n']).next().unwrap_or("");
        let summary: String = summary.chars().take(140).collect();
        out.push_str(&format!("- {library}.{}({})", spec.name, spec.params.join(", ")));
        if !summary.is_empty() {
            out.push_str(&format!(" — {summary}"));
        }
        out.push('\n');
    }
    out
}

pub(crate) fn format_headers(library: &str, specs: &[MethodSpec]) -> String {
    let mut out = format!(
        "// Orchestration library \"{library}\": const {ns} = require(\"{library}\"); (sync-looking) or require(\"{library}/promises\") (returns Promises)\n",
        ns = agent_program::libraries::namespace(library),
    );
    for spec in specs {
        if !spec.description.trim().is_empty() {
            out.push_str("/**\n");
            for line in spec.description.lines() {
                out.push_str(&format!(" * {line}\n"));
            }
            out.push_str(" */\n");
        }
        out.push_str(&format!(
            "{}function {}({});\n",
            if spec.is_async { "async " } else { "" },
            spec.name,
            spec.params.join(", ")
        ));
    }
    out
}

pub(crate) fn search_methods(s: &Session<'_>, terms: &[&str]) -> Vec<(usize, Value)> {
    let mut results = Vec::new();
    for (library, _) in list_libraries(s) {
        let Some(specs) = methods_of(s, &library) else {
            continue;
        };
        for spec in specs {
            let qualified = format!("{library}.{}", spec.name);
            let text = format!("{qualified} {}", spec.description).to_lowercase();
            let hits = terms.iter().filter(|term| text.contains(**term)).count();
            if hits > 0 {
                results.push((
                    hits,
                    json!({
                        "method": qualified,
                        "library": library,
                        "signature": format!("{}({})", spec.name, spec.params.join(", ")),
                        "description": spec.description,
                        "orchestration": true,
                    }),
                ));
            }
        }
    }
    results
}

/// Build an authorized static-library view for a `library_module` frame.
pub(crate) fn compile_static_libraries(
    s: &Session<'_>,
    parent_window: &Window,
    library: &str,
    method: &str,
) -> agent_program::libraries::Libraries {
    let mut probe_window = parent_window.clone();
    probe_window.program_task = Some(json!({
        "role": "body",
        "module": false,
        "library": library,
        "library_method": method,
        "library_module": true,
    }));
    crate::single_agent::library_compile_scope(s, &probe_window)
}

/// Read contracts for native tool libraries and/or JS orchestration libraries.
pub(crate) fn read_contracts(
    s: &Session<'_>,
    catalog: &agent_program::libraries::Libraries,
    definitions: &[Value],
    args: &Value,
) -> Result<(Value, BTreeSet<String>)> {
    use crate::agent_program::{declarations, libraries};

    ensure!(
        args.as_object()
            .is_some_and(|o| o.keys().all(|k| matches!(k.as_str(), "libraries" | "methods" | "overview"))),
        "expected libraries and/or methods, with optional overview"
    );
    ensure!(
        args.get("libraries").is_none_or(Value::is_array)
            && args.get("methods").is_none_or(Value::is_array),
        "libraries and methods must be arrays"
    );
    ensure!(args.get("overview").is_none_or(Value::is_boolean), "overview must be a boolean");
    let groups = args.get("libraries").and_then(Value::as_array);
    let methods = args.get("methods").and_then(Value::as_array);
    ensure!(
        groups.is_some_and(|x| !x.is_empty()) || methods.is_some_and(|x| !x.is_empty()),
        "select at least one library or qualified method"
    );

    let mut native_groups = Vec::new();
    let mut native_methods = Vec::new();
    let mut flow_selected: BTreeMap<String, Vec<MethodSpec>> = BTreeMap::new();

    for group in groups.into_iter().flatten() {
        let name = group.as_str().context("library name must be a string")?;
        if catalog.contains_key(name) {
            native_groups.push(json!(name));
        } else if let Some(specs) = methods_of(s, name) {
            let entry = flow_selected.entry(name.to_owned()).or_default();
            for spec in specs {
                if !entry.iter().any(|existing| existing.contract_key == spec.contract_key) {
                    entry.push(spec);
                }
            }
        } else {
            bail!("unknown or unavailable library {name}");
        }
    }

    for method in methods.into_iter().flatten() {
        let name = method.as_str().context("method name must be a string")?;
        let (group, item) = name
            .split_once('.')
            .context("use a qualified method such as io.read")?;
        if libraries::target(catalog, name).is_some() {
            native_methods.push(json!(name));
        } else if let Some(specs) = methods_of(s, group)
            && let Some(spec) = specs.into_iter().find(|spec| spec.name == item)
        {
            let entry = flow_selected.entry(group.to_owned()).or_default();
            if !entry.iter().any(|existing| existing.contract_key == spec.contract_key) {
                entry.push(spec);
            }
        } else {
            bail!("unknown or unavailable method {name}");
        }
    }

    let overview = args["overview"] == true;
    let mut text = String::new();
    let mut native_keys = BTreeSet::new();
    if !native_groups.is_empty() || !native_methods.is_empty() {
        let mut native_args = json!({"overview": overview});
        if !native_groups.is_empty() {
            native_args["libraries"] = Value::Array(native_groups);
        }
        if !native_methods.is_empty() {
            native_args["methods"] = Value::Array(native_methods);
        }
        let selected = declarations::selected(catalog, &native_args)?;
        native_keys.extend(selected.keys().cloned());
        text.push_str(&if overview {
            declarations::overview(&selected, definitions)
        } else {
            declarations::headers(&selected, definitions)
        });
    }

    for (library, specs) in &flow_selected {
        let section = if overview {
            format_overview(library, specs)
        } else {
            format_headers(library, specs)
        };
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&section);
    }

    let key = if overview { "overview" } else { "declarations" };
    Ok((json!({ key: text }), native_keys))
}
