//! Read-only JavaScript orchestration libraries exposed via `require("<name>")`.
//! Sources live in a project skill's `modules/*.js`, an enabled plugin
//! package's `modules/*.js`, or built-in host presets, and execute in isolated
//! read-only Layer-1 module frames.
use crate::agent_program::{self, Program};
use crate::single_agent::Session;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone)]
pub(crate) struct MethodSpec {
    pub name: String,
    pub params: Vec<String>,
    pub description: String,
    pub is_async: bool,
    pub contract_key: String,
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
