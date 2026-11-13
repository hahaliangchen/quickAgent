//! The host's folder for a project: `.codex-workspace-mcp/projects/<name>-<key>/`
//! under the host root, outside the project. It holds the flow code the AIs
//! write, per chat, and the project's saved flows; the conversation records
//! stay in the host database.
use anyhow::Result;
use std::path::{Path, PathBuf};

fn projects(root: &Path) -> PathBuf {
    root.join(".codex-workspace-mcp").join("projects")
}

/// Hash of the project's canonical path: what told projects apart before the
/// folder carried their name, and still keeps two same-named projects apart.
pub(super) fn project_key(project: &Path) -> String {
    let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    crate::visual_artifacts::hash(project.to_string_lossy().as_bytes())
}

/// The folder of the project a chat works in, named after the project.
pub(super) fn project_dir(root: &Path, task: &str) -> Result<PathBuf> {
    let project = crate::session_browser::task_workspace(root, task)?;
    let key = project_key(&project);
    let name = project
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let name = if name.trim_matches('.').is_empty() {
        "project".to_owned()
    } else {
        name
    };
    Ok(projects(root).join(format!("{name}-{}", &key[..8])))
}

fn chat_name(task: &str) -> String {
    if !task.is_empty()
        && task
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        task.to_owned()
    } else {
        crate::visual_artifacts::hash(task.as_bytes())[..16].to_owned()
    }
}

/// A chat's folder. An existing one is found by the chat alone, so a chat
/// whose record is already gone can still be cleaned up.
pub(super) fn chat_dir(root: &Path, task: &str) -> Result<PathBuf> {
    if let Some(existing) = existing_chat_dir(root, task) {
        return Ok(existing);
    }
    Ok(project_dir(root, task)?.join("chats").join(chat_name(task)))
}

pub(super) fn existing_chat_dir(root: &Path, task: &str) -> Option<PathBuf> {
    let chat = chat_name(task);
    std::fs::read_dir(projects(root))
        .ok()?
        .flatten()
        .map(|project| project.path().join("chats").join(&chat))
        .find(|dir| dir.is_dir())
}

/// Write a main program the AI handed over with write_program: one file per
/// program, named like a project's code file after what it does (`stem`),
/// with -2, -3 when the name is taken. Returns the path relative to the root.
pub(super) fn write_program(root: &Path, task: &str, stem: &str, source: &str) -> Result<String> {
    let dir = chat_dir(root, task)?;
    std::fs::create_dir_all(&dir)?;
    let mut path = dir.join(format!("{stem}.js"));
    let mut attempt = 1;
    while path.exists() {
        attempt += 1;
        path = dir.join(format!("{stem}-{attempt}.js"));
    }
    std::fs::write(&path, source)?;
    Ok(path
        .strip_prefix(root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/"))
}
