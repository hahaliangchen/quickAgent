//! Runs the user's own git binary with argv, never a shell string, so their
//! config, credential helper, hooks and ssh-agent apply unchanged.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, sync::LazyLock, time::Duration};
use tokio::process::Command;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const MAX_PATCH: usize = 400 * 1024;
const FIELD: char = '\0';

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReadRequest {
    pub workspace_root: Option<String>,
    view: String,
    commit: Option<String>,
    path: Option<String>,
    branch: Option<String>,
    limit: Option<usize>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncRequest {
    pub workspace_root: Option<String>,
    action: String,
}

pub fn tool_definitions() -> Vec<Value> {
    serde_json::from_str(include_str!("tools.json")).expect("compiled git tool schemas")
}

/// Credentials embedded in remote URLs must not reach the page or the AI.
pub fn redact(text: &str) -> String {
    static URL_USER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/@\s]+@").unwrap());
    URL_USER.replace_all(text, "$1").into_owned()
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

async fn run(root: &Path, args: &[&str], timeout: Duration) -> Result<Output> {
    let child = Command::new("git")
        .args(["-c", "core.quotepath=off", "-c", "color.ui=never"])
        .args(args)
        .current_dir(root)
        // Fail instead of waiting for a password nobody can type.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        // Reads must not take index.lock away from a command the agent is running.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("GIT_UNAVAILABLE: 本机没有找到 git，请先安装 git。")
        }
        other => other?,
    };
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .with_context(|| format!("git {} 超时", args[0]))??;
    Ok(Output {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: redact(String::from_utf8_lossy(&output.stderr).trim()),
    })
}

async fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = run(root, args, Duration::from_secs(60)).await?;
    ensure!(output.code == 0, "git {} 失败：{}", args[0], output.stderr);
    Ok(output.stdout)
}

fn repository(root: &Path) -> Result<&Path> {
    ensure!(root.is_dir(), "目录不存在：{}", root.display());
    Ok(root)
}

fn commit_id(value: &str) -> Result<&str> {
    ensure!(
        (4..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "commit 必须是提交哈希"
    );
    Ok(value)
}

pub async fn read(root: &Path, request: ReadRequest) -> Result<Value> {
    let root = repository(root)?;
    let top = run(root, &["rev-parse", "--show-toplevel"], Duration::from_secs(20)).await?;
    ensure!(
        top.code == 0,
        "NOT_A_REPOSITORY: {} 不是 git 仓库",
        root.display()
    );
    let top = Path::new(top.stdout.trim()).to_owned();
    match request.view.as_str() {
        "overview" => overview(&top, request.branch.as_deref(), request.limit).await,
        "commit" => match request.commit.as_deref() {
            Some(commit) => commit_detail(&top, commit_id(commit)?).await,
            None => working_tree(&top).await,
        },
        "files" => files(root, &top).await,
        "file" => {
            let path = request.path.as_deref().context("file 需要 path")?;
            let commit = request.commit.as_deref().map(commit_id).transpose()?;
            file(&top, commit, path).await
        }
        "file_diff" => {
            let path = request.path.as_deref().context("file_diff 需要 path")?;
            ensure!(!path.is_empty() && !path.contains(FIELD), "path 无效");
            let commit = request.commit.as_deref().map(commit_id).transpose()?;
            file_diff(&top, commit, path).await
        }
        other => bail!("未知的 view：{other}"),
    }
}

async fn overview(top: &Path, branch: Option<&str>, limit: Option<usize>) -> Result<Value> {
    let limit = limit.unwrap_or(200).clamp(1, 1000).to_string();
    let format = "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%D%x00%s%x1e";
    let mut log = vec!["log", "--date-order", "-n", &limit, format];
    match branch {
        Some(branch) => {
            ensure!(!branch.starts_with('-'), "branch 无效");
            log.push(branch);
        }
        None => log.extend(["--exclude=refs/stash", "--all"]),
    }
    log.push("--");
    let (status, refs, stashes, commits) = tokio::join!(
        git(
            top,
            &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"]
        ),
        git(
            top,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(upstream:short)%00%(HEAD)%00%(committerdate:unix)",
                "refs/heads",
                "refs/remotes",
            ]
        ),
        git(top, &["stash", "list", "--format=%gd%x00%H%x00%at%x00%s"]),
        run(top, &log, Duration::from_secs(60)),
    );
    let commits = commits?;
    // A repository without commits has nothing to log yet.
    let commits = if commits.code == 0 {
        parse_log(&commits.stdout)
    } else if commits.stderr.contains("does not have any commits") {
        vec![]
    } else {
        bail!("git log 失败：{}", commits.stderr)
    };
    let mut status = parse_status(&status?);
    status["repository"] = json!(top.file_name().map(|n| n.to_string_lossy()));
    status["root"] = json!(top.display().to_string());
    status["branches"] = json!(parse_refs(&refs?));
    status["stashes"] = json!(
        stashes?
            .lines()
            .filter_map(|line| {
                let f: Vec<_> = line.split(FIELD).collect();
                (f.len() == 4).then(|| json!({"name":f[0],"hash":f[1],"time":f[2].parse::<i64>().ok(),"subject":f[3]}))
            })
            .collect::<Vec<_>>()
    );
    status["commits"] = json!(commits);
    Ok(status)
}

fn parse_log(raw: &str) -> Vec<Value> {
    raw.split('\x1e')
        .map(|record| record.trim_start_matches('\n'))
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let f: Vec<_> = record.splitn(7, FIELD).collect();
            (f.len() == 7).then(|| {
                json!({
                    "hash": f[0],
                    "parents": f[1].split_whitespace().collect::<Vec<_>>(),
                    "author": f[2],
                    "email": f[3],
                    "time": f[4].parse::<i64>().ok(),
                    "refs": f[5].split(", ").filter(|r| !r.is_empty()).collect::<Vec<_>>(),
                    "subject": f[6],
                })
            })
        })
        .collect()
}

fn parse_refs(raw: &str) -> Vec<Value> {
    raw.lines()
        .filter_map(|line| {
            let f: Vec<_> = line.split(FIELD).collect();
            if f.len() != 5 || f[0].ends_with("/HEAD") {
                return None;
            }
            let (name, remote) = match f[0].strip_prefix("refs/heads/") {
                Some(name) => (name, false),
                None => (f[0].strip_prefix("refs/remotes/")?, true),
            };
            Some(json!({
                "name": name, "remote": remote, "hash": f[1],
                "upstream": (!f[2].is_empty()).then_some(f[2]),
                "current": f[3] == "*", "time": f[4].parse::<i64>().ok(),
            }))
        })
        .collect()
}

/// `git status --porcelain=v2 --branch -z`: headers, then one entry per path.
fn parse_status(raw: &str) -> Value {
    let (mut head, mut oid, mut upstream, mut ahead, mut behind) = (None, None, None, 0, 0);
    let mut changes = vec![];
    let mut entries = raw.split(FIELD).filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if let Some(header) = entry.strip_prefix("# ") {
            match header.split_once(' ') {
                Some(("branch.head", v)) if v != "(detached)" => head = Some(v.to_owned()),
                Some(("branch.oid", v)) if v != "(initial)" => oid = Some(v.to_owned()),
                Some(("branch.upstream", v)) => upstream = Some(v.to_owned()),
                Some(("branch.ab", v)) => {
                    for part in v.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = entry.as_bytes()[0];
        let (xy, path, orig) = match kind {
            b'1' => (entry.get(2..4), entry.splitn(9, ' ').nth(8), None),
            b'2' => (entry.get(2..4), entry.splitn(10, ' ').nth(9), entries.next()),
            b'u' => (entry.get(2..4), entry.splitn(11, ' ').nth(10), None),
            b'?' => (Some("??"), entry.get(2..), None),
            _ => continue,
        };
        let (Some(xy), Some(path)) = (xy, path) else {
            continue;
        };
        changes.push(json!({
            "path": path, "orig_path": orig,
            "index": &xy[..1], "worktree": &xy[1..],
            "conflict": kind == b'u',
        }));
    }
    json!({
        "head": head, "head_hash": oid, "upstream": upstream,
        "ahead": ahead, "behind": behind, "changes": changes,
    })
}

/// `--numstat -z`: `added\tdeleted\tpath\0`, or for a rename
/// `added\tdeleted\t\0old\0new\0`. Binary files count as `-`.
fn parse_numstat(raw: &str) -> Vec<Value> {
    let mut files = vec![];
    let mut tokens = raw.split(FIELD);
    while let Some(token) = tokens.next() {
        let mut f = token.trim_start_matches('\n').splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let (orig, path) = if path.is_empty() {
            match (tokens.next(), tokens.next()) {
                (Some(old), Some(new)) => (Some(old), new),
                _ => break,
            }
        } else {
            (None, path)
        };
        files.push(json!({
            "path": path, "orig_path": orig,
            "added": added.parse::<u64>().ok(), "deleted": deleted.parse::<u64>().ok(),
        }));
    }
    files
}

/// Tracked and untracked files, minus what .gitignore hides.
async fn files(root: &Path, top: &Path) -> Result<Value> {
    const MAX_FILES: usize = 20_000;
    let raw = git(top, &["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).await?;
    let mut paths: Vec<_> = raw.split(FIELD).filter(|p| !p.is_empty()).collect();
    paths.sort_unstable();
    paths.dedup();
    let truncated = paths.len() > MAX_FILES;
    paths.truncate(MAX_FILES);
    // Paths stay repository-relative; the prefix says where the workspace sits.
    let prefix = git(root, &["rev-parse", "--show-prefix"]).await?;
    Ok(json!({"root": top.display().to_string(), "prefix": prefix.trim(), "paths": paths, "truncated": truncated}))
}

/// One file as text: the working copy, or its blob at a commit.
async fn file(top: &Path, commit: Option<&str>, path: &str) -> Result<Value> {
    const MAX_FILE: u64 = 1024 * 1024;
    let relative = Path::new(path);
    ensure!(
        !path.is_empty()
            && relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "path 必须是仓库内的相对路径"
    );
    let (bytes, content) = match commit {
        Some(commit) => {
            let spec = format!("{commit}:{path}");
            let size: u64 = git(top, &["cat-file", "-s", &spec]).await?.trim().parse()?;
            if size > MAX_FILE {
                return Ok(json!({"path": path, "commit": commit, "bytes": size, "too_large": true}));
            }
            (size, git(top, &["cat-file", "blob", &spec]).await?)
        }
        None => {
            let full = top.join(relative).canonicalize().with_context(|| format!("文件不存在：{path}"))?;
            ensure!(full.starts_with(top.canonicalize()?) && full.is_file(), "不是仓库内的文件：{path}");
            let size = full.metadata()?.len();
            if size > MAX_FILE {
                return Ok(json!({"path": path, "commit": null, "bytes": size, "too_large": true}));
            }
            (size, String::from_utf8_lossy(&tokio::fs::read(&full).await?).into_owned())
        }
    };
    let binary = content.contains('\0');
    Ok(json!({
        "path": path, "commit": commit, "bytes": bytes, "too_large": false, "binary": binary,
        "content": if binary { Value::Null } else { json!(content) },
    }))
}

async fn parents(top: &Path, commit: &str) -> Result<Vec<String>> {
    let line = git(top, &["rev-list", "--parents", "-n", "1", commit]).await?;
    Ok(line.split_whitespace().skip(1).map(str::to_owned).collect())
}

/// A merge is shown against its first parent, as history views usually do.
async fn commit_detail(top: &Path, commit: &str) -> Result<Value> {
    let meta = git(
        top,
        &[
            "show",
            "-s",
            "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ct%x00%D%x00%B",
            commit,
            "--",
        ],
    )
    .await?;
    let f: Vec<_> = meta.splitn(9, FIELD).collect();
    ensure!(f.len() == 9, "无法解析提交 {commit}");
    let parents: Vec<_> = f[1].split_whitespace().collect();
    let base = parents.first().copied().unwrap_or(EMPTY_TREE);
    let files = git(top, &["diff", "--numstat", "-z", "-M", base, f[0], "--"]).await?;
    Ok(json!({
        "hash": f[0], "parents": parents, "author": f[2], "email": f[3],
        "time": f[4].parse::<i64>().ok(), "committer": f[5], "commit_time": f[6].parse::<i64>().ok(),
        "refs": f[7].split(", ").filter(|r| !r.is_empty()).collect::<Vec<_>>(),
        "message": f[8].trim_end(), "files": parse_numstat(&files),
    }))
}

async fn head_or_empty(top: &Path) -> String {
    match git(top, &["rev-parse", "--verify", "-q", "HEAD"]).await {
        Ok(hash) if !hash.trim().is_empty() => hash.trim().to_owned(),
        _ => EMPTY_TREE.to_owned(),
    }
}

async fn working_tree(top: &Path) -> Result<Value> {
    let base = head_or_empty(top).await;
    let diff = ["diff", "--numstat", "-z", "-M", &base, "--"];
    let (tracked, untracked) = tokio::join!(
        git(top, &diff),
        git(top, &["ls-files", "--others", "--exclude-standard", "-z"]),
    );
    let mut files = parse_numstat(&tracked?);
    files.extend(
        untracked?
            .split(FIELD)
            .filter(|p| !p.is_empty())
            .map(|p| json!({"path":p,"orig_path":null,"added":null,"deleted":null,"untracked":true})),
    );
    Ok(json!({"hash": null, "files": files}))
}

async fn file_diff(top: &Path, commit: Option<&str>, path: &str) -> Result<Value> {
    let output = match commit {
        Some(commit) => {
            let parents = parents(top, commit).await?;
            let base = parents.first().map_or(EMPTY_TREE, String::as_str);
            run(top, &["diff", "-M", base, commit, "--", path], Duration::from_secs(60)).await?
        }
        None => {
            let tracked = git(top, &["ls-files", "-z", "--", path]).await?;
            if tracked.is_empty() {
                // `--no-index` exits 1 when the files differ, which is the point.
                let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
                let out = run(top, &["diff", "--no-index", "--", null, path], Duration::from_secs(60)).await?;
                ensure!(out.code <= 1, "git diff 失败：{}", out.stderr);
                out
            } else {
                let base = head_or_empty(top).await;
                run(top, &["diff", &base, "--", path], Duration::from_secs(60)).await?
            }
        }
    };
    ensure!(output.code <= 1, "git diff 失败：{}", output.stderr);
    let mut patch = output.stdout;
    let truncated = patch.len() > MAX_PATCH;
    if truncated {
        let mut end = MAX_PATCH;
        while !patch.is_char_boundary(end) {
            end -= 1;
        }
        patch.truncate(end);
    }
    Ok(json!({"commit": commit, "path": path, "patch": patch, "truncated": truncated}))
}

pub async fn sync(root: &Path, request: SyncRequest) -> Result<Value> {
    let root = repository(root)?;
    let args: &[&str] = match request.action.as_str() {
        "fetch" => &["fetch", "--prune"],
        "pull" => &["pull", "--ff-only"],
        "push" => &["push"],
        other => bail!("未知的 action：{other}"),
    };
    let output = run(root, args, Duration::from_secs(300)).await?;
    let text = redact(output.stdout.trim());
    if output.code != 0 {
        let hint = if output.stderr.contains("terminal prompts disabled")
            || output.stderr.contains("could not read Username")
        {
            "\n没有可用的凭证：先在终端里成功推送一次，让 git 的凭证助手记住它。"
        } else {
            ""
        };
        bail!("git {} 失败：{}{hint}", args[0], output.stderr);
    }
    Ok(json!({"action": request.action, "stdout": text, "stderr": output.stderr}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_in_remote_urls_are_removed() {
        assert_eq!(
            redact("To https://me:ghp_secret@github.com/a/b.git"),
            "To https://github.com/a/b.git"
        );
        assert_eq!(redact("ssh://git@host/x"), "ssh://host/x");
        assert_eq!(redact("no url here"), "no url here");
    }

    #[test]
    fn status_reads_branch_counts_renames_and_untracked() {
        let raw = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 h1 h2 src/a b.rs\0\
2 R. N... 100644 100644 100644 h1 h2 R100 new.rs\0old.rs\0\
? notes.txt\0";
        let status = parse_status(raw);
        assert_eq!(status["head"], "main");
        assert_eq!((status["ahead"].as_i64(), status["behind"].as_i64()), (Some(2), Some(1)));
        let changes = status["changes"].as_array().unwrap();
        assert_eq!(changes[0]["path"], "src/a b.rs");
        assert_eq!(changes[0]["worktree"], "M");
        assert_eq!((&changes[1]["path"], &changes[1]["orig_path"]), (&json!("new.rs"), &json!("old.rs")));
        assert_eq!(changes[2]["index"], "?");
    }

    #[test]
    fn numstat_reads_renames_and_binary_files() {
        let files = parse_numstat("3\t1\tsrc/x.rs\0-\t-\timg.png\x000\t0\t\0a.rs\0b.rs\0");
        assert_eq!(files.len(), 3);
        assert_eq!(files[0]["added"], 3);
        assert_eq!(files[1]["added"], Value::Null);
        assert_eq!((&files[2]["orig_path"], &files[2]["path"]), (&json!("a.rs"), &json!("b.rs")));
    }

    #[tokio::test]
    async fn reads_a_real_repository() {
        let dir = std::env::temp_dir().join(format!("git-plugin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                    .args(args)
                    .current_dir(&dir)
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        sh(&["init", "-q", "-b", "main"]);
        let request = |view: &str| ReadRequest { view: view.into(), ..Default::default() };
        // An empty repository has a branch name and no commits.
        let empty = read(&dir, request("overview")).await.unwrap();
        assert_eq!(empty["commits"], json!([]));
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-qm", "first"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();
        let overview = read(&dir, request("overview")).await.unwrap();
        assert_eq!(overview["head"], "main");
        let commit = overview["commits"][0]["hash"].as_str().unwrap().to_owned();
        assert_eq!(overview["commits"][0]["refs"][0], "HEAD -> main");
        assert_eq!(overview["changes"].as_array().unwrap().len(), 2);

        let detail = read(&dir, ReadRequest { view: "commit".into(), commit: Some(commit.clone()), ..Default::default() }).await.unwrap();
        assert_eq!(detail["message"], "first");
        assert_eq!(detail["files"][0]["added"], 1);
        let listed = read(&dir, request("files")).await.unwrap();
        assert_eq!(listed["paths"], json!(["a.txt", "new.txt"]));
        assert_eq!(listed["prefix"], "");
        let text = |commit: Option<String>| ReadRequest { view: "file".into(), commit, path: Some("a.txt".into()), ..Default::default() };
        assert_eq!(read(&dir, text(None)).await.unwrap()["content"], "one\ntwo\n");
        assert_eq!(read(&dir, text(Some(commit.clone()))).await.unwrap()["content"], "one\n");
        let outside = ReadRequest { view: "file".into(), path: Some("../x".into()), ..Default::default() };
        assert!(read(&dir, outside).await.is_err());
        let worktree = read(&dir, request("commit")).await.unwrap();
        assert_eq!(worktree["files"].as_array().unwrap().len(), 2);

        let diff = |commit: Option<String>, path: &str| ReadRequest { view: "file_diff".into(), commit, path: Some(path.into()), ..Default::default() };
        let first = read(&dir, diff(Some(commit), "a.txt")).await.unwrap();
        assert!(first["patch"].as_str().unwrap().contains("+one"));
        let edited = read(&dir, diff(None, "a.txt")).await.unwrap();
        assert!(edited["patch"].as_str().unwrap().contains("+two"));
        let untracked = read(&dir, diff(None, "new.txt")).await.unwrap();
        assert!(untracked["patch"].as_str().unwrap().contains("+fresh"));

        assert!(read(&dir, ReadRequest { view: "commit".into(), commit: Some("--output=x".into()), ..Default::default() }).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
