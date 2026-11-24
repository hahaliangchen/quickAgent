//! Per-turn before/after images. Never infer completed edits from model arguments.
use crate::server::agent_service::AgentServiceState;
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
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

// Legacy rewind serialization only. Ordinary mutations and undo share the
// workspace registry below, so a live shell cannot lock unrelated projects.
pub(crate) static MUTATIONS: RwLock<()> = RwLock::const_new(());
static WORKSPACE_MUTATIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Weak<RwLock<()>>>>> = std::sync::OnceLock::new();
static FILE_MUTATIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(PathBuf, PathBuf), std::sync::Weak<RwLock<()>>>>> = std::sync::OnceLock::new();

fn workspace_mutations(workspace: &Path) -> Result<(PathBuf, std::sync::Arc<RwLock<()>>)> {
    let root = path_lock_key(fs::canonicalize(workspace)?);
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
        if let Ok(conn) = crate::server::agent_service::open_db(&self.root) {
            let _ = conn.execute("DELETE FROM agent_resource_claims WHERE resource=?1 AND owner=?2", params![self.resource,self.owner]);
        }
    }
}
fn path_lock_key(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    let path = PathBuf::from(path.to_string_lossy().to_lowercase());
    path
}

pub(crate) fn mutation_paths(workspace: &Path, name: &str, args: &Value) -> Result<Vec<PathBuf>> {
    if name == crate::tools::batch_edits::NAME {
        return crate::tools::batch_edits::paths(workspace, args);
    }
    if !matches!(name, "write_file" | "edit_file" | "replace_range" | "delete_path" | "rename_path" | "create_dir") {
        return Ok(Vec::new());
    }
    let path = safe_path(workspace, args["path"].as_str().context("missing file path")?)?;
    let mut paths = vec![path];
    if name == "rename_path" {
        paths.push(safe_path(workspace, args["to"].as_str().context("missing destination path")?)?);
    }
    Ok(paths)
}

pub(crate) fn resource_keys(workspace: &Path, name: &str, args: &Value) -> Result<Vec<String>> {
    let paths = mutation_paths(workspace, name, args)?;
    let keys = paths.into_iter().map(|path| format!("local:{}", path_lock_key(path).display()))
        .collect::<std::collections::BTreeSet<_>>();
    Ok(keys.into_iter().collect())
}
pub(crate) fn claim_resource(root: &Path, task: &str, call_id: &str, resource: String) -> Result<ResourceClaim> {
    let owner=format!("main:{task}:{call_id}");
    let mut conn=crate::server::agent_service::open_db(root)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_resource_claims(resource TEXT PRIMARY KEY,task_id TEXT NOT NULL,owner TEXT NOT NULL,owner_agent TEXT,expires_at INTEGER NOT NULL);")?;
    let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now=chrono::Utc::now().timestamp_millis();
    let previous:Option<(String,i64)>=tx.query_row("SELECT owner,expires_at FROM agent_resource_claims WHERE resource=?1",[&resource],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((other,expiry))=previous {ensure!(other==owner || expiry<=now,"RESOURCE_WRITE_OWNERSHIP_CONFLICT: {resource} is owned by {other}");}
    tx.execute("INSERT INTO agent_resource_claims(resource,task_id,owner,owner_agent,expires_at) VALUES (?1,?2,?3,NULL,?4) ON CONFLICT(resource) DO UPDATE SET task_id=excluded.task_id,owner=excluded.owner,owner_agent=NULL,expires_at=excluded.expires_at",params![resource,task,owner,now+30*60*1000])?;
    tx.commit()?;
    Ok(ResourceClaim{root:root.to_path_buf(),resource,owner})
}

pub(crate) enum PathGuard {
    Read { _guard: tokio::sync::OwnedRwLockReadGuard<()> },
    Write { _guard: tokio::sync::OwnedRwLockWriteGuard<()> },
}

pub(crate) enum MutationGuard {
    File { _paths: Vec<PathGuard>, _workspace: tokio::sync::OwnedRwLockReadGuard<()> },
    Workspace { _workspace: tokio::sync::OwnedRwLockWriteGuard<()> },
}

pub(crate) async fn mutation_guard(workspace: &Path, name: &str, args: &Value, cancel: &CancellationToken) -> Result<MutationGuard> {
    let (root, workspace_lock) = workspace_mutations(workspace)?;
    let paths = mutation_paths(workspace, name, args)?;
    if paths.is_empty() {
        let guard = tokio::select! { _ = cancel.cancelled() => bail!("Tool execution cancelled"), guard = workspace_lock.write_owned() => guard };
        ensure!(workspace_mutations(workspace)?.0 == root, "workspace location changed while waiting");
        return Ok(MutationGuard::Workspace { _workspace: guard });
    }
    // Lock leaf paths exclusively and their parents in shared mode so a rename
    // or recursive delete cannot race a tool editing a child. Order all locks.
    let mut keys = BTreeMap::new();
    for path in &paths {
        let path = path_lock_key(path.clone());
        keys.insert(path.clone(), true);
        for parent in path.ancestors().skip(1).take_while(|parent| *parent != root) {
            keys.entry(parent.to_path_buf()).or_insert(false);
        }
    }
    let locks = {
        let mut registry = FILE_MUTATIONS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        registry.retain(|_, lock| lock.strong_count() > 0);
        keys.into_iter().map(|(path, exclusive)| {
            let key = (root.clone(), path);
            let lock = registry.get(&key).and_then(std::sync::Weak::upgrade).unwrap_or_else(|| {
                let lock = std::sync::Arc::new(RwLock::new(()));
                registry.insert(key, std::sync::Arc::downgrade(&lock));
                lock
            });
            (lock, exclusive)
        }).collect::<Vec<_>>()
    };
    let workspace_guard = tokio::select! { _ = cancel.cancelled() => bail!("Tool execution cancelled"), guard = workspace_lock.read_owned() => guard };
    let mut guards = Vec::with_capacity(locks.len());
    for (lock, exclusive) in locks {
        let guard = tokio::select! {
            _ = cancel.cancelled() => bail!("Tool execution cancelled"),
            guard = async {
                if exclusive { PathGuard::Write { _guard: lock.write_owned().await } }
                else { PathGuard::Read { _guard: lock.read_owned().await } }
            } => guard,
        };
        guards.push(guard);
    }
    ensure!(workspace_mutations(workspace)?.0 == root, "workspace location changed while waiting");
    ensure!(mutation_paths(workspace, name, args)? == paths, "file location changed while waiting");
    Ok(MutationGuard::File { _paths: guards, _workspace: workspace_guard })
}
static TEMP_ID: AtomicU64 = AtomicU64::new(0);
const FILE_LIMIT: u64 = 2 * 1024 * 1024;
const SNAPSHOT_LIMIT: usize = 128 * 1024 * 1024;

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
    hash: Option<String>,
    stamp: String,
}
type Snapshot = BTreeMap<String, Image>;

/// Images and ledger high-water mark retained while the process is running.
pub(crate) struct ShellSnapshot {
    store: PathBuf,
    workspace: PathBuf,
    task: String,
    turn: usize,
    token: String,
    start_seq: i64,
    before: Option<Snapshot>,
    managed: Vec<String>,
    errors: Vec<String>,
}

impl ShellSnapshot {
    pub(crate) fn token(&self) -> &str { &self.token }
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
    crate::server::agent_service::open_db(root)
}

/// Runs inside the agent schema's immediate transaction, including migration.
pub(crate) fn schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_file_changes (
        task_id TEXT NOT NULL, turn INTEGER NOT NULL, path TEXT NOT NULL,
        before_exists INTEGER NOT NULL, before_bytes BLOB, after_exists INTEGER NOT NULL, after_bytes BLOB,
        added INTEGER NOT NULL, deleted INTEGER NOT NULL, binary INTEGER NOT NULL, coarse INTEGER NOT NULL,
        unavailable INTEGER NOT NULL, conflicted INTEGER NOT NULL DEFAULT 0, reverted INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(task_id,turn,path));
        CREATE TABLE IF NOT EXISTS agent_change_turns(task_id TEXT NOT NULL,turn INTEGER NOT NULL,incomplete INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(task_id,turn));
        CREATE TABLE IF NOT EXISTS agent_shell_change_intervals(token TEXT PRIMARY KEY NOT NULL,workspace TEXT NOT NULL,task_id TEXT NOT NULL,turn INTEGER NOT NULL,start_seq INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS agent_shell_interval_metadata(task_id TEXT NOT NULL,turn INTEGER NOT NULL,workspace TEXT NOT NULL,interval_id TEXT NOT NULL,metadata TEXT NOT NULL,PRIMARY KEY(task_id,workspace,interval_id));
        CREATE TABLE IF NOT EXISTS agent_change_ledger (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace TEXT NOT NULL, task_id TEXT NOT NULL, turn INTEGER NOT NULL, path TEXT NOT NULL,
            source TEXT NOT NULL, interval_id TEXT,
            before_exists INTEGER NOT NULL, before_hash TEXT, before_bytes BLOB,
            after_exists INTEGER NOT NULL, after_hash TEXT, after_bytes BLOB, created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_change_ledger_ws_seq ON agent_change_ledger(workspace,seq);
        CREATE INDEX IF NOT EXISTS idx_change_ledger_task_turn ON agent_change_ledger(task_id,turn,seq);")?;
    let legacy = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('agent_shell_change_intervals') WHERE name='workspace' AND pk=1)",
        [], |row| row.get::<_, bool>(0))?;
    if legacy {
        conn.execute_batch("ALTER TABLE agent_shell_change_intervals RENAME TO agent_shell_change_intervals_legacy;
            CREATE TABLE agent_shell_change_intervals(token TEXT PRIMARY KEY NOT NULL,workspace TEXT NOT NULL,task_id TEXT NOT NULL,turn INTEGER NOT NULL,start_seq INTEGER NOT NULL DEFAULT 0);
            INSERT INTO agent_shell_change_intervals(token,workspace,task_id,turn) SELECT token,workspace,task_id,turn FROM agent_shell_change_intervals_legacy;
            DROP TABLE agent_shell_change_intervals_legacy;")?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_shell_intervals_workspace ON agent_shell_change_intervals(workspace,task_id,turn);
        CREATE INDEX IF NOT EXISTS idx_shell_intervals_turn ON agent_shell_change_intervals(task_id,turn);")?;
    Ok(())
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
            return Ok((Image::missing(), None));
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
    // Large files still get a content identity, even when Undo cannot retain them.
    let hash = if bytes.is_none() {
        let mut digest = Sha256::new();
        let size = std::io::copy(&mut fs::File::open(path)?.take(metadata.len() + 1), &mut digest)?;
        ensure!(size == metadata.len(), "file changed during snapshot: {}", path.display());
        Some(format!("{:x}", digest.finalize()))
    } else { None };
    let after = fs::metadata(path)?;
    ensure!(after.is_file() && FileStamp::new(&after) == stat, "file changed during snapshot: {}", path.display());
    Ok((Image {
        exists: true,
        bytes,
        hash,
        stamp,
    }, stat))
}

fn capture_path_or_tree(
    canonical_root: &Path,
    path: &Path,
    snapshot: &mut Snapshot,
    budget: &mut usize,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_file() {
        let relative = path
            .strip_prefix(canonical_root)?
            .to_string_lossy()
            .replace('\\', "/");
        snapshot.insert(relative, image(path, budget)?);
        return Ok(());
    }
    if metadata.is_dir() {
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            capture_directory(canonical_root, &dir, snapshot)?;
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                if entry.file_name() == ".git" {
                    continue;
                }
                let entry_path = entry.path();
                let entry_meta = fs::symlink_metadata(&entry_path)?;
                if entry_meta.file_type().is_symlink() {
                    continue;
                }
                if entry_meta.is_dir() {
                    stack.push(entry_path);
                } else if entry_meta.is_file() {
                    ensure!(
                        snapshot.len() < 20_000,
                        "workspace exceeds change-tracking file limit"
                    );
                    let safe = safe_path_canonical(canonical_root, &entry_path.to_string_lossy())?;
                    let relative = safe
                        .strip_prefix(canonical_root)?
                        .to_string_lossy()
                        .replace('\\', "/");
                    snapshot.insert(relative, image(&safe, budget)?);
                }
            }
        }
    }
    Ok(())
}

fn capture_directory(root: &Path, path: &Path, snapshot: &mut Snapshot) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) && path != root {
        ensure!(snapshot.len() < 20_000, "workspace exceeds change-tracking file limit");
        let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        // A trailing slash distinguishes directory entries from regular files.
        snapshot.insert(format!("{relative}/"), Image {
            exists: true, bytes: None, hash: Some(format!("{:x}", Sha256::digest([]))), stamp: String::new(),
        });
    }
    Ok(())
}

fn capture(root: &Path, name: &str, args: &Value) -> Result<Snapshot> {
    let mut snapshot = Snapshot::new();
    let mut budget = SNAPSHOT_LIMIT;
    if name == crate::tools::batch_edits::NAME || name == "git_restore" {
        let root = root.canonicalize()?;
        let paths = if name == crate::tools::batch_edits::NAME {
            crate::tools::batch_edits::paths(&root,args)?
        } else {
            let paths = crate::tools::git_tools::mutation_paths(&root,args)?;
            if args["staged"] == true { return Ok(snapshot); }
            paths.into_iter().map(|p|p.1).collect()
        };
        for path in paths { capture_path_or_tree(&root,&path,&mut snapshot,&mut budget)?; }
        return Ok(snapshot);
    }
    if name == "create_dir" {
        let root = root.canonicalize()?;
        let path = safe_path_canonical(&root, args["path"].as_str().context("missing directory path")?)?;
        for dir in path.ancestors().take_while(|dir| *dir != root) {
            capture_directory(&root, dir, &mut snapshot)?;
        }
        return Ok(snapshot);
    }
    if matches!(name, "delete_path" | "rename_path") {
        let canonical_root = fs::canonicalize(root)?;
        let path = safe_path(root, args["path"].as_str().context("missing file path")?)?;
        capture_path_or_tree(&canonical_root, &path, &mut snapshot, &mut budget)?;
        if name == "rename_path" {
            let to = safe_path(root, args["to"].as_str().context("missing destination path")?)?;
            capture_path_or_tree(&canonical_root, &to, &mut snapshot, &mut budget)?;
        }
        return Ok(snapshot);
    }
    if name != "git_commit" && !crate::tools::project_process::may_change_files(name) {
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

impl Image {
    fn missing() -> Self {
        Self { exists: false, bytes: None, hash: None, stamp: String::new() }
    }

    fn hash(&self) -> Option<String> {
        self.hash.clone().or_else(|| self.bytes.as_ref().map(|bytes| format!("{:x}", Sha256::digest(bytes))))
    }
}

fn same(a: &Image, b: &Image) -> bool {
    a.exists == b.exists && match (&a.bytes, &b.bytes) {
        (Some(a), Some(b)) => a == b,
        _ => a.hash() == b.hash(),
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

fn record_file(
    tx: &Connection, task: &str, turn: usize, path: &str, old: &Image, new: &Image, conflicted: bool,
) -> Result<()> {
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
    let conflict = conflict || conflicted;
    if !conflict && original_exists == new.exists
        && original_bytes == new.bytes
        && (!new.exists || new.bytes.is_some())
    {
        tx.execute(
            "DELETE FROM agent_file_changes WHERE task_id=?1 AND turn=?2 AND path=?3",
            params![task, turn, path],
        )?;
        return Ok(());
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
    Ok(())
}

struct LedgerScope<'a> {
    workspace: &'a Path,
    task: &'a str,
    turn: usize,
    interval_id: Option<&'a str>,
}

impl LedgerScope<'_> {
    fn append(&self, tx: &Connection, path: &str, old: &Image, new: &Image) -> Result<()> {
        let source = if self.interval_id.is_some() { "shell" } else { "tool" };
        tx.execute("INSERT INTO agent_change_ledger(workspace,task_id,turn,path,source,interval_id,
            before_exists,before_hash,before_bytes,after_exists,after_hash,after_bytes,created_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![self.workspace.to_string_lossy(),self.task,self.turn,path,source,self.interval_id,
                old.exists,old.hash(),old.bytes,new.exists,new.hash(),new.bytes,chrono::Utc::now().timestamp_millis()])?;
        Ok(())
    }
}

fn record_tools(tx: &Connection, scope: &LedgerScope<'_>, before: &Snapshot, after: &Snapshot) -> Result<()> {
    let missing = Image::missing();
    let keys: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
    for path in keys {
        let old = before.get(path).unwrap_or(&missing);
        let new = after.get(path).unwrap_or(&missing);
        if same(old, new) { continue; }
        scope.append(tx, path, old, new)?;
        // Keep directory changes in the ledger without changing the existing file-only Undo contract.
        if !path.ends_with('/') {
            record_file(tx, scope.task, scope.turn, path, old, new, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn record(root: &Path, task: &str, turn: usize, before: Snapshot, after: Snapshot) -> Result<Value> {
    let mut conn = connection(root)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    record_tools(&tx, &LedgerScope { workspace: root, task, turn, interval_id: None }, &before, &after)?;
    tx.commit()?;
    summary(root, task, turn)
}

fn mark_incomplete_tx(conn: &Connection, task: &str, turn: usize) -> Result<()> {
    conn.execute("INSERT INTO agent_change_turns(task_id,turn,incomplete) VALUES (?1,?2,1) ON CONFLICT(task_id,turn) DO UPDATE SET incomplete=1", params![task, turn])?;
    Ok(())
}

fn mark_incomplete(root: &Path, task: &str, turn: usize) -> Result<()> {
    mark_incomplete_tx(&connection(root)?, task, turn)?;
    Ok(())
}

#[cfg(test)]
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

#[cfg(test)]
async fn shell_begin(state: &AgentServiceState, workspace: &Workspace, task: &str, turn: usize) -> Result<ShellSnapshot> {
    shell_begin_cancellable(state, workspace, task, turn, &CancellationToken::new()).await
}

/// The barrier prevents tool writes from landing between the high-water mark
/// and the snapshot. It is released before launching the process.
pub(crate) async fn shell_begin_cancellable(
    state: &AgentServiceState,
    workspace: &Workspace,
    task: &str,
    turn: usize,
    cancel: &CancellationToken,
) -> Result<ShellSnapshot> {
    let guard = mutation_guard(workspace.root(), "exec_command", &json!({}), cancel).await?;
    let state = state.clone();
    let files = workspace.root().to_path_buf();
    let task = task.to_owned();
    let before = tokio::task::spawn_blocking(move || -> Result<ShellSnapshot> {
        let _guard = guard;
        let store = fs::canonicalize(state.workspace.root())?;
        let workspace = fs::canonicalize(files)?;
        let token = format!("interval_{}", crate::server::agent_service::uuid_like());
        let mut conn = connection(&store)?;
        summary(&store, &task, turn)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let start_seq = tx.query_row("SELECT COALESCE(MAX(seq),0) FROM agent_change_ledger WHERE workspace=?1",
            [workspace.to_string_lossy()], |row| row.get(0))?;
        tx.execute("INSERT INTO agent_shell_change_intervals(workspace,task_id,turn,token,start_seq) VALUES (?1,?2,?3,?4,?5)",
            params![workspace.to_string_lossy(), task, turn, token, start_seq])?;
        tx.commit()?;
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
        if !errors.is_empty() {
            mark_incomplete(&store, &task, turn)?;
        }
        Ok((before, managed, errors))
        })();
        let (before, managed, errors) = match captured {
            Ok(captured) => captured,
            Err(error) => return Err(shell_begin_error(error, abandon_shell_begin(&store, &workspace, &task, turn, &token))),
        };
        Ok(ShellSnapshot { store, workspace, task, turn, token, start_seq, before, managed, errors })
    }).await??;
    let root = before.store.clone();
    let task = before.task.clone();
    let published = async {
        let value = tokio::task::spawn_blocking(move || summary(&root, &task, turn)).await??;
        crate::server::agent_service::emit(&before.store, &before.task, "workspace/changes", value).await
    }.await;
    if let Err(error) = published {
        let (store, workspace, task, token) = (before.store.clone(), before.workspace.clone(), before.task.clone(), before.token.clone());
        let cleanup = tokio::task::spawn_blocking(move || abandon_shell_begin(&store, &workspace, &task, turn, &token))
            .await.map_err(anyhow::Error::from).and_then(|result| result);
        return Err(shell_begin_error(error, cleanup));
    }
    Ok(before)
}

/// Reconcile only after the last process in this interval has exited. The
/// snapshot barrier also ensures all tool writes visible on disk are in the WAL.
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
    ensure!(fs::canonicalize(state.workspace.root())? == before.store
        && fs::canonicalize(workspace.root())? == before.workspace, "shell snapshot belongs to another workspace");
    let attribution = ShellAttribution::new(interval_id, owners)?;
    let guard = mutation_guard(workspace.root(), "exec_command", &json!({}), &CancellationToken::new()).await?;
    let root = before.store.clone();
    let task = before.task.clone();
    let files = before.workspace.clone();
    let (after, mut errors) = tokio::task::spawn_blocking(move || {
        match capture(&files, "run_program", &json!({})) {
            Ok(after) => (Some(after), Vec::new()),
            Err(error) => (None, vec![format!("after snapshot: {error:#}")]),
        }
    }).await?;
    let mut before = before;
    errors.append(&mut before.errors);
    let capture_failed = !errors.is_empty() || before.before.is_none() || after.is_none();
    let cleanup = (before.workspace.clone(), before.task.clone(), before.token.clone());
    let recorded = crate::context::db_writer::write_async(&root, move |conn| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let removed = tx.execute("DELETE FROM agent_shell_change_intervals WHERE workspace=?1 AND task_id=?2 AND turn=?3 AND token=?4",
            params![before.workspace.to_string_lossy(), before.task, turn, before.token])?;
        ensure!(removed == 1, "shell interval is no longer active");
        let (adoptable, changed_incomplete) = match (&before.before, &after) {
            (Some(old), Some(new)) => reconcile_shell(&tx, &before, &attribution, old, new)?,
            _ => (Default::default(), true),
        };
        if capture_failed || changed_incomplete { mark_incomplete_tx(&tx, &before.task, turn)?; }
        tx.execute("INSERT INTO agent_shell_interval_metadata(task_id,turn,workspace,interval_id,metadata) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(task_id,workspace,interval_id) DO UPDATE SET metadata=excluded.metadata",
            params![before.task, turn, before.workspace.to_string_lossy(), attribution.interval_id, attribution.metadata().to_string()])?;
        tx.commit()?;
        Ok((before, after, attribution, adoptable))
    }).await;
    let (before, after, attribution, adoptable) = match recorded {
        Ok(recorded) => recorded,
        Err(error) => {
            let (files, task, token) = cleanup;
            let saved_root = root.clone();
            let cleanup = tokio::task::spawn_blocking(move || abandon_shell_begin(&saved_root, &files, &task, turn, &token))
                .await.map_err(anyhow::Error::from).and_then(|result| result);
            return Err(shell_begin_error(error, cleanup));
        }
    };
    let state = state.clone();
    let (value, errors) = tokio::task::spawn_blocking(move || -> Result<_> {
        // Retain the barrier until baseline adoption is finished, but not while publishing events.
        let _guard = guard;
        if let (Some(old), Some(new)) = (before.before, after) {
            for relative in before.managed.into_iter().filter(|path| adoptable.contains(path)) {
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
        }
        if !errors.is_empty() { mark_incomplete(&before.store, &before.task, turn)?; }
        Ok((summary(&before.store, &before.task, turn)?, errors))
    }).await??;
    crate::server::agent_service::emit(&root, &task, "workspace/changes", value.clone()).await?;
    if !errors.is_empty() {
        crate::server::agent_service::emit(&root, &task, "workspace/changes_error",
            json!({"turn":turn,"message":errors.join("; ")})).await?;
    }
    Ok(value)
}

fn reconcile_shell(
    tx: &Connection, interval: &ShellSnapshot, attribution: &ShellAttribution,
    before: &Snapshot, after: &Snapshot,
) -> Result<(std::collections::BTreeSet<String>, bool)> {
    let workspace = interval.workspace.to_string_lossy();
    let overlapping: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_shell_change_intervals WHERE workspace=?1)",
        [&workspace], |row| row.get(0))?;
    let mut stmt = tx.prepare("SELECT seq,path,after_exists,after_hash FROM agent_change_ledger WHERE workspace=?1 AND seq>?2 ORDER BY seq")?;
    let mut latest = BTreeMap::new();
    for row in stmt.query_map(params![workspace,interval.start_seq], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?, row.get::<_, Option<String>>(3)?))
    })? {
        let (seq, path, exists, hash) = row?;
        latest.insert(path, (seq, exists, hash));
    }
    let scope = LedgerScope { workspace: &interval.workspace, task: &interval.task, turn: interval.turn,
        interval_id: Some(&attribution.interval_id) };
    let mut adoptable = std::collections::BTreeSet::new();
    let mut changed_incomplete = false;
    let missing = Image::missing();
    for path in before.keys().chain(after.keys()).collect::<std::collections::BTreeSet<_>>() {
        let old = before.get(path).unwrap_or(&missing);
        let new = after.get(path).unwrap_or(&missing);
        let tracked = latest.get(path);
        if let Some((_, exists, hash)) = tracked {
            if new.exists == *exists && new.hash() == *hash { continue; }
        } else if same(old, new) { continue; }
        let residual_before = tracked.map(|(seq, exists, hash)| -> Result<Image> {
            let bytes = tx.query_row("SELECT after_bytes FROM agent_change_ledger WHERE seq=?1", [seq], |row| row.get(0))?;
            Ok(Image { exists: *exists, hash: hash.clone(), bytes, stamp: String::new() })
        }).transpose()?;
        let old = residual_before.as_ref().unwrap_or(old);
        if (old.exists && old.bytes.is_none()) || (new.exists && new.bytes.is_none()) {
            changed_incomplete = true;
        }
        // Two running processes cannot be distinguished by snapshots alone. Record
        // the observed residual once, but do not let ambiguous ownership enable undo.
        let conflicted = tracked.is_some() || overlapping;
        scope.append(tx, path, old, new)?;
        record_file(tx, &interval.task, interval.turn, path, old, new, conflicted)?;
        if tracked.is_some() {
            tx.execute("UPDATE agent_file_changes SET conflicted=1 WHERE path=?1 AND (task_id,turn) IN (
                SELECT task_id,turn FROM agent_change_ledger WHERE workspace=?2 AND seq>?3 AND path=?1 AND source='tool')",
                params![path,workspace,interval.start_seq])?;
        }
        if !conflicted { adoptable.insert(path.clone()); }
    }
    Ok((adoptable, changed_incomplete))
}

pub fn summary(root: &Path, task: &str, turn: usize) -> Result<Value> {
    summary_from(&connection(root)?, task, turn)
}

fn summary_from(conn: &Connection, task: &str, turn: usize) -> Result<Value> {
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
    crate::tools::tool_registry::ToolRegistry::is_tracked_mutation(name)
        // Managed shell sessions have their own boundary and change intervals.
        || (!crate::tools::shell_process::is_tool(name) && crate::tools::project_process::may_change_files(name))
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
    if name == crate::tools::batch_edits::NAME {
        crate::tools::batch_edits::paths(&files,&args)?;
        invalidate_snapshot_cache(&files);
    } else if crate::tools::git_tools::mutates(&name) {
        crate::tools::git_tools::mutation_paths(&files,&args)?;
        invalidate_snapshot_cache(&files);
    } else if !crate::tools::project_process::may_change_files(&name) {
        // Boundary checks are mandatory even if optional snapshot capture fails.
        safe_path(&files, args["path"].as_str().context("missing file path")?)?;
        if name == "rename_path" {
            safe_path(&files, args["to"].as_str().context("missing destination path")?)?;
        }
        invalidate_snapshot_cache(&files);
    }
    let capture_root = files.clone();
    let capture_name = name.clone();
    let capture_args = args.clone();
    let before =
        tokio::task::spawn_blocking(move || capture(&capture_root, &capture_name, &capture_args))
            .await?;
    let boundary = crate::tools::process_sandbox::ExecutionBoundary::for_mode(state.permission_mode);
    let result = if crate::tools::project_process::is_tool(&name) {
        crate::tools::project_process::execute_with_boundary(&workspace, &name, args.clone(), &cancel, boundary).await
    } else if crate::tools::git_tools::is_tool(&name) {
        crate::tools::git_tools::execute(&workspace,&name,&args,boundary,&cancel).await
    } else if crate::tools::verify_tools::is_tool(&name) {
        crate::tools::verify_tools::execute(&workspace, &name, &args, &cancel, boundary).await
    } else if crate::tools::program_execution::is_tool(&name) {
        crate::tools::program_execution::execute_with_boundary(&workspace, &args, &cancel, boundary).await
    } else {
        // A started blocking file commit cannot be aborted. Await completion
        // before recording its after-image or releasing the mutation lock.
        if cancel.is_cancelled() {
            Err(anyhow::anyhow!("Tool execution cancelled"))
        } else {
            crate::server::agent_service::execute_tool(
                workspace.clone(),
                state.tool_catalog.as_deref(),
                &name,
                args.clone(),
            )
            .await
        }
    };
    if !crate::tools::project_process::may_change_files(&name) {
        invalidate_snapshot_cache(&files);
    }
    let background_running = result.as_ref().is_ok_and(|value| value["running"] == true);
    let metadata_changed = result.as_ref().is_ok_and(|v|v["git_metadata_changed"] == true)
        || result.as_ref().is_err_and(|e| e.downcast_ref::<crate::tools::git_tools::MetadataMayHaveChanged>().is_some());
    let captured = tokio::task::spawn_blocking(move || -> Result<_> {
        let before = before?;
        let after = capture(&files, &name, &args)?;
        Ok((fs::canonicalize(&files)?, before, after))
    }).await?;
    let saved_task = task.clone();
    let changes = match captured {
        Ok((files, before, after)) => crate::context::db_writer::write_async(&root, move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            record_tools(&tx, &LedgerScope { workspace: &files, task: &saved_task, turn, interval_id: None }, &before, &after)?;
            // The process can still write after we return its background handle.
            if metadata_changed || background_running { mark_incomplete_tx(&tx, &saved_task, turn)?; }
            let value = summary_from(&tx, &saved_task, turn)?;
            tx.commit()?;
            Ok(value)
        }).await,
        Err(error) => Err(error),
    };
    if metadata_changed {
        crate::server::agent_service::emit(&root, &task, "workspace/git_metadata", json!({"turn":turn,"reversible":false,"confirmed":result.is_ok()})).await?;
    }
    match changes {
        Ok(summary) => {
            crate::server::agent_service::emit(&root, &task, "workspace/changes", summary).await?;
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
                crate::server::agent_service::emit(&root, &task, "workspace/changes", summary).await?;
            }
            crate::server::agent_service::emit(
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

pub(crate) fn destructive_guard(root: &Path, files: &Path) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>> {
    let (_, lock) = workspace_mutations(files)?;
    let guard = lock.try_write_owned().map_err(|_| anyhow::anyhow!("conflict: workspace files are being mutated"))?;
    ensure_no_shell(root, files)?;
    Ok(guard)
}

pub(crate) fn ensure_no_shell(root: &Path, files: &Path) -> Result<()> {
    let workspace = fs::canonicalize(files)?;
    let shell_running: bool = connection(root)?.query_row("SELECT EXISTS(SELECT 1 FROM agent_shell_change_intervals WHERE workspace=?1)",
        [workspace.to_string_lossy()], |row| row.get(0))?;
    ensure!(!shell_running, "conflict: SHELL_BUSY: wait for the running shell to exit before restoring files");
    Ok(())
}

pub(crate) fn undo(root: &Path, task: &str, turn: usize) -> Result<Value> {
    let files = crate::context::session_browser::task_workspace(root, task)?;
    let _guard = destructive_guard(root, &files)?;
    undo_locked(root, task, turn, &files)
}

/// The caller must hold the destructive workspace guard.
pub(crate) fn undo_locked(root: &Path, task: &str, turn: usize, files: &Path) -> Result<Value> {
    let conn = connection(root)?;
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
    let files = match crate::context::session_browser::task_workspace(&root, &task) {
        Ok(files) => files,
        Err(error) => return response(Ok(Err(error))),
    };
    if let Err(error) = crate::tools::shell_process::wait_for_own_writers(&files, &root, &task).await {
        return response(Ok(Err(error)));
    }
    let saved_root = root.clone();
    let saved_task = task.clone();
    let result = tokio::task::spawn_blocking(move || undo(&saved_root, &saved_task, turn)).await;
    if let Ok(Ok(ref summary)) = result {
        let _ =
            crate::server::agent_service::emit(&root, &task, "workspace/changes", summary.clone()).await;
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
    include!("parts/workspace_changes_tests.inc.rs");
}
