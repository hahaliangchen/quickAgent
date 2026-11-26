//! Small, literal-path Git tools. Never accept revisions, options, or shell text.
use crate::{tools::process_sandbox::ExecutionBoundary, tools::Workspace};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub const TOOLS: &[&str] = &["git_blame", "git_commit", "git_restore"];

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}
#[derive(Debug)]
pub struct MetadataMayHaveChanged;
impl std::fmt::Display for MetadataMayHaveChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Git metadata may have changed; inspect the index and HEAD before retrying")
    }
}
impl std::error::Error for MetadataMayHaveChanged {}

pub fn mutates(name: &str) -> bool {
    matches!(name, "git_commit" | "git_restore")
}

fn literal_path(root: &Path, raw: &str) -> Result<(String, PathBuf)> {
    ensure!(
        !raw.is_empty() && raw.len() <= 4096 && !raw.contains(['\0', '\r', '\n']),
        "expected a relative Git file path"
    );
    #[cfg(windows)]
    let normalized = raw.replace('\\', "/");
    #[cfg(not(windows))]
    let normalized = {
        ensure!(!raw.contains('\\'), "expected a relative Git file path");
        raw.to_owned()
    };
    let mut parts = Vec::new();
    let root = root.canonicalize()?;
    let mut lexical = root.clone();
    for part in Path::new(&normalized).components() {
        let Component::Normal(part) = part else {
            anyhow::bail!("expected a relative Git file path without traversal")
        };
        let s = part.to_str().context("path must be UTF-8")?;
        ensure!(
            !s.eq_ignore_ascii_case(".git") && !s.eq_ignore_ascii_case(".codex-workspace-mcp"),
            "Git metadata and agent state are not valid targets"
        );
        lexical.push(part);
        if let Ok(meta) = std::fs::symlink_metadata(&lexical) {
            ensure!(
                !meta.file_type().is_symlink(),
                "Git tool does not follow symbolic links: {raw}"
            );
        }
        parts.push(s);
    }
    ensure!(!parts.is_empty(), "expected a file path");
    let path = crate::tools::file_edit::workspace_path(&root, &normalized)?;
    #[cfg(windows)]
    let same_target = path
        .to_string_lossy()
        .eq_ignore_ascii_case(&lexical.to_string_lossy());
    #[cfg(not(windows))]
    let same_target = path == lexical;
    ensure!(
        same_target && !path.is_dir(),
        "Git tool requires an explicit file path: {raw}"
    );
    Ok((parts.join("/"), path))
}

pub fn mutation_paths(root: &Path, args: &Value) -> Result<Vec<(String, PathBuf)>> {
    let paths = args["paths"]
        .as_array()
        .context("paths must be an explicit array")?;
    ensure!(
        (1..=32).contains(&paths.len()),
        "paths must contain 1 to 32 file paths"
    );
    let mut seen = BTreeSet::new();
    paths
        .iter()
        .map(|path| {
            let (name, p) =
                literal_path(root, path.as_str().context("paths must contain strings")?)?;
            #[cfg(windows)]
            let key = name.to_lowercase();
            #[cfg(not(windows))]
            let key = name.clone();
            ensure!(seen.insert(key), "duplicate Git path: {name}");
            Ok((name, p))
        })
        .collect()
}

async fn git(
    workspace: &Workspace,
    argv: Vec<String>,
    boundary: ExecutionBoundary,
    cancel: &CancellationToken,
) -> Result<String> {
    let name = argv.first().cloned().unwrap_or_default();
    let base = [
        "--literal-pathspecs",
        "--no-pager",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "color.ui=never",
    ];
    let args = base
        .into_iter()
        .map(str::to_owned)
        .chain(argv)
        .collect::<Vec<_>>();
    let result = crate::tools::program_execution::execute_with_boundary(
        workspace,
        &json!({"program":"git","args":args,"timeout_seconds":45,"max_output_bytes":262144}),
        cancel,
        boundary,
    )
    .await?;
    let err = result["stderr"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| result["error_message"].as_str())
        .unwrap_or("no details");
    ensure!(
        result["process_success"] == true,
        "git {name} failed: {}",
        crate::server::builtin_plugins::git::backend::redact(&err.chars().take(1400).collect::<String>())
    );
    ensure!(
        result["stdout_truncated"] != true,
        "git {name} exceeded its output limit"
    );
    Ok(result["stdout"]
        .as_str()
        .context("Git output is not valid UTF-8")?
        .to_owned())
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

fn with_paths(options: &[&str], paths: &[(String, PathBuf)]) -> Vec<String> {
    argv(options)
        .into_iter()
        .chain(["--".to_owned()])
        .chain(paths.iter().map(|p| p.0.clone()))
        .collect()
}

fn commit_commands(message: &str, paths: &[(String, PathBuf)]) -> (Vec<String>, Vec<String>) {
    (
        with_paths(&["add"], paths),
        with_paths(&["commit", "--only", "-m", message], paths),
    )
}

pub async fn execute(
    workspace: &Workspace,
    name: &str,
    args: &Value,
    boundary: ExecutionBoundary,
    cancel: &CancellationToken,
) -> Result<Value> {
    let selected_repo = args["repo"].as_str();
    if let Some(repo) = selected_repo {
        ensure!(!repo.trim().is_empty() && repo.len() <= 4096, "repo must be a valid directory path");
        ensure!(!repo.split(['/', '\\']).any(|part| matches!(part, ".codex-workspace-mcp" | ".ssh" | ".aws" | ".gnupg" | ".kube")), "sensitive directories are not valid Git repositories");
    }
    let selected_workspace = selected_repo.map(Workspace::new).transpose()?;
    let workspace = selected_workspace.as_ref().unwrap_or(workspace);
    ensure!(
        !mutates(name) || boundary != ExecutionBoundary::ReadOnly,
        "Git metadata writes are not allowed in read-only mode"
    );
    let top = git(
        workspace,
        argv(&["rev-parse", "--show-toplevel"]),
        boundary,
        cancel,
    )
    .await?;
    let root = workspace.root().canonicalize()?;
    ensure!(
        Path::new(top.trim_end()).canonicalize()? == root,
        "repo must point to the repository root"
    );
    if name == "git_blame" {
        let (path, _) = literal_path(&root, args["path"].as_str().context("path is required")?)?;
        let first = args["start_line"]
            .as_u64()
            .filter(|n| *n > 0)
            .context("start_line must be positive")?;
        let last = args["end_line"]
            .as_u64()
            .filter(|n| *n >= first && *n - first < 200)
            .context("end_line must include at most 200 lines")?;
        let range = format!("{first},{last}");
        let output = git(
            workspace,
            argv(&["blame", "--line-porcelain", "-L", &range, "--", &path]),
            boundary,
            cancel,
        )
        .await?;
        let rows = parse_blame(&output);
        ensure!(!rows.is_empty(), "Git blame returned no line records");
        return Ok(json!(rows));
    }
    let paths = mutation_paths(&root, args)?;
    let git_dir = git(
        workspace,
        argv(&["rev-parse", "--absolute-git-dir"]),
        boundary,
        cancel,
    )
    .await?;
    let common = git(
        workspace,
        argv(&["rev-parse", "--path-format=absolute", "--git-common-dir"]),
        boundary,
        cancel,
    )
    .await?;
    let git_dir = Path::new(git_dir.trim_end()).canonicalize()?;
    ensure!(
        git_dir.starts_with(&root)
            && Path::new(common.trim_end())
                .canonicalize()?
                .starts_with(&root),
        "Git metadata outside the workspace cannot be written"
    );
    let _git_guard = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Git operation cancelled"),
        lock = git_lock(&git_dir).lock_owned() => lock,
    };
    match name {
        "git_restore" => {
            let staged = args["staged"] == true;
            let opts: &[&str] = if staged {
                &["restore", "--staged"]
            } else {
                &["restore", "--worktree"]
            };
            let restored = git(workspace, with_paths(opts, &paths), boundary, cancel).await;
            if staged {
                restored.context(MetadataMayHaveChanged)?;
            } else {
                restored?;
            }
            if !staged {
                for (_, path) in &paths {
                    crate::index::source_read::invalidate(path);
                    let _ = crate::tools::refresh_written_index(&root, path, true);
                }
            }
            Ok(
                json!({"paths":paths.iter().map(|p|p.0.as_str()).collect::<Vec<_>>(),"files_count":paths.len(),"staged":staged,"git_metadata_changed":staged}),
            )
        }
        "git_commit" => {
            let message = args["message"]
                .as_str()
                .filter(|m| !m.trim().is_empty() && m.len() <= 4096 && !m.contains('\0'))
                .context("message must be non-empty and at most 4096 bytes")?;
            ensure!(
                ![
                    "MERGE_HEAD",
                    "CHERRY_PICK_HEAD",
                    "REVERT_HEAD",
                    "rebase-merge",
                    "rebase-apply",
                    "sequencer"
                ]
                .iter()
                .any(|p| git_dir.join(p).exists()),
                "finish the in-progress Git operation before committing explicit paths"
            );
            let (add, commit) = commit_commands(message, &paths);
            git(workspace, add, boundary, cancel)
                .await
                .context(MetadataMayHaveChanged)?;
            git(workspace, commit, boundary, cancel)
                .await
                .context(MetadataMayHaveChanged)?;
            let commit = git(
                workspace,
                argv(&["rev-parse", "HEAD"]),
                boundary,
                &CancellationToken::new(),
            )
            .await
            .context(MetadataMayHaveChanged)?;
            ensure!(
                matches!(commit.trim().len(), 40 | 64)
                    && commit.trim().bytes().all(|b| b.is_ascii_hexdigit()),
                "could not confirm commit id"
            );
            let listing = git(
                workspace,
                argv(&[
                    "diff-tree",
                    "--root",
                    "--no-commit-id",
                    "--name-only",
                    "--no-renames",
                    "-r",
                    "-z",
                    "HEAD",
                ]),
                boundary,
                &CancellationToken::new(),
            )
            .await
            .context(MetadataMayHaveChanged)?;
            Ok(
                json!({"commit":commit.trim(),"files_count":listing.split_terminator('\0').count(),"summary":message.trim().lines().next().unwrap_or(""),"git_metadata_changed":true}),
            )
        }
        _ => anyhow::bail!("unknown Git tool"),
    }
}

fn git_lock(path: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex, OnceLock, Weak},
    };
    type Lock = tokio::sync::Mutex<()>;
    static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Lock>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks.retain(|_, v| v.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Lock::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

fn parse_blame(output: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut row: Option<Value> = None;
    for l in output.lines() {
        if l.starts_with('\t') {
            if let Some(row) = row.take() {
                rows.push(row);
            }
            continue;
        }
        if let Some(current) = row.as_mut() {
            if let Some(author) = l.strip_prefix("author ") {
                current["author"] = json!(author);
            }
            if let Some(time) = l.strip_prefix("author-time ") {
                current["date"] = json!(
                    time.parse::<i64>()
                        .ok()
                        .and_then(|n| chrono::DateTime::from_timestamp(n, 0))
                        .map(|date| date.to_rfc3339())
                );
            }
            if let Some(summary) = l.strip_prefix("summary ") {
                current["summary"] = json!(summary);
            }
            continue;
        }
        let pieces = l.split_whitespace().collect::<Vec<_>>();
        if pieces.len() >= 3
            && matches!(pieces[0].len(), 40 | 64)
            && pieces[0].bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(line) = pieces[2].parse::<u64>()
        {
            row =
                Some(json!({"line":line,"commit":pieces[0],"author":"","date":null,"summary":""}));
        }
    }
    rows
}

pub fn definitions() -> Vec<Value> {
    let paths = json!({"type":"array","minItems":1,"maxItems":32,"items":{"type":"string"}});
    vec![
        json!({"name":"git_blame","description":"Read line authorship for an inclusive range of up to 200 lines in one file.","inputSchema":{"type":"object","required":["path","start_line","end_line"],"properties":{"repo":{"type":"string"},"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}}}}),
        json!({"name":"git_commit","description":"Stage and commit only explicit file paths, never other staged entries. On failure the requested files may remain staged. Git history cannot be undone by workspace undo.","inputSchema":{"type":"object","required":["paths","message"],"properties":{"repo":{"type":"string"},"paths":paths,"message":{"type":"string","minLength":1,"maxLength":4096}}}}),
        json!({"name":"git_restore","description":"Restore specified files from the index; staged=true instead restores those index entries from HEAD without changing the worktree.","inputSchema":{"type":"object","required":["paths"],"properties":{"repo":{"type":"string"},"paths":paths,"staged":{"type":"boolean","default":false}}}}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

    fn run(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    #[tokio::test]
    async fn blame_and_restore_worktree_and_index_are_scoped_to_literal_paths() {
        let dir =
            std::env::temp_dir().join(format!("git-tools-{}", crate::server::agent_service::uuid_like()));
        fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "fixture"]);
        run(&dir, &["config", "core.autocrlf", "false"]);
        fs::write(dir.join("a.txt"), "original\nline two\n").unwrap();
        fs::write(dir.join("b.txt"), "original B\n").unwrap();
        run(&dir, &["add", "--", "a.txt", "b.txt"]);
        let tree = run(&dir, &["write-tree"]);
        let obj = run(
            &dir,
            &["commit-tree", tree.trim(), "-m", "Fixture baseline"],
        );
        run(&dir, &["update-ref", "HEAD", obj.trim()]);
        let workspace = Workspace::new(&dir).unwrap();
        let cancel = CancellationToken::new();
        let blame = execute(
            &workspace,
            "git_blame",
            &json!({"path":"a.txt","start_line":2,"end_line":2}),
            ExecutionBoundary::Host,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(blame[0]["line"], 2);
        assert_eq!(blame[0]["commit"], obj.trim());
        assert_eq!(blame[0]["author"], "Fixture");
        assert_eq!(blame[0]["summary"], "Fixture baseline");
        fs::write(dir.join("a.txt"), "staged A\n").unwrap();
        fs::write(dir.join("b.txt"), "staged B\n").unwrap();
        run(&dir, &["add", "--", "a.txt", "b.txt"]);
        fs::write(dir.join("a.txt"), "work A\n").unwrap();
        fs::write(dir.join("b.txt"), "work B\n").unwrap();
        execute(
            &workspace,
            "git_restore",
            &json!({"paths":["a.txt"]}),
            ExecutionBoundary::Host,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "staged A\n");
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "work B\n");
        execute(
            &workspace,
            "git_restore",
            &json!({"paths":["a.txt"],"staged":true}),
            ExecutionBoundary::Host,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            run(&dir, &["diff", "--cached", "--name-only"]).trim(),
            "b.txt"
        );
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "staged A\n");
        let paths = mutation_paths(&dir, &json!({"paths":["-n"]})).unwrap();
        let (add, commit) = commit_commands("--amend", &paths);
        assert_eq!(add, ["add", "--", "-n"]);
        assert_eq!(commit, ["commit", "--only", "-m", "--amend", "--", "-n"]);
        assert!(mutation_paths(&dir, &json!({"paths":["../outside"]})).is_err());
        assert!(mutation_paths(&dir, &json!({"paths":[".git/index"]})).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
