//! Per-turn before/after images. Never infer completed edits from model arguments.
use crate::agent_service::AgentServiceState;
use crate::tools::Workspace;
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Json,
    extract::{Path as RoutePath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

// Legacy rewind serialization only. Ordinary mutations and undo share the
// workspace registry below, so a live shell cannot lock unrelated projects.
pub(crate) static MUTATIONS: RwLock<()> = RwLock::const_new(());
static WORKSPACE_MUTATIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Weak<RwLock<()>>>>> = std::sync::OnceLock::new();
static FILE_MUTATIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(PathBuf, PathBuf), std::sync::Weak<Mutex<()>>>>> = std::sync::OnceLock::new();

fn workspace_mutations(workspace: &Path) -> Result<(PathBuf, std::sync::Arc<RwLock<()>>)> {
    let root = fs::canonicalize(workspace)?;
    #[cfg(windows)]
    let root = PathBuf::from(root.to_string_lossy().to_lowercase());
    let mut locks = WORKSPACE_MUTATIONS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = match locks.get(&root).and_then(std::sync::Weak::upgrade) {
        Some(lock) => lock,
        None => {
            let lock = std::sync::Arc::new(RwLock::new(()));
            locks.insert(root.clone(), std::sync::Arc::downgrade(&lock));
            lock
        }
    };
    Ok((root, lock))
}

/// Cross-process lease complements the in-process file mutation lock.
pub(crate) struct ResourceClaim { root: PathBuf, resource: String, owner: String }
impl Drop for ResourceClaim {
    fn drop(&mut self) {
        if let Ok(conn) = crate::agent_service::open_db(&self.root) {
            let _ = conn.execute("DELETE FROM agent_resource_claims WHERE resource=?1 AND owner=?2", params![self.resource,self.owner]);
        }
    }
}
pub(crate) fn resource_key(workspace: &Path, name: &str, args: &Value) -> Result<Option<String>> {
    if matches!(name, "write_file" | "edit_file" | "replace_range") {
        let path = args["path"].as_str().context("write path required")?;
        return Ok(Some(format!("local:{}", crate::file_edit::workspace_path(workspace,path)?.display())));
    }
    Ok(None)
}
pub(crate) fn claim_resource(root: &Path, task: &str, call_id: &str, resource: String) -> Result<ResourceClaim> {
    let owner=format!("main:{task}:{call_id}");
    let mut conn=crate::agent_service::open_db(root)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_resource_claims(resource TEXT PRIMARY KEY,task_id TEXT NOT NULL,owner TEXT NOT NULL,owner_agent TEXT,expires_at INTEGER NOT NULL);")?;
    let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now=chrono::Utc::now().timestamp_millis();
    let previous:Option<(String,i64)>=tx.query_row("SELECT owner,expires_at FROM agent_resource_claims WHERE resource=?1",[&resource],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((other,expiry))=previous {ensure!(other==owner || expiry<=now,"RESOURCE_WRITE_OWNERSHIP_CONFLICT: {resource} is owned by {other}");}
    tx.execute("INSERT INTO agent_resource_claims(resource,task_id,owner,owner_agent,expires_at) VALUES (?1,?2,?3,NULL,?4) ON CONFLICT(resource) DO UPDATE SET task_id=excluded.task_id,owner=excluded.owner,owner_agent=NULL,expires_at=excluded.expires_at",params![resource,task,owner,now+30*60*1000])?;
    tx.commit()?;
    Ok(ResourceClaim{root:root.to_path_buf(),resource,owner})
}

pub(crate) enum MutationGuard {
    File { _workspace: tokio::sync::OwnedRwLockReadGuard<()>, _file: tokio::sync::OwnedMutexGuard<()> },
    Workspace { _workspace: tokio::sync::OwnedRwLockWriteGuard<()> },
}

pub(crate) async fn mutation_guard(workspace: &Path, name: &str, args: &Value, cancel: &CancellationToken) -> Result<MutationGuard> {
    let (root, workspace_lock) = workspace_mutations(workspace)?;
    if !matches!(name, "write_file" | "edit_file" | "replace_range") {
        let guard = tokio::select! { _ = cancel.cancelled() => bail!("Tool execution cancelled"), guard = workspace_lock.write_owned() => guard };
        ensure!(workspace_mutations(workspace)?.0 == root, "workspace location changed while waiting");
        return Ok(MutationGuard::Workspace { _workspace: guard });
    }
    let raw = args["path"].as_str().context("missing file path")?;
    let path = safe_path(workspace, raw)?;
    #[cfg(windows)]
    let path = PathBuf::from(path.to_string_lossy().to_lowercase());
    let file = {
        let mut locks = FILE_MUTATIONS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        let key = (root.clone(), path.clone());
        if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) { lock } else {
            let lock = std::sync::Arc::new(Mutex::new(()));
            locks.insert(key, std::sync::Arc::downgrade(&lock));
            lock
        }
    };
    let workspace_guard = tokio::select! { _ = cancel.cancelled() => bail!("Tool execution cancelled"), guard = workspace_lock.read_owned() => guard };
    let file = tokio::select! { _ = cancel.cancelled() => bail!("Tool execution cancelled"), guard = file.lock_owned() => guard };
    let resolved = safe_path(workspace, raw)?;
    #[cfg(windows)]
    let resolved = PathBuf::from(resolved.to_string_lossy().to_lowercase());
    ensure!(workspace_mutations(workspace)?.0 == root, "workspace location changed while waiting");
    ensure!(resolved == path, "file location changed while waiting");
    Ok(MutationGuard::File { _workspace: workspace_guard, _file: file })
}
static TEMP_ID: AtomicU64 = AtomicU64::new(0);
const FILE_LIMIT: u64 = 2 * 1024 * 1024;
const SNAPSHOT_LIMIT: usize = 32 * 1024 * 1024;

// Keep at most one workspace's bounded byte cache. A cache hit still requires
// walking, safe_path validation and fresh metadata; it is never a before-image.
static SNAPSHOT_CACHE: std::sync::Mutex<Option<CachedSnapshot>> = std::sync::Mutex::new(None);
static CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
std::thread_local! {
    static SNAPSHOT_READS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

struct CachedSnapshot {
    root: PathBuf,
    files: BTreeMap<String, (FileStamp, Vec<u8>)>,
}

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: std::time::SystemTime,
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    fn new(metadata: &fs::Metadata) -> Option<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino(), metadata.ctime(), metadata.ctime_nsec()),
        })
    }

    fn reusable(&self) -> bool {
        // Recent/coarse timestamps can hide another write in the same clock
        // tick. Read those files again rather than trusting cached bytes.
        self.modified.elapsed().is_ok_and(|age| age >= std::time::Duration::from_secs(2))
    }
}

pub(crate) fn invalidate_snapshot_cache(root: &Path) {
    CACHE_GENERATION.fetch_add(1, Ordering::SeqCst);
    let canonical = fs::canonicalize(root).ok();
    let mut cache = SNAPSHOT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if canonical.is_none() || cache.as_ref().is_some_and(|cache| Some(&cache.root) == canonical.as_ref()) {
        *cache = None;
    }
}

#[derive(Clone, PartialEq)]
struct Image {
    exists: bool,
    bytes: Option<Vec<u8>>,
    stamp: String,
}
type Snapshot = BTreeMap<String, Image>;

/// Owned interval state. The runner retains this (and its mutation guard) until
/// the process exits, including when a tool response returns a live session.
pub(crate) struct ShellSnapshot {
    store: PathBuf,
    workspace: PathBuf,
    task: String,
    turn: usize,
    token: String,
    before: Option<Snapshot>,
    managed: Vec<String>,
    errors: Vec<String>,
}

/// The runner supplies effective parallel owners, not raw serial frame IDs.
/// Participants identify the interval's scope, not a per-file author when shared.
pub(crate) struct ShellAttribution {
    interval_id: String,
    owners: Vec<String>,
}

impl ShellAttribution {
    pub(crate) fn new(interval_id: &str, owners: &[String]) -> Result<Self> {
        ensure!(!interval_id.trim().is_empty(), "shell interval id is required");
        ensure!(owners.iter().all(|owner| !owner.trim().is_empty() && !owner.starts_with("@shared_shell:")),
            "shell owners must be normalized parallel owners or @main");
        let owners = owners.iter().cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        Ok(Self { interval_id: interval_id.into(), owners })
    }

    pub(crate) fn writer(&self) -> String {
        if self.owners.len() == 1 { self.owners[0].clone() }
        else { format!("@shared_shell:{}", self.interval_id) }
    }

    pub(crate) fn metadata(&self) -> Value {
        let shared = self.owners.len() != 1;
        json!({"kind":if shared { "shared_shell_interval" } else { "shell_interval" },
            "interval_id":self.interval_id,"owners":self.owners,"writer":self.writer(),
            "attribution":if shared { "unknown" } else { "single_owner" },"shared":shared})
    }
}

fn connection(root: &Path) -> Result<Connection> {
    let conn = crate::agent_service::open_db(root)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_file_changes (
        task_id TEXT NOT NULL, turn INTEGER NOT NULL, path TEXT NOT NULL,
        before_exists INTEGER NOT NULL, before_bytes BLOB, after_exists INTEGER NOT NULL, after_bytes BLOB,
        added INTEGER NOT NULL, deleted INTEGER NOT NULL, binary INTEGER NOT NULL, coarse INTEGER NOT NULL,
        unavailable INTEGER NOT NULL, conflicted INTEGER NOT NULL DEFAULT 0, reverted INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(task_id,turn,path));
        CREATE TABLE IF NOT EXISTS agent_change_turns(task_id TEXT NOT NULL,turn INTEGER NOT NULL,incomplete INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(task_id,turn));
        CREATE TABLE IF NOT EXISTS agent_shell_change_intervals(workspace TEXT PRIMARY KEY,task_id TEXT NOT NULL,turn INTEGER NOT NULL,token TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS agent_shell_interval_metadata(task_id TEXT NOT NULL,turn INTEGER NOT NULL,workspace TEXT NOT NULL,interval_id TEXT NOT NULL,metadata TEXT NOT NULL,PRIMARY KEY(task_id,workspace,interval_id));")?;
    Ok(conn)
}

// Reject traversal and symlink/junction escapes, including nonexistent destinations.
fn safe_path(root: &Path, raw: &str) -> Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    safe_path_canonical(&root, raw)
}

// The caller has already canonicalized the workspace root. Resolve the leaf or
// nearest existing ancestor on every call; only repeated root resolution is saved.
fn safe_path_canonical(root: &Path, raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    ensure!(
        !candidate
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "path traversal is not allowed"
    );
    ensure!(
        !fs::symlink_metadata(&candidate).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "symlink mutations cannot be tracked safely"
    );
    let mut ancestor = candidate.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().context("path has no existing ancestor")?;
    }
    let resolved = fs::canonicalize(ancestor)?;
    ensure!(
        resolved.starts_with(&root),
        "path is outside the tracked workspace"
    );
    let suffix = candidate.strip_prefix(ancestor)?;
    let final_path = if suffix.as_os_str().is_empty() {
        resolved
    } else {
        resolved.join(suffix)
    };
    ensure!(
        final_path.starts_with(&root),
        "path is outside the tracked workspace"
    );
    Ok(final_path)
}

fn image(path: &Path, budget: &mut usize) -> Result<Image> {
    Ok(cached_image(path, budget, None)?.0)
}

fn cached_image(
    path: &Path,
    budget: &mut usize,
    cached: Option<&(FileStamp, Vec<u8>)>,
) -> Result<(Image, Option<FileStamp>)> {
    let metadata = match fs::metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Image {
                exists: false,
                bytes: None,
                stamp: String::new(),
            }, None));
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(metadata.is_file(), "not a regular file: {}", path.display());
    let stat = FileStamp::new(&metadata);
    let stamp = format!("{}:{:?}", metadata.len(), metadata.modified()?);
    let bytes = if metadata.len() <= FILE_LIMIT && metadata.len() as usize <= *budget {
        let bytes = if let Some((_, bytes)) = cached.filter(|(cached_stat, _)| {
            stat.as_ref() == Some(cached_stat) && cached_stat.reusable()
        }) {
            bytes.clone()
        } else {
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            fs::File::open(path)?.take(metadata.len() + 1).read_to_end(&mut bytes)?;
            #[cfg(test)]
            SNAPSHOT_READS.with(|reads| {
                let (files, total) = reads.get();
                reads.set((files + 1, total + bytes.len() as u64));
            });
            ensure!(bytes.len() as u64 == metadata.len(), "file changed during snapshot: {}", path.display());
            bytes
        };
        *budget = budget.saturating_sub(bytes.len());
        Some(bytes)
    } else {
        None
    };
    let after = fs::metadata(path)?;
    ensure!(after.is_file() && FileStamp::new(&after) == stat, "file changed during snapshot: {}", path.display());
    Ok((Image {
        exists: true,
        bytes,
        stamp,
    }, stat))
}

fn capture(root: &Path, name: &str, args: &Value) -> Result<Snapshot> {
    let mut snapshot = Snapshot::new();
    let mut budget = SNAPSHOT_LIMIT;
    if !crate::project_process::may_change_files(name) {
        let path = safe_path(root, args["path"].as_str().context("missing file path")?)?;
        let relative = path
            .strip_prefix(fs::canonicalize(root)?)?
            .to_string_lossy()
            .replace('\\', "/");
        snapshot.insert(relative, image(&path, &mut budget)?);
    } else {
        let root = fs::canonicalize(root)?;
        let generation = CACHE_GENERATION.load(Ordering::SeqCst);
        let cached = SNAPSHOT_CACHE.lock().unwrap_or_else(|e| e.into_inner()).take();
        let cached = cached.filter(|cached| cached.root == root);
        let mut next = CachedSnapshot { root: root.clone(), files: BTreeMap::new() };
        let mut walker = ignore::WalkBuilder::new(&root);
        // Hidden files are tracked; what git ignores is not. Ignored folders
        // hold experiment output and dependencies by the thousand, and one
        // such project went past the file limit below, so no shell change in
        // it was recorded at all (2026-11-13: 39,588 files, 38,617 of them
        // under two ignored folders).
        walker
            .standard_filters(false)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            .parents(true)
            .follow_links(false)
            .filter_entry(|entry| {
                !matches!(
                    entry.file_name().to_str(),
                    Some(
                        ".git"
                            | ".codex-workspace-mcp"
                            | "node_modules"
                            | "target"
                            | "dist"
                            | "build"
                            | ".venv"
                            | "__pycache__"
                    )
                )
            });
        for entry in walker.build() {
            let entry = entry?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            ensure!(
                snapshot.len() < 20_000,
                "workspace exceeds change-tracking file limit"
            );
            let path = safe_path_canonical(&root, &entry.path().to_string_lossy())?;
            let relative = path
                .strip_prefix(&root)?
                .to_string_lossy()
                .replace('\\', "/");
            let prior = cached.as_ref().and_then(|cached| cached.files.get(&relative));
            let (image, stat) = cached_image(&path, &mut budget, prior)?;
            ensure!(safe_path_canonical(&root, &relative)? == path, "file location changed during snapshot");
            if let (Some(stat), Some(bytes)) = (stat, image.bytes.as_ref()) {
                next.files.insert(relative.clone(), (stat, bytes.clone()));
            }
            snapshot.insert(relative, image);
        }
        let mut cache = SNAPSHOT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if CACHE_GENERATION.load(Ordering::SeqCst) == generation {
            *cache = Some(next);
        }
    }
    Ok(snapshot)
}

fn same(a: &Image, b: &Image) -> bool {
    a.exists == b.exists
        && match (&a.bytes, &b.bytes) {
            (Some(a), Some(b)) => a == b,
            _ => a.stamp == b.stamp,
        }
}

fn text(bytes: &Option<Vec<u8>>) -> Option<&str> {
    let bytes = bytes.as_deref().unwrap_or(&[]);
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// Cell budget for the exact LCS table. Larger residuals use Myers' greedy
/// search, which costs O((n+m)·D) for D changed lines instead of n·m.
const DIFF_TABLE_CELLS: usize = 4_000_000;
/// Work budget for the greedy search, counted in diagonal steps and matched lines.
const DIFF_SEARCH_STEPS: usize = 16_000_000;
/// The greedy search keeps one entry per diagonal. Files below the 2 MiB snapshot
/// limit fit in this many residual lines; beyond it the estimate is used instead.
const DIFF_SEARCH_LINES: usize = 200_000;

fn counts(before: &str, after: &str) -> (usize, usize, bool) {
    counts_within(before, after, DIFF_TABLE_CELLS, DIFF_SEARCH_STEPS)
}

/// Added/deleted line counts and whether they are exact. A large file with a few
/// edited lines is searched, not tabulated, so it reports its edited lines. Only
/// a residual whose edit search exceeds the budget is `coarse`, and even then the
/// numbers are a bounded estimate of the changed lines, never the whole file.
fn counts_within(
    before: &str,
    after: &str,
    table_cells: usize,
    search_steps: usize,
) -> (usize, usize, bool) {
    let before_lines: Vec<_> = before.split_inclusive('\n').collect();
    let after_lines: Vec<_> = after.split_inclusive('\n').collect();
    let prefix = before_lines.iter().zip(&after_lines).take_while(|(a, b)| a == b).count();
    let mut a = &before_lines[prefix..];
    let mut b = &after_lines[prefix..];
    while !a.is_empty() && !b.is_empty() && a.last() == b.last() {
        a = &a[..a.len() - 1];
        b = &b[..b.len() - 1];
    }
    if a.len().saturating_mul(b.len()) <= table_cells {
        let common = lcs(a, b);
        return (b.len() - common, a.len() - common, false);
    }
    if let Some((added, deleted)) = myers(a, b, search_steps) {
        return (added, deleted, false);
    }
    let (added, deleted) = estimate(a, b);
    (added, deleted, true)
}

fn lcs(a: &[&str], b: &[&str]) -> usize {
    let mut row = vec![0usize; b.len() + 1];
    for line in a {
        let mut diagonal = 0;
        for (j, other) in b.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = if line == other {
                diagonal + 1
            } else {
                row[j].max(old)
            };
            diagonal = old;
        }
    }
    row[b.len()]
}

/// Myers' greedy forward search. Only the minimal edit distance is needed: the
/// distance and both lengths give the added/deleted split, so a big file with a
/// few changed lines stays exact without an n·m table.
fn myers(a: &[&str], b: &[&str], budget: usize) -> Option<(usize, usize)> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let span = (n + m) as usize;
    if span > DIFF_SEARCH_LINES {
        return None;
    }
    let offset = span as isize;
    let mut furthest = vec![0isize; 2 * span + 1];
    let mut steps = 0usize;
    for distance in 0..=span as isize {
        let mut diagonal = -distance;
        while diagonal <= distance {
            let index = (diagonal + offset) as usize;
            let mut x = if diagonal == -distance
                || (diagonal != distance && furthest[index - 1] < furthest[index + 1])
            {
                furthest[index + 1]
            } else {
                furthest[index - 1] + 1
            };
            let mut y = x - diagonal;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
                steps += 1;
            }
            furthest[index] = x;
            steps += 1;
            if steps > budget {
                return None;
            }
            if x >= n && y >= m {
                let delta = m - n;
                return Some((((distance + delta) / 2) as usize, ((distance - delta) / 2) as usize));
            }
            diagonal += 2;
        }
    }
    None
}

/// Bounded estimate for the rare search that is out of budget: a line whose
/// multiplicity changed counts once, so a mostly intact file never looks rewritten.
fn estimate(a: &[&str], b: &[&str]) -> (usize, usize) {
    let mut balance: std::collections::HashMap<&str, isize> = std::collections::HashMap::new();
    for line in a {
        *balance.entry(line).or_insert(0) -= 1;
    }
    for line in b {
        *balance.entry(line).or_insert(0) += 1;
    }
    let (mut added, mut deleted) = (0usize, 0usize);
    for delta in balance.into_values() {
        if delta > 0 {
            added += delta as usize;
        } else {
            deleted += delta.unsigned_abs();
        }
    }
    (added, deleted)
}

fn record(
    root: &Path,
    task: &str,
    turn: usize,
    before: Snapshot,
    after: Snapshot,
) -> Result<Value> {
    let mut conn = connection(root)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let missing = Image {
        exists: false,
        bytes: None,
        stamp: String::new(),
    };
    let keys: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
    for path in keys {
        let old = before.get(path).unwrap_or(&missing);
        let new = after.get(path).unwrap_or(&missing);
        if same(old, new) {
            continue;
        }
        let prior = tx.query_row("SELECT before_exists,before_bytes,after_exists,after_bytes,conflicted FROM agent_file_changes WHERE task_id=?1 AND turn=?2 AND path=?3",
            params![task,turn,path], |row| Ok((row.get::<_,bool>(0)?, row.get::<_,Option<Vec<u8>>>(1)?, row.get::<_,bool>(2)?,row.get::<_,Option<Vec<u8>>>(3)?,row.get::<_,bool>(4)?))).optional()?;
        let (original_exists, original_bytes, conflict) = match prior {
            Some((exists, bytes, last_exists, last_bytes, conflict)) => (
                exists,
                bytes,
                conflict || last_exists != old.exists || last_bytes != old.bytes,
            ),
            None => (old.exists, old.bytes.clone(), false),
        };
        if original_exists == new.exists
            && original_bytes == new.bytes
            && (!new.exists || new.bytes.is_some())
        {
            tx.execute(
                "DELETE FROM agent_file_changes WHERE task_id=?1 AND turn=?2 AND path=?3",
                params![task, turn, path],
            )?;
            continue;
        }
        let unavailable =
            (original_exists && original_bytes.is_none()) || (new.exists && new.bytes.is_none());
        let (added, deleted, binary, coarse) = match (text(&original_bytes), text(&new.bytes)) {
            (Some(a), Some(b)) if !unavailable => {
                let (added, deleted, coarse) = counts(a, b);
                (added, deleted, false, coarse)
            }
            _ => (0, 0, true, false),
        };
        tx.execute("INSERT INTO agent_file_changes(task_id,turn,path,before_exists,before_bytes,after_exists,after_bytes,added,deleted,binary,coarse,unavailable,conflicted,reverted)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0)
            ON CONFLICT(task_id,turn,path) DO UPDATE SET after_exists=excluded.after_exists,after_bytes=excluded.after_bytes,
            added=excluded.added,deleted=excluded.deleted,binary=excluded.binary,coarse=excluded.coarse,
            unavailable=excluded.unavailable,conflicted=excluded.conflicted,reverted=0",
            params![task,turn,path,original_exists,original_bytes,new.exists,new.bytes,added,deleted,binary,coarse,unavailable,conflict])?;
    }
    tx.commit()?;
    summary(root, task, turn)
}

fn mark_incomplete(root: &Path, task: &str, turn: usize) -> Result<()> {
    connection(root)?.execute("INSERT INTO agent_change_turns(task_id,turn,incomplete) VALUES (?1,?2,1) ON CONFLICT(task_id,turn) DO UPDATE SET incomplete=1", params![task, turn])?;
    Ok(())
}

fn snapshot_is_incomplete(snapshot: &Snapshot) -> bool {
    snapshot.values().any(|image| image.exists && image.bytes.is_none())
}

fn abandon_shell_begin(root: &Path, workspace: &Path, task: &str, turn: usize, token: &str) -> Result<()> {
    let mut conn = connection(root)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let matching: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_shell_change_intervals WHERE workspace=?1 AND task_id=?2 AND turn=?3 AND token=?4)",
        params![workspace.to_string_lossy(), task, turn, token], |row| row.get(0))?;
    if !matching { return Ok(()); }
    let incomplete = tx.execute("INSERT INTO agent_change_turns(task_id,turn,incomplete) VALUES (?1,?2,1) ON CONFLICT(task_id,turn) DO UPDATE SET incomplete=1", params![task, turn]);
    // Still remove our marker if the incomplete flag cannot be written. No
    // command has started, and retaining it would permanently strand the root.
    tx.execute("DELETE FROM agent_shell_change_intervals WHERE workspace=?1 AND task_id=?2 AND turn=?3 AND token=?4",
        params![workspace.to_string_lossy(), task, turn, token])?;
    tx.commit()?;
    incomplete.context("could not mark failed shell begin incomplete")?;
    Ok(())
}

fn shell_begin_error(error: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => anyhow::anyhow!("{error:#}; shell begin cleanup failed: {cleanup:#}"),
    }
}

/// Stat every file before every command. The runner serializes shell commands
/// and owns the mutation guard; this function must not acquire that guard again.
pub(crate) async fn shell_begin(
    state: &AgentServiceState,
    workspace: &Workspace,
    task: &str,
    turn: usize,
) -> Result<ShellSnapshot> {
    let state = state.clone();
    let files = workspace.root().to_path_buf();
    let task = task.to_owned();
    let before = tokio::task::spawn_blocking(move || -> Result<ShellSnapshot> {
        let store = fs::canonicalize(state.workspace.root())?;
        let workspace = fs::canonicalize(files)?;
        let token = crate::agent_service::uuid_like();
        let conn = connection(&store)?;
        // Reject invalid task IDs before leaving a durable running marker.
        summary(&store, &task, turn)?;
        let inserted = conn.execute("INSERT OR IGNORE INTO agent_shell_change_intervals(workspace,task_id,turn,token) VALUES (?1,?2,?3,?4)",
            params![workspace.to_string_lossy(), task, turn, token])?;
        ensure!(inserted == 1, "SHELL_BUSY: workspace already has a live shell interval");
        let captured = (|| -> Result<_> {
        let mut errors = Vec::new();
        let before = match capture(&workspace, "run_program", &json!({})) {
            Ok(snapshot) => Some(snapshot),
            Err(error) => { errors.push(format!("before snapshot: {error:#}")); None }
        };
        let managed = if let Some(snapshot) = &before {
            match state.shell_matching_baselines(&task, &workspace, |relative, base| {
                let Ok(path) = safe_path(&workspace, relative) else { return false };
                match snapshot.get(relative) {
                    Some(image) => image.exists == base.is_some()
                        && image.bytes.as_deref() == base
                        && (!image.exists || image.bytes.is_some()),
                    None => base.is_none() && fs::symlink_metadata(path)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                }
            }) {
                Ok(files) => files,
                Err(error) => { errors.push(format!("managed baseline: {error:#}")); Vec::new() }
            }
        } else { Vec::new() };
        if !errors.is_empty() || before.as_ref().is_some_and(snapshot_is_incomplete) {
            mark_incomplete(&store, &task, turn)?;
        }
        Ok((before, managed, errors))
        })();
        let (before, managed, errors) = match captured {
            Ok(captured) => captured,
            Err(error) => return Err(shell_begin_error(error, abandon_shell_begin(&store, &workspace, &task, turn, &token))),
        };
        Ok(ShellSnapshot { store, workspace, task, turn, token, before, managed, errors })
    }).await??;
    let root = before.store.clone();
    let task = before.task.clone();
    let published = async {
        let value = tokio::task::spawn_blocking(move || summary(&root, &task, turn)).await??;
        crate::agent_service::emit(&before.store, &before.task, "workspace/changes", value).await
    }.await;
    if let Err(error) = published {
        let (store, workspace, task, token) = (before.store.clone(), before.workspace.clone(), before.task.clone(), before.token.clone());
        let cleanup = tokio::task::spawn_blocking(move || abandon_shell_begin(&store, &workspace, &task, turn, &token))
            .await.map_err(anyhow::Error::from).and_then(|result| result);
        return Err(shell_begin_error(error, cleanup));
    }
    Ok(before)
}

/// Call only after process exit, never merely because a session was returned.
/// The begin marker keeps `complete` and undo false for the entire live session.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn shell_finish(
    state: &AgentServiceState,
    workspace: &Workspace,
    task: &str,
    turn: usize,
    before: ShellSnapshot,
    interval_id: &str,
    owners: &[String],
) -> Result<Value> {
    ensure!(before.task == task && before.turn == turn, "shell snapshot belongs to another turn");
    let attribution = ShellAttribution::new(interval_id, owners)?;
    let state = state.clone();
    let files = workspace.root().to_path_buf();
    let root = before.store.clone();
    let task = before.task.clone();
    let (value, errors) = tokio::task::spawn_blocking(move || -> Result<(Value, Vec<String>)> {
        ensure!(fs::canonicalize(state.workspace.root())? == before.store
            && fs::canonicalize(files)? == before.workspace, "shell snapshot belongs to another workspace");
        let conn = connection(&before.store)?;
        let active: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_shell_change_intervals WHERE workspace=?1 AND task_id=?2 AND turn=?3 AND token=?4)",
            params![before.workspace.to_string_lossy(), before.task, turn, before.token], |row| row.get(0))?;
        ensure!(active, "shell interval is no longer active");
        let mut errors = before.errors;
        let after = match capture(&before.workspace, "run_program", &json!({})) {
            Ok(snapshot) => Some(snapshot),
            Err(error) => { errors.push(format!("after snapshot: {error:#}")); None }
        };
        if after.as_ref().is_some_and(snapshot_is_incomplete) {
            mark_incomplete(&before.store, &before.task, turn)?;
        }
        if let (Some(old), Some(new)) = (before.before, after) {
            // Baselines are adopted from the very same images that are recorded.
            // Missing entries are absence only after a fresh safe-path/stat check.
            for relative in before.managed {
                let adopt = (|| -> Result<()> {
                    let path = safe_path(&before.workspace, &relative)?;
                    let after = if let Some(image) = new.get(&relative) {
                        let metadata = fs::metadata(&path)?;
                        ensure!(metadata.is_file()
                            && image.stamp == format!("{}:{:?}", metadata.len(), metadata.modified()?),
                            "file changed before baseline takeover: {relative}");
                        Some(image.bytes.as_deref().context("shell after-image bytes unavailable")?)
                    } else {
                        ensure!(fs::symlink_metadata(&path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                            "file absent from shell snapshot still exists: {relative}");
                        None
                    };
                    let original = old.get(&relative).and_then(|image| image.bytes.as_deref());
                    state.shell_adopt_baseline(&before.task, &before.workspace, turn, &relative, original, after, &attribution)
                })();
                if let Err(error) = adopt { errors.push(format!("managed baseline {relative}: {error:#}")); }
            }
            if let Err(error) = record(&before.store, &before.task, turn, old, new) {
                errors.push(format!("record shell changes: {error:#}"));
            }
        }
        if !errors.is_empty() { mark_incomplete(&before.store, &before.task, turn)?; }
        conn.execute("INSERT INTO agent_shell_interval_metadata(task_id,turn,workspace,interval_id,metadata) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(task_id,workspace,interval_id) DO UPDATE SET metadata=excluded.metadata",
            params![before.task, turn, before.workspace.to_string_lossy(), attribution.interval_id, attribution.metadata().to_string()])?;
        conn.execute("DELETE FROM agent_shell_change_intervals WHERE workspace=?1 AND token=?2",
            params![before.workspace.to_string_lossy(), before.token])?;
        Ok((summary(&before.store, &before.task, turn)?, errors))
    }).await??;
    crate::agent_service::emit(&root, &task, "workspace/changes", value.clone()).await?;
    if !errors.is_empty() {
        crate::agent_service::emit(&root, &task, "workspace/changes_error",
            json!({"turn":turn,"message":errors.join("; ")})).await?;
    }
    Ok(value)
}

pub fn summary(root: &Path, task: &str, turn: usize) -> Result<Value> {
    let conn = connection(root)?;
    ensure!(
        conn.query_row("SELECT 1 FROM agent_tasks WHERE id=?1", [task], |_| Ok(()))
            .optional()?
            .is_some(),
        "task not found"
    );
    let mut stmt = conn.prepare("SELECT path,added,deleted,binary,coarse,unavailable,conflicted,reverted,before_exists,after_exists FROM agent_file_changes WHERE task_id=?1 AND turn=?2 ORDER BY path")?;
    let files = stmt.query_map(params![task,turn], |row| Ok(json!({"path":row.get::<_,String>(0)?,"added":row.get::<_,usize>(1)?,"deleted":row.get::<_,usize>(2)?,
        "binary":row.get::<_,bool>(3)?,"coarse":row.get::<_,bool>(4)?,"unavailable":row.get::<_,bool>(5)?,"conflicted":row.get::<_,bool>(6)?,"reverted":row.get::<_,bool>(7)?,
        "created":!row.get::<_,bool>(8)?,"removed":!row.get::<_,bool>(9)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let reverted = !files.is_empty() && files.iter().all(|file| file["reverted"] == true);
    let incomplete = conn
        .query_row(
            "SELECT incomplete FROM agent_change_turns WHERE task_id=?1 AND turn=?2",
            params![task, turn],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false);
    let running: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_shell_change_intervals WHERE task_id=?1 AND turn=?2)",
        params![task, turn], |row| row.get(0))?;
    let intervals = conn.prepare("SELECT metadata FROM agent_shell_interval_metadata WHERE task_id=?1 AND turn=?2 ORDER BY interval_id")?
        .query_map(params![task, turn], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter().map(|metadata| serde_json::from_str::<Value>(&metadata)).collect::<serde_json::Result<Vec<_>>>()?;
    let undo_available = !incomplete && !running
        && !files.is_empty()
        && !reverted
        && files
            .iter()
            .all(|file| file["unavailable"] == false && file["conflicted"] == false);
    Ok(
        json!({"turn":turn,"total":files.len(),"added":files.iter().map(|file|file["added"].as_u64().unwrap_or(0)).sum::<u64>(),
        "deleted":files.iter().map(|file|file["deleted"].as_u64().unwrap_or(0)).sum::<u64>(),"files":files,"reverted":reverted,"undo_available":undo_available,"complete":!incomplete && !running,"shell_intervals":intervals}),
    )
}

pub fn is_mutation(name: &str) -> bool {
    matches!(name, "write_file" | "replace_range" | "edit_file")
        || crate::project_process::may_change_files(name)
}

/// Run a mutating tool in `workspace` (the task's project) and record its
/// before/after images. The change log itself lives in the server's store.
pub async fn execute_tracked(
    state: AgentServiceState,
    workspace: std::sync::Arc<crate::tools::Workspace>,
    task: String,
    turn: usize,
    name: String,
    args: Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let root = state.workspace.root().to_path_buf();
    let files = workspace.root().to_path_buf();
    if !crate::project_process::may_change_files(&name) {
        // Boundary checks are mandatory even if optional snapshot capture fails.
        safe_path(&files, args["path"].as_str().context("missing file path")?)?;
        invalidate_snapshot_cache(&files);
    }
    let capture_root = files.clone();
    let capture_name = name.clone();
    let capture_args = args.clone();
    let before =
        tokio::task::spawn_blocking(move || capture(&capture_root, &capture_name, &capture_args))
            .await?;
    let boundary = crate::process_sandbox::ExecutionBoundary::for_mode(state.permission_mode);
    let result = if crate::project_process::is_tool(&name) {
        crate::project_process::execute_with_boundary(&workspace, &name, args.clone(), &cancel, boundary).await
    } else if crate::program_execution::is_tool(&name) {
        crate::program_execution::execute_with_boundary(&workspace, &args, &cancel, boundary).await
    } else {
        // A started blocking file commit cannot be aborted. Await completion
        // before recording its after-image or releasing the mutation lock.
        if cancel.is_cancelled() {
            Err(anyhow::anyhow!("Tool execution cancelled"))
        } else {
            crate::agent_service::execute_tool(
                workspace.clone(),
                state.tool_catalog.as_deref(),
                &name,
                args.clone(),
            )
            .await
        }
    };
    if !crate::project_process::may_change_files(&name) {
        invalidate_snapshot_cache(&files);
    }
    let background_running = result.as_ref().is_ok_and(|value| value["running"] == true);
    let saved_root = root.clone();
    let saved_task = task.clone();
    let changes = tokio::task::spawn_blocking(move || {
        let before = before?;
        let after = capture(&files,&name,&args)?;
        record(&saved_root,&saved_task,turn,before,after)?;
        if background_running {
            // A running script can change files after this snapshot. Do not
            // advertise an atomic undo for an incomplete execution interval.
            connection(&saved_root)?.execute("INSERT INTO agent_change_turns(task_id,turn,incomplete) VALUES (?1,?2,1) ON CONFLICT(task_id,turn) DO UPDATE SET incomplete=1",params![saved_task,turn])?;
        }
        summary(&saved_root,&saved_task,turn)
    }).await?;
    match changes {
        Ok(summary) => {
            crate::agent_service::emit(&root, &task, "workspace/changes", summary).await?;
        }
        Err(error) => {
            let saved_root = root.clone();
            let saved_task = task.clone();
            let incomplete=tokio::task::spawn_blocking(move || -> Result<Value> {
                let conn=connection(&saved_root)?;
                conn.execute("INSERT INTO agent_change_turns(task_id,turn,incomplete) VALUES (?1,?2,1) ON CONFLICT(task_id,turn) DO UPDATE SET incomplete=1",params![saved_task,turn])?;
                summary(&saved_root,&saved_task,turn)
            }).await?;
            if let Ok(summary) = incomplete {
                crate::agent_service::emit(&root, &task, "workspace/changes", summary).await?;
            }
            crate::agent_service::emit(
                &root,
                &task,
                "workspace/changes_error",
                json!({"turn":turn,"message":error.to_string()}),
            )
            .await?;
        }
    }
    result
}

pub async fn get_summary(
    State(state): State<AgentServiceState>,
    RoutePath((task, turn)): RoutePath<(String, usize)>,
) -> Response {
    let root = state.workspace.root().to_path_buf();
    response(tokio::task::spawn_blocking(move || summary(&root, &task, turn)).await)
}

#[derive(serde::Deserialize)]
pub struct DiffQuery {
    path: Option<String>,
}

pub async fn get_diff(
    State(state): State<AgentServiceState>,
    RoutePath((task, turn, index)): RoutePath<(String, usize, usize)>,
    Query(query): Query<DiffQuery>,
) -> Response {
    let root = state.workspace.root().to_path_buf();
    response(tokio::task::spawn_blocking(move || {
        let conn=connection(&root)?;
        let offset=if query.path.is_some() {0} else {index};
        let data=conn.query_row("SELECT path,before_bytes,after_bytes,binary,unavailable FROM agent_file_changes WHERE task_id=?1 AND turn=?2 AND (?3 IS NULL OR path=?3) ORDER BY path LIMIT 1 OFFSET ?4",
            params![task,turn,query.path,offset], |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<Vec<u8>>>(1)?,row.get::<_,Option<Vec<u8>>>(2)?,row.get::<_,bool>(3)?,row.get::<_,bool>(4)?))).optional()?.context("change not found")?;
        let (path,before,after,binary,unavailable)=data;
        Ok(json!({"path":path,"oldText":if binary || unavailable || before.is_none() {None} else {text(&before)},
            "newText":if binary || unavailable {None} else {text(&after)},"binary":binary,"unavailable":unavailable}))
    }).await)
}

fn restore(path: &Path, bytes: &Option<Vec<u8>>) -> Result<()> {
    if let Some(bytes) = bytes {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let name = path
            .file_name()
            .context("missing filename")?
            .to_string_lossy();
        let tmp = path.with_file_name(format!(
            ".{name}.agent-undo-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut created = false;
        let write = (|| -> Result<()> {
            let permissions = fs::metadata(path)
                .ok()
                .map(|metadata| metadata.permissions());
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            created = true;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            if let Some(permissions) = permissions {
                fs::set_permissions(&tmp, permissions)?;
            }
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if write.is_err() && created {
            let _ = fs::remove_file(&tmp);
        }
        write
    } else {
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

pub(crate) fn undo(root: &Path, task: &str, turn: usize) -> Result<Value> {
    // Paths are relative to the task's project; the log is in the server store.
    let files = crate::session_browser::task_workspace(root, task)?;
    let (files, lock) = workspace_mutations(&files)?;
    // Also protects synchronous rewind callers. Fail fast rather than blocking
    // an async worker (or waiting for a long-running shell session).
    let _guard = lock.try_write_owned().map_err(|_| anyhow::anyhow!("conflict: workspace files are being mutated"))?;
    undo_locked(root, task, turn, &files)
}

fn undo_locked(root: &Path, task: &str, turn: usize, files: &Path) -> Result<Value> {
    let conn = connection(root)?;
    let canonical = fs::canonicalize(&files)?;
    let shell_running: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_shell_change_intervals WHERE workspace=?1)",
        [canonical.to_string_lossy()], |row| row.get(0))?;
    ensure!(!shell_running, "conflict: wait for the running shell to exit before undoing files");
    let active: usize = conn.query_row(
        "SELECT COUNT(*) FROM agent_tasks WHERE status IN ('running','cancelling')",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        active == 0,
        "conflict: wait for running tasks to stop before undoing files"
    );
    let current = summary(root, task, turn)?;
    if current["reverted"] == true {
        return Ok(current);
    }
    ensure!(
        current["undo_available"] == true,
        "conflict: no complete, uncontested snapshot is available for this turn"
    );
    let mut stmt=conn.prepare("SELECT path,before_bytes,after_exists,after_bytes FROM agent_file_changes WHERE task_id=?1 AND turn=?2 AND reverted=0 ORDER BY path")?;
    let rows = stmt
        .query_map(params![task, turn], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<Vec<u8>>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut prepared = Vec::new();
    let mut conflicts = Vec::new();
    for (relative, before, exists, after) in rows {
        let path = safe_path(&files, &relative)?;
        let mut budget = usize::MAX;
        let actual = image(&path, &mut budget)?;
        if actual.exists != exists || actual.bytes != after {
            conflicts.push(relative.clone());
        }
        prepared.push((relative, path, before, after));
    }
    if !conflicts.is_empty() {
        bail!(
            "conflict: files changed after this turn: {}",
            conflicts.join(", ")
        );
    }
    invalidate_snapshot_cache(&files);
    let mut applied = Vec::new();
    for (index, (relative, path, before, after)) in prepared.iter().enumerate() {
        let mut restored = false;
        let outcome = (|| -> Result<()> {
            ensure!(
                safe_path(&files, relative)? == *path,
                "conflict: {relative} changed its filesystem location"
            );
            let mut budget = usize::MAX;
            let actual = image(path, &mut budget)?;
            ensure!(
                actual.bytes == *after && actual.exists == after.is_some(),
                "conflict: {relative} changed during undo"
            );
            restore(path, before)?;
            restored = true;
            conn.execute(
                "UPDATE agent_file_changes SET reverted=1 WHERE task_id=?1 AND turn=?2 AND path=?3",
                params![task, turn, relative],
            )?;
            Ok(())
        })();
        if let Err(error) = outcome {
            let mut rollback_errors = Vec::new();
            if restored {
                applied.push(index);
            }
            for prior in applied.iter().rev().copied() {
                let (relative, path, before, after): &(
                    String,
                    PathBuf,
                    Option<Vec<u8>>,
                    Option<Vec<u8>>,
                ) = &prepared[prior];
                let rollback = (|| -> Result<()> {
                    ensure!(
                        safe_path(&files, relative)? == *path,
                        "file location changed during rollback"
                    );
                    let mut budget = usize::MAX;
                    let actual = image(path, &mut budget)?;
                    ensure!(
                        actual.exists == before.is_some() && actual.bytes == *before,
                        "file changed externally during rollback"
                    );
                    restore(path, after)
                })();
                if let Err(error) = rollback {
                    rollback_errors.push(format!("{relative}: {error}"));
                } else {
                    conn.execute("UPDATE agent_file_changes SET reverted=0 WHERE task_id=?1 AND turn=?2 AND path=?3",params![task,turn,relative])?;
                }
            }
            if !rollback_errors.is_empty() {
                conn.execute("INSERT INTO agent_change_turns(task_id,turn,incomplete) VALUES (?1,?2,1) ON CONFLICT(task_id,turn) DO UPDATE SET incomplete=1",params![task,turn])?;
            }
            bail!(
                "undo failed: {error}; rollback failures: {}",
                rollback_errors.join("; ")
            );
        }
        applied.push(index);
    }
    summary(root, task, turn)
}

pub async fn undo_turn(
    State(state): State<AgentServiceState>,
    RoutePath((task, turn)): RoutePath<(String, usize)>,
) -> Response {
    let root = state.workspace.root().to_path_buf();
    let saved_root = root.clone();
    let saved_task = task.clone();
    let result = tokio::task::spawn_blocking(move || undo(&saved_root, &saved_task, turn)).await;
    if let Ok(Ok(ref summary)) = result {
        let _ =
            crate::agent_service::emit(&root, &task, "workspace/changes", summary.clone()).await;
    }
    response(result)
}

fn response(result: std::result::Result<Result<Value>, tokio::task::JoinError>) -> Response {
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => {
            let message = error.to_string();
            let status = if message.starts_with("conflict:") {
                StatusCode::CONFLICT
            } else if message.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(json!({"error":message}))).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_state() -> Result<(PathBuf, AgentServiceState)> {
        let root = std::env::temp_dir().join(format!("shell-changes-{}", crate::agent_service::uuid_like()));
        fs::create_dir_all(&root)?;
        let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
        connection(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','running',0,0)", [])?;
        Ok((root, state))
    }

    #[test]
    fn canonical_safe_paths_preserve_missing_paths_and_boundaries() -> Result<()> {
        let base = std::env::temp_dir().join(format!("canonical-path-{}", crate::agent_service::uuid_like()));
        let workspace = base.join("workspace");
        let outside = base.join("outside");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&outside)?;
        fs::write(workspace.join("file.txt"), "inside")?;
        let root = fs::canonicalize(&workspace)?;
        for raw in ["file.txt", "missing/deep/file.txt"] {
            assert_eq!(safe_path(&workspace, raw)?, safe_path_canonical(&root, raw)?);
        }
        assert_eq!(safe_path_canonical(&root, "missing/deep/file.txt")?, root.join("missing/deep/file.txt"));
        assert!(safe_path_canonical(&root, "../outside/file.txt").is_err());
        assert!(safe_path_canonical(&root, &outside.join("file.txt").to_string_lossy()).is_err());
        let _ = fs::remove_dir_all(base);
        Ok(())
    }

    #[test]
    fn canonical_safe_paths_reject_leaf_links_and_ancestor_escapes() -> Result<()> {
        let base = std::env::temp_dir().join(format!("canonical-links-{}", crate::agent_service::uuid_like()));
        let workspace = base.join("workspace");
        let outside = base.join("outside");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&outside)?;
        let target = outside.join("file.txt");
        fs::write(&target, "outside")?;
        let leaf = workspace.join("leaf.txt");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&target, &leaf);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &leaf);
        if let Err(error) = linked {
            if error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) {
                println!("SYMLINK_CHECK_SKIPPED: {error}");
                let _ = fs::remove_dir_all(base);
                return Ok(());
            }
            return Err(error.into());
        }
        let escape = workspace.join("escape");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &escape)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &escape)?;
        let root = fs::canonicalize(&workspace)?;
        assert!(safe_path_canonical(&root, "leaf.txt").is_err());
        assert!(safe_path_canonical(&root, "escape/file.txt").is_err());
        assert!(safe_path_canonical(&root, "escape/missing/file.txt").is_err());
        fs::remove_file(leaf)?;
        #[cfg(windows)]
        fs::remove_dir(escape)?;
        #[cfg(unix)]
        fs::remove_file(escape)?;
        let _ = fs::remove_dir_all(base);
        Ok(())
    }

    #[tokio::test]
    async fn shell_interval_stays_incomplete_until_exit_and_adopts_only_matching_baselines() -> Result<()> {
        let (root, state) = shell_state()?;
        let workspace = state.workspace.as_ref();
        fs::write(root.join("own.txt"), "before\n")?;
        fs::write(root.join("external.txt"), "outside\n")?;
        fs::write(root.join("removed.txt"), "remove\n")?;
        state.shell_matching_baselines("t", workspace.root(), |_, _| false)?;
        let conn = connection(&root)?;
        for (path, bytes) in [("own.txt", b"before\n".as_slice()), ("external.txt", b"baseline\n".as_slice()), ("removed.txt", b"remove\n".as_slice())] {
            conn.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,?2,1,?3)",
                params![workspace.root().to_string_lossy(), path, bytes])?;
        }
        let before = shell_begin(&state, workspace, "t", 1).await?;
        assert_eq!(summary(&root, "t", 1)?["complete"], false);
        fs::write(root.join("own.txt"), "intermediate\n")?;
        // Returning a live session does not finish the interval.
        assert_eq!(summary(&root, "t", 1)?["undo_available"], false);
        fs::write(root.join("own.txt"), "after\n")?;
        fs::write(root.join("external.txt"), "outside plus shell\n")?;
        fs::write(root.join("created.txt"), "created\n")?;
        fs::remove_file(root.join("removed.txt"))?;
        let value = shell_finish(&state, workspace, "t", 1, before, "test-1", &["@main".into()]).await?;
        assert_eq!(value["complete"], true);
        assert_eq!(value["total"], 4);
        assert_eq!(value["undo_available"], true);
        let own: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path='own.txt'", [], |row| row.get(0))?;
        let external: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path='external.txt'", [], |row| row.get(0))?;
        assert_eq!(own, b"after\n");
        assert_eq!(external, b"baseline\n");
        let writers: usize = conn.query_row("SELECT COUNT(*) FROM agent_file_writer_versions WHERE task_id='t' AND owner='@main'", [], |row| row.get(0))?;
        assert_eq!(writers, 2);
        // Revalidate after-images before the next command: an intervening
        // external write must be its before-image, not the previous after-image.
        fs::write(root.join("own.txt"), "outside again\n")?;
        let before = shell_begin(&state, workspace, "t", 2).await?;
        fs::write(root.join("own.txt"), "next command\n")?;
        shell_finish(&state, workspace, "t", 2, before, "test-2", &["@main".into()]).await?;
        let original: Vec<u8> = conn.query_row("SELECT before_bytes FROM agent_file_changes WHERE task_id='t' AND turn=2 AND path='own.txt'", [], |row| row.get(0))?;
        assert_eq!(original, b"outside again\n");
        let base: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path='own.txt'", [], |row| row.get(0))?;
        assert_eq!(base, b"after\n");
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn failed_shell_begin_releases_its_marker_and_can_be_retried() -> Result<()> {
        for stage in ["emit", "incomplete"] {
            let (root, state) = shell_state()?;
            let conn = connection(&root)?;
            if stage == "emit" {
                conn.execute_batch("CREATE TRIGGER reject_shell_begin BEFORE INSERT ON agent_task_events WHEN NEW.kind='workspace/changes' BEGIN SELECT RAISE(ABORT,'injected emit failure'); END;")?;
            } else {
                fs::File::create(root.join("large.bin"))?.set_len(FILE_LIMIT + 1)?;
                conn.execute_batch("CREATE TRIGGER reject_shell_begin BEFORE INSERT ON agent_change_turns BEGIN SELECT RAISE(ABORT,'injected incomplete failure'); END;")?;
            }
            let result = shell_begin(&state, &state.workspace, "t", 1).await;
            assert!(result.is_err(), "injected {stage} failure did not fire");
            let active: usize = conn.query_row("SELECT COUNT(*) FROM agent_shell_change_intervals", [], |row| row.get(0))?;
            assert_eq!(active, 0, "failed {stage} begin stranded a running marker");
            if stage == "emit" { assert_eq!(summary(&root, "t", 1)?["complete"], false); }
            conn.execute_batch("DROP TRIGGER reject_shell_begin;")?;
            let before = shell_begin(&state, &state.workspace, "t", 1).await?;
            shell_finish(&state, &state.workspace, "t", 1, before, "retry", &["@main".into()]).await?;
            drop(conn);
            drop(state);
            let _ = fs::remove_dir_all(root);
        }
        Ok(())
    }

    #[test]
    fn failed_shell_begin_cleanup_cannot_remove_a_different_token() -> Result<()> {
        let (root, state) = shell_state()?;
        let workspace = state.workspace.root();
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_shell_change_intervals(workspace,task_id,turn,token) VALUES (?1,'t',1,'new-token')", [workspace.to_string_lossy()])?;
        abandon_shell_begin(&root, workspace, "t", 1, "old-token")?;
        let active: String = conn.query_row("SELECT token FROM agent_shell_change_intervals", [], |row| row.get(0))?;
        assert_eq!(active, "new-token");
        let flags: usize = conn.query_row("SELECT COUNT(*) FROM agent_change_turns", [], |row| row.get(0))?;
        assert_eq!(flags, 0, "old-token cleanup must not mark the new interval's turn incomplete");
        abandon_shell_begin(&root, workspace, "t", 1, "new-token")?;
        let active: usize = conn.query_row("SELECT COUNT(*) FROM agent_shell_change_intervals", [], |row| row.get(0))?;
        assert_eq!(active, 0);
        assert_eq!(summary(&root, "t", 1)?["complete"], false);
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn shell_interval_metadata_survives_summary_and_preserves_effective_owners() -> Result<()> {
        let (root, state) = shell_state()?;
        fs::write(root.join("owned.txt"), "before")?;
        state.shell_matching_baselines("t", state.workspace.root(), |_, _| false)?;
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,'owned.txt',1,?2)",
            params![state.workspace.root().to_string_lossy(), b"before".as_slice()])?;
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        fs::write(root.join("owned.txt"), "single")?;
        let single = shell_finish(&state, &state.workspace, "t", 1, before, "single", &["a".into(), "a".into()]).await?;
        assert_eq!(single["shell_intervals"][0]["writer"], "a");
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        fs::write(root.join("owned.txt"), "shared")?;
        let shared = shell_finish(&state, &state.workspace, "t", 1, before, "shared", &["b".into(), "a".into()]).await?;
        assert_eq!(shared["shell_intervals"].as_array().unwrap().len(), 2);
        let saved = summary(&root, "t", 1)?;
        assert_eq!(shared, saved);
        let metadata = saved["shell_intervals"].as_array().unwrap().iter().find(|interval| interval["interval_id"] == "shared").unwrap();
        assert_eq!(metadata["owners"], json!(["a", "b"]));
        assert_eq!(metadata["writer"], "@shared_shell:shared");
        assert_eq!(metadata["attribution"], "unknown");
        let recorded: String = conn.query_row("SELECT metadata FROM agent_file_writer_metadata WHERE task_id='t' AND owner='@shared_shell:shared'", [], |row| row.get(0))?;
        assert_eq!(&serde_json::from_str::<Value>(&recorded)?, metadata);
        assert_eq!(ShellAttribution::new("serial", &["@main".into(), "@main".into()])?.writer(), "@main");
        assert_eq!(ShellAttribution::new("unknown", &[])?.metadata()["attribution"], "unknown");
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn shell_snapshot_limits_remain_explicitly_incomplete() -> Result<()> {
        let (root, state) = shell_state()?;
        fs::File::create(root.join("large.bin"))?.set_len(FILE_LIMIT + 1)?;
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        let value = shell_finish(&state, &state.workspace, "t", 1, before, "limits", &["@main".into()]).await?;
        assert_eq!(value["complete"], false);
        assert_eq!(value["undo_available"], false);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn cached_bytes_require_matching_metadata_and_obey_the_byte_budget() -> Result<()> {
        let root = std::env::temp_dir().join(format!("snapshot-cache-{}", crate::agent_service::uuid_like()));
        fs::create_dir_all(&root)?;
        let path = root.join("file.txt");
        fs::write(&path, b"old")?;
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        fs::File::options().write(true).open(&path)?.set_times(fs::FileTimes::new().set_modified(past))?;
        let (first, stat) = cached_image(&path, &mut 10, None)?;
        let cached = (stat.context("missing metadata")?, first.bytes.context("missing bytes")?);
        let (hit, _) = cached_image(&path, &mut 10, Some(&cached))?;
        assert_eq!(hit.bytes.as_deref(), Some(b"old".as_slice()));
        let (limited, _) = cached_image(&path, &mut 2, Some(&cached))?;
        assert!(limited.bytes.is_none(), "cache hits must still charge the snapshot budget");
        fs::write(&path, b"new")?; // Same length, different modification time.
        let (fresh, _) = cached_image(&path, &mut 10, Some(&cached))?;
        assert_eq!(fresh.bytes.as_deref(), Some(b"new".as_slice()));
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn mutation_guards_isolate_workspaces_and_share_undo_locks() -> Result<()> {
        let (root, state) = shell_state()?;
        let other = root.join("other");
        fs::create_dir_all(&other)?;
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_session_meta(task_id,workspace) VALUES ('t',?1)", [state.workspace.root().to_string_lossy()])?;
        conn.execute("UPDATE agent_tasks SET status='completed' WHERE id='t'", [])?;
        let cancel = CancellationToken::new();
        let broad = mutation_guard(&root, "exec_command", &json!({}), &cancel).await?;
        let independent = tokio::time::timeout(std::time::Duration::from_millis(200),
            mutation_guard(&other, "exec_command", &json!({}), &cancel)).await??;
        drop(independent);
        let independent_file = tokio::time::timeout(std::time::Duration::from_millis(200),
            mutation_guard(&other, "write_file", &json!({"path":"a.txt"}), &cancel)).await??;
        drop(independent_file);
        let alias = root.join(".");
        let (_, alias_lock) = workspace_mutations(&alias)?;
        assert!(alias_lock.try_write().is_err(), "canonical aliases share the shell lock");
        assert!(undo(&root, "t", 1).unwrap_err().to_string().contains("workspace files are being mutated"));
        drop(broad);
        assert_eq!(undo(&root, "t", 1).unwrap_err().to_string(), "conflict: no complete, uncontested snapshot is available for this turn");
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    /// Diagnostic only: does not change the current attribution policy.
    #[tokio::test]
    #[ignore = "read-only audit experiment: prints attribution and cache limitations"]
    async fn shell_snapshot_audit_observations() -> Result<()> {
        let (root, state) = shell_state()?;
        fs::create_dir_all(root.join("node_modules"))?;
        for relative in ["cache.txt", "concurrent.txt", "node_modules/managed.txt"] {
            fs::write(root.join(relative), b"old")?;
        }
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let cache_path = root.join("cache.txt");
        fs::File::options().write(true).open(&cache_path)?.set_times(fs::FileTimes::new().set_modified(old_time))?;
        state.shell_matching_baselines("t", state.workspace.root(), |_, _| false)?;
        let conn = connection(&root)?;
        for relative in ["cache.txt", "concurrent.txt", "node_modules/managed.txt"] {
            conn.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,?2,1,?3)",
                params![state.workspace.root().to_string_lossy(), relative, b"old".as_slice()])?;
        }
        capture(&root, "run_program", &json!({}))?;
        fs::write(&cache_path, b"new")?;
        fs::File::options().write(true).open(&cache_path)?.set_times(fs::FileTimes::new().set_modified(old_time))?;
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        println!("SHELL_AUDIT preserved_mtime actual={:?} captured={:?} matched_managed={}",
            fs::read(&cache_path)?, before.before.as_ref().and_then(|snapshot| snapshot.get("cache.txt")).and_then(|image| image.bytes.as_ref()),
            before.managed.iter().any(|path| path == "cache.txt"));
        // No subprocess is started: these stand in for an external writer's
        // changes during the interval, including a spawn-failure interval.
        fs::write(root.join("concurrent.txt"), b"external during interval")?;
        fs::write(root.join("node_modules/managed.txt"), b"command in excluded directory")?;
        let value = shell_finish(&state, &state.workspace, "t", 1, before, "audit", &["@main".into()]).await?;
        for relative in ["cache.txt", "concurrent.txt", "node_modules/managed.txt"] {
            let baseline: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path=?1", [relative], |row| row.get(0))?;
            println!("SHELL_AUDIT path={relative} actual={:?} baseline={:?}",
                String::from_utf8_lossy(&fs::read(root.join(relative))?), String::from_utf8_lossy(&baseline));
        }
        println!("SHELL_AUDIT summary={value}");
        drop(conn);
        drop(state);
        invalidate_snapshot_cache(&root);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    /// Opt-in benchmark: application cache cold/warm, not an OS disk-cache flush.
    #[test]
    #[ignore = "snapshot performance measurement; creates temporary fixture trees"]
    fn snapshot_performance_measurement() -> Result<()> {
        use std::time::{Duration, Instant, SystemTime};
        for (count, size) in [(1_000usize, 4_096usize), (5_000, 4_096), (10_000, 1_024), (1_000, 65_536)] {
            let root = std::env::temp_dir().join(format!("snapshot-perf-{}", crate::agent_service::uuid_like()));
            fs::create_dir_all(&root)?;
            let old = SystemTime::now() - Duration::from_secs(60);
            let content = vec![b'x'; size];
            for i in 0..count {
                let path = root.join(format!("file-{i:05}.txt"));
                fs::write(&path, &content)?;
                fs::File::options().write(true).open(&path)?.set_times(fs::FileTimes::new().set_modified(old))?;
            }
            for mode in ["cold", "warm", "changed_one", "recent_timestamps"] {
                let mut samples = Vec::new();
                let mut io = (0, 0);
                let mut retained = 0;
                let mut incomplete = false;
                for run in 0..5 {
                    if mode == "cold" { invalidate_snapshot_cache(&root); }
                    if mode == "changed_one" {
                        let mut changed = content.clone();
                        changed[0] = b'a' + run;
                        fs::write(root.join("file-00000.txt"), changed)?;
                    }
                    if mode == "recent_timestamps" {
                        for i in 0..count {
                            fs::File::options().write(true).open(root.join(format!("file-{i:05}.txt")))?
                                .set_times(fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(30)))?;
                        }
                    }
                    SNAPSHOT_READS.with(|reads| reads.set((0, 0)));
                    let start = Instant::now();
                    let snapshot = capture(&root, "run_program", &json!({}))?;
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    let reads = SNAPSHOT_READS.with(|reads| reads.get());
                    io.0 += reads.0;
                    io.1 += reads.1;
                    retained = snapshot.values().filter_map(|image| image.bytes.as_ref()).map(Vec::len).sum::<usize>();
                    incomplete = snapshot_is_incomplete(&snapshot);
                }
                samples.sort_by(f64::total_cmp);
                println!("SNAPSHOT_PERF files={count} file_bytes={size} mode={mode} samples=5 median_ms={:.3} min_ms={:.3} max_ms={:.3} mean_reads={:.1} mean_read_bytes={:.0} retained_bytes={retained} incomplete={incomplete}",
                    samples[2], samples[0], samples[4], io.0 as f64 / 5.0, io.1 as f64 / 5.0);
            }
            invalidate_snapshot_cache(&root);
            let _ = fs::remove_dir_all(root);
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "shell lifecycle performance measurement including managed baseline updates"]
    async fn shell_lifecycle_performance_measurement() -> Result<()> {
        use std::time::{Duration, Instant, SystemTime};
        for mode in ["unmanaged_noop", "managed_noop", "managed_one", "managed_all"] {
            let (root, state) = shell_state()?;
            let content = vec![b'x'; 1_024];
            let old = SystemTime::now() - Duration::from_secs(60);
            for i in 0..1_000 {
                let path = root.join(format!("file-{i:05}.txt"));
                fs::write(&path, &content)?;
                fs::File::options().write(true).open(&path)?.set_times(fs::FileTimes::new().set_modified(old))?;
            }
            if mode != "unmanaged_noop" {
                state.shell_matching_baselines("t", state.workspace.root(), |_, _| false)?;
                let mut conn = connection(&root)?;
                let tx = conn.transaction()?;
                for i in 0..1_000 {
                    tx.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,?2,1,?3)",
                        params![state.workspace.root().to_string_lossy(), format!("file-{i:05}.txt"), &content])?;
                }
                tx.commit()?;
            }
            capture(&root, "run_program", &json!({}))?;
            let mut begins = Vec::new();
            let mut finishes = Vec::new();
            for run in 0..3 {
                let start = Instant::now();
                let before = shell_begin(&state, &state.workspace, "t", run + 1).await?;
                begins.push(start.elapsed().as_secs_f64() * 1000.0);
                let changed = if mode == "managed_all" { 1_000 } else if mode == "managed_one" { 1 } else { 0 };
                let mut changed_content = content.clone();
                changed_content[0] = b'a' + run as u8;
                for i in 0..changed {
                    fs::write(root.join(format!("file-{i:05}.txt")), &changed_content)?;
                }
                let start = Instant::now();
                let value = shell_finish(&state, &state.workspace, "t", run + 1, before, &format!("perf-{run}"), &["@main".into()]).await?;
                finishes.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(value["complete"], true);
            }
            begins.sort_by(f64::total_cmp);
            finishes.sort_by(f64::total_cmp);
            println!("SHELL_LIFECYCLE_PERF files=1000 file_bytes=1024 mode={mode} samples=3 begin_median_ms={:.3} finish_median_ms={:.3} begin_max_ms={:.3} finish_max_ms={:.3}",
                begins[1], finishes[1], begins[2], finishes[2]);
            invalidate_snapshot_cache(&root);
            drop(state);
            let _ = fs::remove_dir_all(root);
        }
        Ok(())
    }

    #[tokio::test]
    async fn broad_mutations_wait_for_file_writes_and_cancel_cleanly() {
        let root = std::env::temp_dir().join(format!("changes-lock-{}", crate::agent_service::uuid_like()));
        fs::create_dir_all(&root).unwrap();
        let cancel = CancellationToken::new();
        let file = mutation_guard(&root, "write_file", &json!({"path":"a.txt"}), &cancel).await.unwrap();
        let broad_args = json!({});
        let pending = mutation_guard(&root, "run_project_script", &broad_args, &cancel);
        tokio::pin!(pending);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending).await.is_err());
        cancel.cancel();
        assert!(pending.await.is_err());
        drop(file);
        let fresh = CancellationToken::new();
        let broad = mutation_guard(&root, "run_project_script", &json!({}), &fresh).await.unwrap();
        let file_args = json!({"path":"b.txt"});
        let pending_file = mutation_guard(&root, "write_file", &file_args, &fresh);
        tokio::pin!(pending_file);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending_file).await.is_err());
        drop(broad);
        drop(pending_file.await.unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undo_restores_files_in_the_tasks_own_project() {
        let base =
            std::env::temp_dir().join(format!("changes-{}", crate::agent_service::uuid_like()));
        let (root, project) = (base.join("server"), base.join("project"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("a.txt"), "new\n").unwrap();
        fs::write(root.join("a.txt"), "server copy\n").unwrap();
        let conn = crate::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','completed',0,0)", []).unwrap();
        conn.execute(
            "INSERT INTO agent_session_meta(task_id,workspace) VALUES ('t',?1)",
            [project.to_string_lossy()],
        )
        .unwrap();
        let mut before = Snapshot::new();
        before.insert(
            "a.txt".into(),
            Image {
                exists: true,
                bytes: Some(b"old\n".to_vec()),
                stamp: String::new(),
            },
        );
        let mut after = Snapshot::new();
        after.insert(
            "a.txt".into(),
            Image {
                exists: true,
                bytes: Some(b"new\n".to_vec()),
                stamp: String::new(),
            },
        );
        record(&root, "t", 1, before, after).unwrap();
        undo(&root, "t", 1).unwrap();
        assert_eq!(fs::read_to_string(project.join("a.txt")).unwrap(), "old\n");
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "server copy\n",
            "the server checkout is untouched"
        );
        let _ = fs::remove_dir_all(&base);
    }

    fn line_file(count: usize, label: &str) -> String {
        (0..count).map(|i| format!("{label} {i}\n")).collect()
    }

    /// 3_000 lines on both sides exceed the exact table (9M cells), which is the
    /// case that used to report the whole file as changed.
    fn edited_big_file() -> (String, String, usize) {
        let before = line_file(3_000, "line");
        let mut lines: Vec<String> = before.lines().map(str::to_string).collect();
        let mut changed = 0;
        for index in (100..lines.len()).step_by(70) {
            lines[index] = format!("line {index} edited");
            changed += 1;
        }
        (before, lines.into_iter().map(|line| format!("{line}\n")).collect(), changed)
    }

    #[test]
    fn a_large_file_reports_its_edited_lines() {
        let (before, after, changed) = edited_big_file();
        assert!(3_000 * 3_000 > DIFF_TABLE_CELLS, "the fixture must take the search path");
        assert_eq!(counts(&before, &after), (changed, changed, false));
    }

    #[test]
    fn an_out_of_budget_search_estimates_the_edited_lines_and_marks_them_coarse() {
        let (before, after, changed) = edited_big_file();
        // One step of search budget cannot reach the end of a 6_000-line residual.
        assert_eq!(counts_within(&before, &after, 0, 1), (changed, changed, true));
    }

    #[test]
    fn the_table_and_the_greedy_search_agree_on_small_inputs() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = ["a\n", "b\n", "c\n", "d\n"];
        for _ in 0..300 {
            let (n, m) = ((next() % 13) as usize, (next() % 13) as usize);
            let a: Vec<&str> = (0..n).map(|_| alphabet[(next() % 4) as usize]).collect();
            let b: Vec<&str> = (0..m).map(|_| alphabet[(next() % 4) as usize]).collect();
            let common = lcs(&a, &b);
            let expected = (m - common, n - common, false);
            let before = a.concat();
            let after = b.concat();
            assert_eq!(counts_within(&before, &after, usize::MAX, 0), expected, "table: {before:?} {after:?}");
            assert_eq!(counts_within(&before, &after, 0, usize::MAX), expected, "search: {before:?} {after:?}");
        }
    }

    #[test]
    fn a_recorded_large_file_change_reports_exact_counts() -> Result<()> {
        let base = std::env::temp_dir().join(format!("changes-counts-{}", crate::agent_service::uuid_like()));
        fs::create_dir_all(&base)?;
        crate::agent_service::open_db(&base)?.execute(
            "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','completed',0,0)",
            [],
        )?;
        let (before, after, changed) = edited_big_file();
        let image = |bytes: String| Image { exists: true, bytes: Some(bytes.into_bytes()), stamp: String::new() };
        let mut before_snapshot = Snapshot::new();
        before_snapshot.insert("big.txt".into(), image(before));
        let mut after_snapshot = Snapshot::new();
        after_snapshot.insert("big.txt".into(), image(after));
        record(&base, "t", 1, before_snapshot, after_snapshot)?;
        let value = summary(&base, "t", 1)?;
        assert_eq!(value["files"][0]["coarse"], false);
        assert_eq!(value["files"][0]["added"], changed);
        assert_eq!(value["files"][0]["deleted"], changed);
        assert_eq!(value["added"], changed);
        assert_eq!(value["deleted"], changed);
        let _ = fs::remove_dir_all(&base);
        Ok(())
    }
    /// 2026-11-13: a project whose ignored `scratch/` and `runs/` folders held
    /// 38,617 of its 39,588 files exceeded the snapshot's file limit, so every
    /// shell command reported that its changes could not be recorded.
    #[test]
    fn a_shell_snapshot_leaves_out_what_git_ignores() -> Result<()> {
        let root = std::env::temp_dir().join(format!("grow-snapshot-ignore-{}", crate::agent_service::uuid_like()));
        fs::create_dir_all(root.join("scratch"))?;
        fs::write(root.join(".gitignore"), "/scratch/\n*.log\n")?;
        for relative in ["kept.txt", ".env", "scratch/output.txt", "build.log"] {
            fs::write(root.join(relative), b"x")?;
        }
        let names = |root: &Path| -> Result<Vec<String>> { Ok(capture(root, "run_program", &json!({}))?.into_keys().collect()) };
        // Outside a repository there is nothing git ignores.
        assert_eq!(names(&root)?, [".env", ".gitignore", "build.log", "kept.txt", "scratch/output.txt"]);
        fs::create_dir_all(root.join(".git"))?;
        invalidate_snapshot_cache(&root);
        assert_eq!(names(&root)?, [".env", ".gitignore", "kept.txt"], "hidden files stay tracked; ignored ones are left out");
        fs::remove_dir_all(&root)?;
        Ok(())
    }

}
