//! Host-owned file baselines for Agent edits, including what the task's own
//! commands change. External MCP keeps its own contract.
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{fs, path::{Path, PathBuf}};

fn connection(root: &Path) -> Result<rusqlite::Connection> {
    let conn = crate::server::agent_service::open_db(root)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_file_baselines (
        task_id TEXT NOT NULL, workspace TEXT NOT NULL, path TEXT NOT NULL,
        exists_at_baseline INTEGER NOT NULL, bytes BLOB,
        PRIMARY KEY(task_id, workspace, path));
        CREATE TABLE IF NOT EXISTS agent_file_writer_versions (
        task_id TEXT NOT NULL, workspace TEXT NOT NULL, path TEXT NOT NULL, owner TEXT NOT NULL,
        turn INTEGER NOT NULL, seq INTEGER NOT NULL,
        PRIMARY KEY(task_id, workspace, path, owner, turn));
        CREATE TABLE IF NOT EXISTS agent_file_writer_metadata (
        task_id TEXT NOT NULL,workspace TEXT NOT NULL,path TEXT NOT NULL,owner TEXT NOT NULL,
        turn INTEGER NOT NULL,seq INTEGER NOT NULL,metadata TEXT NOT NULL,
        PRIMARY KEY(task_id,workspace,path,owner,turn));")?;
    Ok(conn)
}

fn key(workspace: &Path, raw: &str) -> Result<(String, String, PathBuf)> {
    let root = workspace.canonicalize()?;
    let path = crate::tools::file_edit::workspace_path(&root, raw)?;
    let relative = path.strip_prefix(&root)?.to_string_lossy().replace('\\', "/");
    Ok((root.to_string_lossy().into_owned(), relative, path))
}

fn baseline(root: &Path, task: &str, workspace: &str, path: &str) -> Result<Option<Option<Vec<u8>>>> {
    let conn = connection(root)?;
    Ok(conn.query_row("SELECT exists_at_baseline, bytes FROM agent_file_baselines WHERE task_id=?1 AND workspace=?2 AND path=?3",
        params![task, workspace, path], |r| {
            let exists: bool = r.get(0)?;
            let bytes: Option<Vec<u8>> = r.get(1)?;
            Ok(if exists { bytes } else { None })
        }).optional()?)
}

fn save(root: &Path, task: &str, workspace: &str, path: &str, bytes: Option<&[u8]>, replace: bool) -> Result<()> {
    let conn = connection(root)?;
    let sql = if replace {
        "INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(task_id,workspace,path) DO UPDATE SET exists_at_baseline=excluded.exists_at_baseline,bytes=excluded.bytes"
    } else {
        "INSERT OR IGNORE INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES (?1,?2,?3,?4,?5)"
    };
    conn.execute(sql, params![task, workspace, path, bytes.is_some(), bytes])?;
    Ok(())
}

/// Each write takes the task's next write version. `owner` is the parallel
/// function that wrote, or `@main` for the main program and its serial calls.
fn record_writer(root: &Path, task: &str, workspace: &str, path: &str, owner: &str, turn: usize) -> Result<()> {
    record_writer_on(&connection(root)?, task, workspace, path, owner, turn)
}

fn record_writer_on(conn: &rusqlite::Connection, task: &str, workspace: &str, path: &str, owner: &str, turn: usize) -> Result<()> {
    conn.execute("INSERT INTO agent_file_writer_versions(task_id,workspace,path,owner,turn,seq) VALUES (?1,?2,?3,?4,?5,(SELECT COALESCE(MAX(seq),0)+1 FROM agent_file_writer_versions WHERE task_id=?1)) ON CONFLICT(task_id,workspace,path,owner,turn) DO UPDATE SET seq=excluded.seq",
        params![task, workspace, path, owner, turn as i64])?;
    // A later ordinary write by this owner must not inherit shell provenance.
    conn.execute("DELETE FROM agent_file_writer_metadata WHERE task_id=?1 AND workspace=?2 AND path=?3 AND owner=?4 AND turn=?5",
        params![task, workspace, path, owner, turn])?;
    Ok(())
}

// The parent module deliberately stays private. These crate-scoped wrappers
// allow shell tracking to use its captured bytes without exposing baseline
// internals or reading every managed file a second time.
impl crate::server::agent_service::AgentServiceState {
    pub(crate) fn shell_matching_baselines(
        &self,
        task: &str,
        workspace: &Path,
        matches: impl Fn(&str, Option<&[u8]>) -> bool,
    ) -> Result<Vec<String>> {
        let work = workspace.canonicalize()?.to_string_lossy().into_owned();
        let conn = connection(self.workspace.root())?;
        let mut statement = conn.prepare("SELECT path,exists_at_baseline,bytes FROM agent_file_baselines WHERE task_id=?1 AND workspace=?2")?;
        let mut rows = statement.query(params![task, work])?;
        let mut files = Vec::new();
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let exists: bool = row.get(1)?;
            let bytes: Option<Vec<u8>> = row.get(2)?;
            // Missing bytes for an existing baseline are not evidence of absence.
            if (!exists || bytes.is_some()) && matches(&path, if exists { bytes.as_deref() } else { None }) {
                files.push(path);
            }
        }
        Ok(files)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn shell_adopt_baseline(
        &self,
        task: &str,
        workspace: &Path,
        turn: usize,
        relative: &str,
        before: Option<&[u8]>,
        after: Option<&[u8]>,
        attribution: &crate::tools::workspace_changes::ShellAttribution,
    ) -> Result<()> {
        if before == after { return Ok(()); }
        let work = workspace.to_string_lossy();
        let mut conn = connection(self.workspace.root())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Preserve a newer host baseline if it changed during the interval.
        let changed = tx.execute("UPDATE agent_file_baselines SET exists_at_baseline=?4,bytes=?5 WHERE task_id=?1 AND workspace=?2 AND path=?3 AND exists_at_baseline=?6 AND bytes IS ?7",
            params![task, work, relative, after.is_some(), after, before.is_some(), before])?;
        if changed > 0 {
            let owner = attribution.writer();
            record_writer_on(&tx, task, &work, relative, &owner, turn)?;
            tx.execute("INSERT INTO agent_file_writer_metadata(task_id,workspace,path,owner,turn,seq,metadata) SELECT task_id,workspace,path,owner,turn,seq,?6 FROM agent_file_writer_versions WHERE task_id=?1 AND workspace=?2 AND path=?3 AND owner=?4 AND turn=?5",
                params![task, work, relative, owner, turn, attribution.metadata().to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// The task's latest write version. A function started at this version has
/// not seen any write numbered above it.
pub(super) fn write_clock(root: &Path, task: &str) -> Result<i64> {
    Ok(connection(root)?.query_row("SELECT COALESCE(MAX(seq),0) FROM agent_file_writer_versions WHERE task_id=?1", [task], |r| r.get(0))?)
}

/// The task's managed files that still match their baselines when one of its
/// commands starts. What the command then changes in them is the task's own edit.
pub(super) struct Untouched {
    workspace: String,
    files: Vec<(String, PathBuf, Option<Vec<u8>>)>,
}

pub(super) fn untouched(root: &Path, task: &str, workspace: &Path) -> Result<Untouched> {
    let canonical = workspace.canonicalize()?;
    let work = canonical.to_string_lossy().into_owned();
    let conn = connection(root)?;
    let mut statement = conn.prepare("SELECT path, exists_at_baseline, bytes FROM agent_file_baselines WHERE task_id=?1 AND workspace=?2")?;
    let rows = statement.query_map(params![task, work], |r| {
        let exists: bool = r.get(1)?;
        let bytes: Option<Vec<u8>> = r.get(2)?;
        Ok((r.get::<_, String>(0)?, if exists { bytes } else { None }))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    // A file that already differs from its baseline keeps it, so that outside
    // change still stops the next edit.
    let files = rows.into_iter().filter_map(|(relative, base)| {
        let path = crate::tools::file_edit::workspace_path(&canonical, &relative).ok()?;
        (crate::tools::file_edit::snapshot(&path).ok()? == base).then_some((relative, path, base))
    }).collect();
    Ok(Untouched { workspace: work, files })
}

impl Untouched {
    /// After the command, what it changed becomes the baseline and counts as a
    /// write by whoever ran it, as its edits do.
    pub(super) fn adopt(self, root: &Path, task: &str, owner: Option<&str>, turn: usize) -> Result<()> {
        crate::tools::workspace_changes::invalidate_snapshot_cache(Path::new(&self.workspace));
        for (relative, path, before) in self.files {
            let Ok(after) = crate::tools::file_edit::snapshot(&path) else { continue };
            if after == before {
                continue;
            }
            save(root, task, &self.workspace, &relative, after.as_deref(), true)?;
            record_writer(root, task, &self.workspace, &relative, owner.unwrap_or("@main"), turn)?;
        }
        Ok(())
    }
}

/// At the start of a turn, a managed file that changed since its baseline
/// takes its current content as the baseline: between turns the user is in
/// control, so what changed then is the world the new turn works on, not an
/// external edit to stop on. A change made while a turn runs still conflicts.
/// Returns the files whose baseline moved.
pub(super) fn refresh_baselines(root: &Path, task: &str) -> Result<Vec<String>> {
    let rows: Vec<(String, String, bool, Option<Vec<u8>>)> = connection(root)?
        .prepare("SELECT workspace,path,exists_at_baseline,bytes FROM agent_file_baselines WHERE task_id=?1")?
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut moved = Vec::new();
    for (workspace, path, exists, bytes) in rows {
        // Missing bytes for an existing baseline are not evidence of absence.
        if exists && bytes.is_none() {
            continue;
        }
        // An unreadable file keeps its baseline; the next write reports it.
        let Ok(current) = crate::tools::file_edit::snapshot(&Path::new(&workspace).join(&path)) else { continue };
        if current != bytes {
            save(root, task, &workspace, &path, current.as_deref(), true)?;
            moved.push(path);
        }
    }
    Ok(moved)
}

/// A managed file that no longer matches the task's record while a turn runs:
/// someone outside the task (the user, another agent, a program) changed it.
pub(super) struct OutsideChange {
    pub path: String,
    pub current_hash: String,
    pub change: Value,
}

/// The task's files that changed outside it since it last read or wrote
/// them. Nothing is moved: the AI is told, and the next edit of such a file
/// hands it the change to decide on (`Prepared::preserve`).
pub(super) fn outside_changes(root: &Path, task: &str) -> Result<Vec<OutsideChange>> {
    let rows: Vec<(String, String, bool, Option<Vec<u8>>)> = connection(root)?
        .prepare("SELECT workspace,path,exists_at_baseline,bytes FROM agent_file_baselines WHERE task_id=?1")?
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut changes = Vec::new();
    for (workspace, path, exists, bytes) in rows {
        if exists && bytes.is_none() {
            continue;
        }
        let Ok(current) = crate::tools::file_edit::snapshot(&Path::new(&workspace).join(&path)) else { continue };
        if current != bytes {
            changes.push(OutsideChange {
                current_hash: current.as_deref().map(crate::index::symbol_description::content_hash).unwrap_or_else(|| "deleted".into()),
                change: change_summary(bytes.as_deref(), current.as_deref()),
                path,
            });
        }
    }
    Ok(changes)
}

/// What changed between two versions of a file, small enough to show an AI:
/// the changed line range and its text before and after, cut when long.
pub(super) fn change_summary(before: Option<&[u8]>, after: Option<&[u8]>) -> Value {
    const SHOWN: usize = 2000;
    let cut = |text: &str| -> Value {
        let mut chars = text.chars();
        let shown: String = chars.by_ref().take(SHOWN).collect();
        if chars.next().is_some() { json!({"text":shown,"truncated":true}) } else { json!(shown) }
    };
    let (before, after) = match (before, after) {
        (None, None) => return json!({"kind":"unchanged"}),
        (None, Some(_)) => return json!({"kind":"created"}),
        (Some(_), None) => return json!({"kind":"deleted"}),
        (Some(before), Some(after)) => (before, after),
    };
    let (Ok(before), Ok(after)) = (std::str::from_utf8(before), std::str::from_utf8(after)) else {
        return json!({"kind":"modified","binary":true});
    };
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let new: Vec<&str> = after.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let removed = &old[prefix..old.len() - suffix];
    let added = &new[prefix..new.len() - suffix];
    json!({"kind":"modified","first_line":prefix + 1,
        "lines_before":removed.len(),"lines_after":added.len(),
        "before":cut(&removed.concat()),"after":cut(&added.concat())})
}

/// A removed or moved path (or directory tree) has no baseline left to compare
/// against. Dropping both exact and child rows keeps the next write from being
/// misread as an external change.
pub(super) fn forget(root: &Path, task: &str, workspace: &Path, raw: &str) -> Result<()> {
    let (work, relative, _) = key(workspace, raw)?;
    let prefix = format!("{relative}/");
    let conn = connection(root)?;
    for table in [
        "agent_file_baselines",
        "agent_file_writer_versions",
        "agent_file_writer_metadata",
    ] {
        conn.execute(
            &format!(
                "DELETE FROM {table} WHERE task_id=?1 AND workspace=?2 \
                 AND (path=?3 OR (length(path) > length(?3) AND substr(path, 1, length(?3) + 1) = ?4))"
            ),
            params![task, work, relative, prefix],
        )?;
    }
    Ok(())
}

pub(super) fn guard_copy(root: &Path, task: &str, workspace: &Path, raw: &str) -> Result<()> {
    let (work, relative, target) = key(workspace, raw)?;
    let prefix = format!("{relative}/");
    let conn = connection(root)?;
    let mut statement = conn.prepare("SELECT path,exists_at_baseline,bytes FROM agent_file_baselines WHERE task_id=?1 AND workspace=?2 AND (path=?3 OR (length(path)>length(?3) AND substr(path,1,length(?3)+1)=?4))")?;
    let rows = statement.query_map(params![task,work,relative,prefix], |row| {
        Ok((row.get::<_,String>(0)?,row.get::<_,bool>(1)?,row.get::<_,Option<Vec<u8>>>(2)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (path, existed, bytes) in rows {
        let expected = if existed { Some(bytes.context("write_conflict: copied destination has no saved content baseline")?) } else { None };
        let current = crate::tools::file_edit::snapshot(&Path::new(&work).join(&path))?;
        anyhow::ensure!(current == expected,"write_conflict: {path} changed outside this task; read it again before copying");
    }
    if target.is_dir() {
        for entry in fs::read_dir(&target)? {
            anyhow::ensure!(entry?.file_name() != ".git", "refusing to copy over repository metadata");
        }
    }
    Ok(())
}

pub(super) fn adopt_copy(root: &Path, task: &str, workspace: &Path, raw: &str, owner: &str, turn: usize) -> Result<()> {
    forget(root, task, workspace, raw)?;
    let (work, relative, target) = key(workspace, raw)?;
    let mut files = Vec::new();
    if target.is_file() { files.push((relative,target)); }
    else if target.is_dir() {
        let mut stack=vec![target.clone()];
        let mut entries=0usize;
        while let Some(dir)=stack.pop() {
            for item in fs::read_dir(&dir)? {
                entries+=1;
                anyhow::ensure!(entries<=10_000,"copied directory exceeds managed-baseline entry limit");
                let entry=item?;
                if entry.file_name()==".git" { continue; }
                let path=entry.path(); let meta=fs::symlink_metadata(&path)?;
                if meta.file_type().is_symlink() { continue; }
                if meta.is_dir() { stack.push(path); }
                else if meta.is_file() {
                    let relative=path.strip_prefix(workspace)?.to_string_lossy().replace('\\',"/");
                    files.push((relative,path));
                    anyhow::ensure!(files.len()<=5000,"copied directory exceeds managed-baseline file limit");
                }
            }
        }
    } else { anyhow::bail!("copied destination is not a regular file or directory"); }
    let mut bytes=0usize;
    for (path,absolute) in files {
        let snapshot=crate::tools::file_edit::snapshot(&absolute)?;
        let content=snapshot.context("copied file exceeds the managed baseline size limit")?;
        bytes=bytes.saturating_add(content.len());
        anyhow::ensure!(bytes<=128*1024*1024,"copied directory exceeds the managed baseline byte limit");
        save(root,task,&work,&path,Some(&content),true)?;
        record_writer(root,task,&work,&path,owner,turn)?;
    }
    Ok(())
}

/// Record a file a read or a shell command produced, so a later write compares
/// against what the task itself last saw. Only a snapshot that matches the
/// actual version delivered by the tool is saved.
pub(super) fn observe(root: &Path, task: &str, workspace: &Path, args: &Value, result: &Value) -> Result<Option<Value>> {
    let Some(raw) = args["path"].as_str() else { return Ok(None); };
    let Some(hash) = result["code_hash"].as_str() else { return Ok(None); };
    // Reads may use accessible absolute paths, but only workspace files can be
    // edited, so only they need a baseline. The read itself already succeeded.
    let (work, rel, path) = match key(workspace, raw) {
        Ok(key) => key,
        Err(error) if matches!(error.downcast_ref::<crate::tools::ToolError>(), Some(crate::tools::ToolError::OutsideWorkspace(_))) => return Ok(None),
        Err(error) => return Err(error),
    };
    let current = crate::tools::file_edit::snapshot(&path)?;
    if current.as_deref().map(crate::index::symbol_description::content_hash).as_deref() != Some(hash) {
        return Ok(Some(json!({"error_code":"ASSET_CONFLICT", "path":rel, "workspace":work,
            "message":"The file changed while it was being read, probably by someone else. No edit was made; read it again before editing it."})));
    }
    save(root, task, &work, &rel, current.as_deref(), false)?;
    Ok(None)
}

pub(super) struct Prepared {
    workspace: String,
    relative: String,
    target: PathBuf,
    base: Option<Vec<u8>>,
    proposed: Vec<u8>,
    request_only: bool,
    pub args: Value,
}

struct WriterConflict {
    owner: String,
    turn: usize,
    seq: i64,
    metadata: Option<Value>,
}

/// Compute the intended content from the saved baseline, never from a conflicting file.
/// After an external change the edit may no longer fit the baseline; the request
/// itself is then the candidate, and the caller stops on the conflict.
pub(super) fn prepare(root: &Path, task: &str, workspace: &Path, name: &str, mut args: Value) -> Result<Prepared> {
    let raw = args["path"].as_str().context("file path required")?;
    let (work, rel, path) = key(workspace, raw)?;
    let base = match baseline(root, task, &work, &rel)? {
        Some(base) => base,
        None => {
            let current = crate::tools::file_edit::snapshot(&path)?;
            save(root, task, &work, &rel, current.as_deref(), false)?;
            baseline(root, task, &work, &rel)?.context("file baseline missing")?
        }
    };
    let changed = crate::tools::file_edit::snapshot(&path)?.as_deref() != base.as_deref();
    let (proposed, request_only) = match proposed_content(name, &args, base.as_deref()) {
        Ok(bytes) => (bytes, false),
        Err(_) if changed => (serde_json::to_vec_pretty(&args)?, true),
        Err(error) => return Err(error),
    };
    let object = args.as_object_mut().context("file arguments must be an object")?;
    object.insert("workspace_root".into(), json!(work.clone()));
    object.insert("path".into(), json!(rel.clone()));
    object.remove("expected_code_hash");
    if let Some(bytes) = &base {
        object.insert("expected_code_hash".into(), json!(crate::index::symbol_description::content_hash(bytes)));
    }
    Ok(Prepared { workspace: work, relative: rel, target: path, base, proposed, request_only, args })
}

fn proposed_content(name: &str, args: &Value, base: Option<&[u8]>) -> Result<Vec<u8>> {
    Ok(match name {
        "write_file" => args["content"].as_str().context("content required")?.as_bytes().to_vec(),
        "edit_file" => {
            let source = std::str::from_utf8(base.context("cannot edit a missing baseline")?)?;
            let edits: Vec<crate::tools::TextEdit> = serde_json::from_value(args["edits"].clone())?;
            crate::tools::file_edit::text_edits(source, &edits)?.into_bytes()
        }
        "replace_range" => {
            let source = std::str::from_utf8(base.context("cannot edit a missing baseline")?)?;
            let start = args["start_line"].as_u64().context("start_line required")? as usize;
            let end = args["end_line"].as_u64().context("end_line required")? as usize;
            let replacement = args["replacement"].as_str().context("replacement required")?;
            crate::tools::file_edit::line_replacement(source, start, end, replacement, args["expected_old_text"].as_str())?.into_bytes()
        }
        _ => anyhow::bail!("unsupported managed file operation"),
    })
}

impl Prepared {
    /// A whole-file write by a function that started at version `started` may
    /// erase a change it never saw: another writer's version above that.
    /// The host does not record what a function read, so a later read does not
    /// lift this. Writing back the current content cannot lose anything.
    pub(super) fn parallel_conflict(&self, root: &Path, task: &str, owner: &str, started: i64) -> Result<bool> {
        Ok(self.conflicting_writer(root, task, owner, started)?.is_some())
    }

    fn conflicting_writer(&self, root: &Path, task: &str, owner: &str, started: i64) -> Result<Option<WriterConflict>> {
        if !self.request_only && self.base.as_deref() == Some(self.proposed.as_slice()) { return Ok(None); }
        let row = connection(root)?.query_row(
            "SELECT v.owner,v.turn,v.seq,m.metadata FROM agent_file_writer_versions v
             LEFT JOIN agent_file_writer_metadata m ON m.task_id=v.task_id AND m.workspace=v.workspace AND m.path=v.path AND m.owner=v.owner AND m.turn=v.turn AND m.seq=v.seq
             WHERE v.task_id=?1 AND v.workspace=?2 AND v.path=?3 AND v.owner<>?4 AND v.seq>?5
             ORDER BY CASE WHEN v.owner GLOB '@shared_shell:*' THEN 0 ELSE 1 END,v.seq DESC LIMIT 1",
            params![task, self.workspace, self.relative, owner, started], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?, row.get::<_, i64>(2)?, row.get::<_, Option<String>>(3)?))
            },
        ).optional()?;
        row.map(|(owner, turn, seq, metadata)| -> Result<WriterConflict> {
            Ok(WriterConflict { owner, turn, seq, metadata: metadata.map(|metadata| serde_json::from_str(&metadata)).transpose()? })
        }).transpose()
    }

    /// Preserve the proposal and diagnose shared provenance without inventing
    /// a file author. Participants retain the same version barrier as outsiders.
    pub(super) fn parallel_conflict_result(&self, root: &Path, task: &str, owner: &str, started: i64) -> Result<Option<Value>> {
        let Some(writer) = self.conflicting_writer(root, task, owner, started)? else { return Ok(None) };
        let shared = writer.owner.starts_with("@shared_shell:");
        let mut result = self.preserve_versions(root, None, if shared { "shared_interval" } else { "parallel" })?;
        result["writer_owner"] = json!(writer.owner);
        result["writer_turn"] = json!(writer.turn);
        result["writer_seq"] = json!(writer.seq);
        if let Some(metadata) = writer.metadata {
            result["shell_interval"] = metadata;
        } else if let Some(id) = writer.owner.strip_prefix("@shared_shell:") {
            result["shell_interval"] = json!({"kind":"shared_shell_interval","interval_id":id,
                "owners":[],"writer":writer.owner,"attribution":"unknown","shared":true});
        }
        result["message"] = json!(if shared {
            "The file changed while several commands ran together, so who changed it is unknown. Nothing was overwritten. Look at the current file and decide: apply your change to it with edit or replace, or ask the user."
        } else {
            "Another function wrote this file after this function started, so writing the whole file would erase its change. Nothing was overwritten. Apply your change to the current content with edit or replace instead, or return and let the caller decide."
        });
        Ok(Some(result))
    }
    pub(super) fn record_writer(&self, root: &Path, task: &str, owner: &str, turn: usize) -> Result<()> {
        record_writer(root, task, &self.workspace, &self.relative, owner, turn)
    }
    pub(super) fn conflict(&self) -> Result<bool> {
        Ok(crate::tools::file_edit::snapshot(&self.target)?.as_deref() != self.base.as_deref())
    }
    /// The file changed outside the task: nothing is overwritten and the turn
    /// goes on. The AI gets what changed and decides; the task's record now
    /// follows the file it was shown, so its next edit applies to that.
    pub(super) fn preserve(&self, root: &Path, task: &str) -> Result<Value> {
        let mut result = self.preserve_versions(root, Some(self.base.as_deref()), "external")?;
        let current = crate::tools::file_edit::snapshot(&self.target)?;
        result["external_change"] = change_summary(self.base.as_deref(), current.as_deref());
        save(root, task, &self.workspace, &self.relative, current.as_deref(), true)?;
        Ok(result)
    }
    pub(super) fn preserve_versions(&self, root: &Path, base: Option<Option<&[u8]>>, reason: &str) -> Result<Value> {
        let dir = root.join(".codex-workspace-mcp").join("conflicts");
        fs::create_dir_all(&dir)?;
        let suffix = self.target.file_name().context("file name required")?.to_string_lossy();
        let (kind, extension) = if self.request_only { ("request", ".request.json") } else { ("file", "") };
        let id = crate::server::agent_service::uuid_like();
        let path = dir.join(format!("{}-{}{}", id, suffix, extension));
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        std::io::Write::write_all(&mut file, &self.proposed)?;
        file.sync_all()?;
        let current = crate::tools::file_edit::snapshot(&self.target)?;
        let current_path = if let Some(bytes) = current.as_deref() {
            let saved = dir.join(format!("{}-current-{}", id, suffix));
            fs::write(&saved, bytes)?; Some(saved.to_string_lossy().into_owned())
        } else { None };
        let base_path = if let Some(Some(bytes)) = base {
            let saved = dir.join(format!("{}-base-{}", id, suffix));
            fs::write(&saved, bytes)?; Some(saved.to_string_lossy().into_owned())
        } else { None };
        let preview = |bytes: Option<&[u8]>| -> Value {
            match bytes.and_then(|b| std::str::from_utf8(b).ok()) {
                Some(text) => { let mut chars = text.chars(); let clipped: String = chars.by_ref().take(2000).collect(); json!({"text":clipped,"truncated":chars.next().is_some()}) },
                None => Value::Null,
            }
        };
        Ok(json!({"error_code":"ASSET_CONFLICT", "path":self.relative,
            "workspace":self.workspace,"candidate":kind,"candidate_path":path.to_string_lossy(),
            "reason":reason, "base_known":base.is_some(), "base_exists":base.flatten().is_some(), "base_path":base_path,
            "current_exists":current.is_some(), "current_path":current_path,
            "base_preview":preview(base.flatten()), "current_preview":preview(current.as_deref()), "candidate_preview":preview(Some(&self.proposed)),
            "message":"This file changed outside this task since you last read or wrote it, perhaps by the user or another agent; external_change shows what changed. Your change was not applied and nothing was overwritten. Decide how to go on: apply your change to the current content (it is now the version your next edit works on), adapt it to what changed, or ask the user if the two changes contradict each other. Your proposed version is saved at candidate_path."}))
    }
    pub(super) fn committed(&self, root: &Path, task: &str) -> Result<bool> {
        crate::tools::workspace_changes::invalidate_snapshot_cache(Path::new(&self.workspace));
        let current = crate::tools::file_edit::snapshot(&self.target)?;
        if current.as_deref() != Some(self.proposed.as_slice()) {
            return Ok(false);
        }
        save(root, task, &self.workspace, &self.relative, current.as_deref(), true)?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "tests/managed_files_tests.rs"]
mod tests;

pub(super) fn guard_batch(root: &Path, task: &str, workspace: &Path, args: &mut Value) -> Result<()> {
    for item in args["files"].as_array_mut().context("batch requires files")? {
        let raw = item["path"].as_str().context("batch requires file paths")?;
        let (work, relative, path) = key(workspace, raw)?;
        if !path.is_file() || std::fs::metadata(&path)?.len() > 2 * 1024 * 1024 {
            continue; // The batch preflight reports invalid files with their paths.
        }
        let current = crate::tools::file_edit::snapshot(&path)?;
        let base = baseline(root, task, &work, &relative)?;
        if let Some(base) = &base {
            anyhow::ensure!(current.as_deref() == base.as_deref(), "write_conflict: {relative} changed outside this task; read it again before applying the batch");
        }
        if item["expected_code_hash"].is_null() && let Some(bytes) = current {
            item["expected_code_hash"] = json!(crate::index::symbol_description::content_hash(&bytes));
        }
    }
    Ok(())
}
