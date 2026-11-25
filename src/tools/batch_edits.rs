//! Multi-file edits share the single-file edit locks and replacement semantics.
use crate::{
    tools::file_edit,
    tools::{TextEdit, Workspace},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub const NAME: &str = "apply_edits";
const MAX_FILES: usize = 32;
const MAX_BYTES_PER_FILE: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 24 * 1024 * 1024;

#[derive(Deserialize)]
struct FileEdit {
    path: String,
    edits: Vec<TextEdit>,
    expected_code_hash: Option<String>,
}

pub fn paths(root: &Path, args: &Value) -> Result<Vec<PathBuf>> {
    let files = args["files"].as_array().context("files must be an array")?;
    ensure!(
        (1..=MAX_FILES).contains(&files.len()),
        "files must contain 1 to {MAX_FILES} entries"
    );
    let root = root.canonicalize()?;
    let mut seen = BTreeSet::new();
    let mut unique = Vec::with_capacity(files.len());
    for f in files {
        let p = file_edit::workspace_path(
            &root,
            f["path"].as_str().context("edit path is required")?,
        )?;
        let rel = p.strip_prefix(&root)?;
        ensure!(
            !rel.components().any(|c| c
                .as_os_str()
                .to_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(".git")
                    || s.eq_ignore_ascii_case(".codex-workspace-mcp"))),
            "cannot edit Git metadata or agent state"
        );
        #[cfg(windows)]
        let identity = p.to_string_lossy().to_lowercase();
        #[cfg(not(windows))]
        let identity = p.clone();
        if seen.insert(identity) {
            unique.push(p);
        }
    }
    Ok(unique)
}

#[derive(Debug, thiserror::Error)]
#[error("batch preflight failed; no files changed: file_index={file_index}, path={path}, edit_index={edit_index:?}")]
pub(crate) struct PreflightFailure {
    pub file_index: usize,
    pub path: String,
    pub edit_index: Option<usize>,
}

fn edit_index(error: &anyhow::Error) -> Option<usize> {
    match error.downcast_ref::<crate::tools::ToolError>()? {
        crate::tools::ToolError::IndexedEdit { index, .. } => Some(*index),
        _ => None,
    }
}

struct Prepared {
    original: Vec<u8>,
    updated: Vec<u8>,
}
fn prepare(path: &Path, request: &FileEdit) -> Result<Prepared> {
    ensure!(
        (1..=128).contains(&request.edits.len()),
        "each file requires 1 to 128 edits"
    );
    let size = request
        .edits
        .iter()
        .map(|e| e.old_text.len().saturating_add(e.new_text.len()))
        .fold(0usize, usize::saturating_add);
    ensure!(size <= MAX_BYTES_PER_FILE, "edit text exceeds 2 MiB");
    let meta = fs::metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= MAX_BYTES_PER_FILE as u64,
        "requires an existing regular UTF-8 file of at most 2 MiB"
    );
    let original = fs::read(path)?;
    ensure!(original.len() <= MAX_BYTES_PER_FILE, "file exceeds 2 MiB");
    file_edit::check_hash(Some(&original), request.expected_code_hash.as_deref())?;
    let updated =
        file_edit::text_edits(std::str::from_utf8(&original)?, &request.edits)?.into_bytes();
    ensure!(
        updated.len() <= MAX_BYTES_PER_FILE,
        "updated file exceeds 2 MiB"
    );
    Ok(Prepared { original, updated })
}

fn publish(
    targets: &[PathBuf],
    prepared: &[Result<Prepared>],
    atomic: bool,
) -> Result<Vec<Result<bool>>> {
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut committed: Vec<usize> = Vec::new();
    for (i, (path, edit)) in targets.iter().zip(prepared).enumerate() {
        let result = match edit {
            Ok(edit) => {
                file_edit::commit(path, Some(&edit.original), &edit.updated).map_err(Into::into)
            }
            Err(error) => Err(anyhow::anyhow!("{error:#}")),
        };
        match result {
            Ok(true) => committed.push(i),
            Err(error) if atomic => {
                let mut rollback_failures = Vec::new();
                for i in committed.into_iter().rev() {
                    let edit = prepared[i].as_ref().unwrap();
                    if let Err(rollback) =
                        file_edit::commit(&targets[i], Some(&edit.updated), &edit.original)
                    {
                        rollback_failures.push(format!("{}: {rollback}", targets[i].display()));
                    }
                }
                if !rollback_failures.is_empty() {
                    anyhow::bail!(
                        "batch write failed: {error}; rollback incomplete: {}",
                        rollback_failures.join("; ")
                    );
                }
                return Err(error).with_context(|| {
                    format!(
                        "batch write failed at {}; restored prior files",
                        path.display()
                    )
                });
            }
            _ => {}
        }
        outcomes.push(result);
    }
    Ok(outcomes)
}

pub fn apply(workspace: &Workspace, args: &Value) -> Result<Value> {
    let root = workspace.root().canonicalize()?;
    let targets = paths(&root, args)?;
    let atomic = args["atomic"].as_bool().unwrap_or(true);
    let files = args["files"].as_array().unwrap();
    let mut lock_paths = targets.clone();
    #[cfg(windows)]
    lock_paths.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    #[cfg(not(windows))]
    lock_paths.sort();
    let locks = lock_paths
        .iter()
        .map(|p| file_edit::file_lock(p))
        .collect::<Vec<_>>();
    let guards = locks
        .iter()
        .map(|l| l.lock().unwrap_or_else(|e| e.into_inner()))
        .collect::<Vec<_>>();
    ensure!(
        targets == paths(&root, args)?,
        "batch file location changed while waiting for locks"
    );
    let mut merged: Vec<(PathBuf, FileEdit)> = Vec::with_capacity(targets.len());
    let mut index_by_path: std::collections::BTreeMap<_, usize> = std::collections::BTreeMap::new();
    for file in files {
        let request = serde_json::from_value::<FileEdit>(file.clone()).context("invalid file edit")?;
        let path = file_edit::workspace_path(&root, &request.path)?;
        #[cfg(windows)]
        let identity = path.to_string_lossy().to_lowercase();
        #[cfg(not(windows))]
        let identity = path.clone();
        if let Some(&slot) = index_by_path.get(&identity) {
            let existing = &mut merged[slot].1;
            match (&existing.expected_code_hash, &request.expected_code_hash) {
                (Some(a), Some(b)) if a != b => {
                    anyhow::bail!("conflicting expected_code_hash for {}", request.path);
                }
                (None, Some(hash)) => existing.expected_code_hash = Some(hash.clone()),
                _ => {}
            }
            existing.edits.extend(request.edits);
        } else {
            index_by_path.insert(identity, merged.len());
            merged.push((path, request));
        }
    }
    let mut used = 0usize;
    let mut prepared = Vec::with_capacity(merged.len());
    for (file_index, (path, request)) in merged.iter().enumerate() {
        let edit = prepare(path, request)
            .with_context(|| format!("{}", request.path))
            .and_then(|edit| {
                used = used
                    .saturating_add(edit.original.len())
                    .saturating_add(edit.updated.len());
                ensure!(
                    used <= MAX_TOTAL_BYTES,
                    "batch exceeds 24 MiB combined content budget"
                );
                Ok(edit)
            });
        if atomic {
            prepared.push(Ok(edit.map_err(|error| {
                let edit_index = edit_index(&error);
                error.context(PreflightFailure { file_index, path: request.path.clone(), edit_index })
            })?));
        } else {
            prepared.push(edit);
        }
    }
    if atomic {
        for (path, edit) in targets.iter().zip(&prepared) {
            ensure!(
                file_edit::snapshot(path)?.as_deref() == Some(&edit.as_ref().unwrap().original),
                "batch preflight conflict at {}; no files changed",
                path.display()
            );
        }
    }
    let outcomes = publish(&targets, &prepared, atomic)?;
    drop(guards);
    let mut files = Vec::new();
    let mut changed = 0;
    let mut failed = 0;
    for (file_index, ((path, edit), outcome)) in targets.into_iter().zip(prepared).zip(outcomes).enumerate() {
        let rel = path
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace('\\', "/");
        match outcome {
            Ok(wrote) => {
                let updated = edit.unwrap().updated;
                changed += usize::from(wrote);
                let index = crate::tools::refresh_written_index(&root, &path, wrote);
                files.push(json!({"path":rel,"changed":wrote,"bytes_written":if wrote {updated.len()} else {0},"code_hash":crate::index::symbol_description::content_hash(&updated),"index_refresh":index}));
            }
            Err(error) => {
                failed += 1;
                files.push(json!({"file_index":file_index,"path":rel,"edit_index":edit.as_ref().err().and_then(edit_index),"changed":false,"error":format!("{error:#}")}));
            }
        }
    }
    Ok(json!({"atomic":atomic,"changed":changed,"failed":failed,"files":files}))
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":NAME,"description":"Apply exact text edits across files. By default preflight every file before writing and roll back prior writes if publishing fails. For independent files, atomic=false enables per-file atomic commits without rolling back successful files.","inputSchema":{"type":"object","required":["files"],"properties":{
        "atomic":{"type":"boolean","default":true},"files":{"type":"array","minItems":1,"maxItems":MAX_FILES,"items":{
            "type":"object","required":["path","edits"],"properties":{"path":{"type":"string"},"expected_code_hash":{"type":"string"},"edits":{"type":"array","minItems":1,"maxItems":128,"items":{"type":"object","required":["old_text","new_text"],"properties":{"old_text":{"type":"string"},"new_text":{"type":"string"},"line_hint":{"type":"integer","minimum":1,"description":"Disambiguate nearby exact old_text matches using a 1-based start line"}}}}}}}}}}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, Workspace) {
        let root =
            std::env::temp_dir().join(format!("batch-{}", crate::server::agent_service::uuid_like()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), "old A\r\n").unwrap();
        fs::write(root.join("b.txt"), "old B\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        (root, workspace)
    }

    #[test]
    fn default_preflight_writes_nothing_when_second_edit_fails() {
        let (root, work) = fixture();
        let mut args = json!({"files":[{"path":"a.txt","edits":[{"old_text":"old A","new_text":"new A"}]},{"path":"b.txt","edits":[{"old_text":"missing","new_text":"new B"}]}]});
        assert!(
            apply(&work, &args)
                .unwrap_err()
                .to_string()
                .contains("no files changed")
        );
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"old A\r\n");
        assert_eq!(fs::read(root.join("b.txt")).unwrap(), b"old B\n");
        args["atomic"] = json!(false);
        let value = apply(&work, &args).unwrap();
        assert_eq!(value["changed"], 1);
        assert_eq!(value["failed"], 1);
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"new A\r\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publication_failure_rolls_back_prior_file_without_overwriting_external_change() {
        let (root, _) = fixture();
        let paths = vec![root.join("a.txt"), root.join("b.txt")];
        let prepared = vec![
            Ok(Prepared {
                original: b"old A\r\n".to_vec(),
                updated: b"new A\r\n".to_vec(),
            }),
            Ok(Prepared {
                original: b"old B\n".to_vec(),
                updated: b"new B\n".to_vec(),
            }),
        ];
        fs::write(&paths[1], "external").unwrap();
        let err = publish(&paths, &prepared, true).unwrap_err().to_string();
        assert!(err.contains("restored prior files"), "{err}");
        assert_eq!(fs::read(&paths[0]).unwrap(), b"old A\r\n");
        assert_eq!(fs::read(&paths[1]).unwrap(), b"external");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_file_entries_merge_sequentially_in_order() {
        let (root, work) = fixture();
        fs::write(root.join("a.txt"), "line 1\nline 2\nline 3\n").unwrap();
        let args = json!({
            "files": [
                {"path": "a.txt", "edits": [{"old_text": "line 1\nline 2", "new_text": "LINE 1\nline 2"}]},
                {"path": "b.txt", "edits": [{"old_text": "old B", "new_text": "new B"}]},
                {"path": "a.txt", "edits": [{"old_text": "line 2\nline 3", "new_text": "LINE 2\nLINE 3"}]}
            ]
        });
        let value = apply(&work, &args).unwrap();
        assert_eq!(value["changed"], 2);
        assert_eq!(value["failed"], 0);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "LINE 1\nLINE 2\nLINE 3\n");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "new B\n");
        fs::remove_dir_all(root).unwrap();
    }
}
