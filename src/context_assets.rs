//! Durable, content-addressed material references. Snapshots remain immutable;
//! working views use handles and restore complete requested or referenced data.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::{Read, Write}, path::Path};

pub const MAX_PAGE_CHARS: usize = 24_000;

#[cfg(test)]
mod streaming_asset_tests {
    use super::*;
    use std::io::{BufWriter, Seek, SeekFrom};
    use std::path::PathBuf;

    struct Fixture { root: PathBuf, seq: i64 }
    impl Fixture {
        fn new() -> Result<Self> {
            let root = std::env::temp_dir().join(format!("streaming-assets-{}", crate::agent_service::uuid_like()));
            std::fs::create_dir_all(&root)?;
            let mut conn = crate::agent_service::open_db(&root)?;
            conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('asset-task','','','completed',0,0)", [])?;
            let tx = conn.transaction()?;
            let seq = crate::agent_service::append_event_tx(&tx, "asset-task", "shell/output", &json!({}), None)?;
            tx.commit()?;
            Ok(Self { root, seq })
        }
        fn register(&self, snapshot: &Path) -> Result<Value> {
            register_file(&self.root, "asset-task", self.seq, "shell_output", &json!({"session_id":"shell-test"}), snapshot)
        }
        fn read(&self, reference: &Value, offset: usize, limit: Option<usize>) -> Result<Value> {
            let mut args = json!({"asset_id":reference["asset_id"],"char_offset":offset});
            if let Some(limit) = limit { args["max_chars"] = json!(limit); }
            read(&self.root, "asset-task", &args, None)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
    }

    #[test]
    fn large_shell_snapshot_pages_unicode_and_verifies_the_entire_file() -> Result<()> {
        let f = Fixture::new()?;
        let snapshot = f.root.join("output.tmp");
        let prefix = "{\"stdout\":\"";
        let suffix = "\",\"stderr\":\"\",\"encoding\":{\"stdout\":\"utf-8\"}}";
        let mut file = BufWriter::new(std::fs::File::create(&snapshot)?);
        file.write_all(prefix.as_bytes())?;
        for _ in 0..100_000 { file.write_all("中🙂a".as_bytes())?; }
        file.write_all(suffix.as_bytes())?;
        file.flush()?;
        drop(file);
        let reference = f.register(&snapshot)?;
        assert_eq!(reference["read_reference"]["arguments"]["max_chars"], MAX_PAGE_CHARS);
        assert_eq!(reference["bytes"], std::fs::metadata(&snapshot)?.len());
        let total = prefix.chars().count() + 300_000 + suffix.chars().count();
        let first = f.read(&reference, 0, None)?;
        assert_eq!(first["returned_chars"], MAX_PAGE_CHARS);
        assert_eq!(first["total_chars"], total);
        assert_eq!(first["next_char_offset"], MAX_PAGE_CHARS);
        assert!(first.get("original_result").is_none());
        assert!(first["content"].as_str().unwrap().starts_with(prefix));
        let offset = prefix.chars().count() + 3 * 77_777 + 1;
        let page = f.read(&reference, offset, Some(8))?;
        assert_eq!(page["content"], "🙂a中🙂a中🙂a");
        assert_eq!(page["next_char_offset"], offset + 8);
        let tail = f.read(&reference, total - 20, Some(20))?;
        assert_eq!(tail["content"], suffix.chars().skip(suffix.chars().count() - 20).collect::<String>());
        assert!(tail["next_char_offset"].is_null());
        assert_eq!(f.read(&reference, total, None)?["returned_chars"], 0);
        assert!(f.read(&reference, total + 1, None).is_err());
        assert!(f.read(&reference, 0, Some(0)).is_err());
        assert_eq!(f.read(&reference, 0, Some(MAX_PAGE_CHARS + 1))?["returned_chars"], MAX_PAGE_CHARS);
        assert!(read(&f.root, "asset-task", &json!({"asset_id":reference["asset_id"],"char_offset":-1}), None).is_err());
        // Alter a byte beyond the requested first page: hashing cannot stop
        // once enough characters for the page have been collected.
        let published = f.root.join(reference["snapshot_path"].as_str().unwrap());
        let mut file = std::fs::OpenOptions::new().write(true).open(published)?;
        file.seek(SeekFrom::End(-1))?;
        file.write_all(b"\xff")?;
        drop(file);
        assert!(f.read(&reference, 0, Some(1)).unwrap_err().to_string().contains("ASSET_CONTENT_CHANGED"));
        Ok(())
    }

    #[test]
    fn file_registration_preserves_identity_and_publishes_an_independent_immutable_blob() -> Result<()> {
        let f = Fixture::new()?;
        let snapshot = f.root.join("output.tmp");
        let value = json!({"stdout":"中文🙂","stderr":"","encoding":{"stdout":"utf-8"}});
        let bytes = serde_json::to_vec(&value)?;
        std::fs::write(&snapshot, &bytes)?;
        let reference = f.register(&snapshot)?;
        assert_eq!(reference["content_hash"], crate::visual_artifacts::hash(&bytes));
        let ordinary = register(&f.root, "asset-task", f.seq, "shell_output", &json!({"session_id":"shell-test"}), &value)?;
        assert_eq!(ordinary, reference);
        assert_eq!(f.register(&snapshot)?, reference);
        std::fs::write(&snapshot, b"caller reused its temporary file")?;
        assert_eq!(f.read(&reference, 0, None)?["original_result"], value);
        assert!(read(&f.root, "foreign-task", &json!({"asset_id":reference["asset_id"]}), None).is_err());
        assert!(read(&f.root, "asset-task", &json!({"asset_id":reference["asset_id"]}), Some(f.seq - 1)).is_err());
        let conn = connection(&f.root)?;
        let count: i64 = conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='asset/registered'", [], |r| r.get(0))?;
        assert_eq!(count, 1);
        drop(conn);
        std::fs::write(f.root.join(reference["snapshot_path"].as_str().unwrap()), b"damaged")?;
        std::fs::write(&snapshot, &bytes)?;
        assert!(f.register(&snapshot).unwrap_err().to_string().contains("incomplete or changed"));
        Ok(())
    }

    #[test]
    fn other_assets_keep_complete_unbounded_delivery_when_no_page_is_requested() -> Result<()> {
        let f = Fixture::new()?;
        let value = json!({"content":"x".repeat(30_000)});
        let reference = register(&f.root, "asset-task", f.seq, "read_file", &json!({"path":"example.txt"}), &value)?;
        let restored = f.read(&reference, 0, None)?;
        assert_eq!(restored["original_result"], value);
        assert!(restored.get("content").is_none());
        Ok(())
    }

    #[test]
    fn snapshot_restores_frozen_file_after_workspace_changes() -> Result<()> {
        let f = Fixture::new()?;
        let value = json!({"path":"example.txt","content":"alpha\nbeta\ngamma\n","complete":true,"bytes":17,"total_lines":3});
        let reference = register(&f.root, "asset-task", f.seq, "read_file", &json!({"path":"example.txt"}), &value)?;
        let restored = snapshot(&f.root, "asset-task", &json!({"asset_id":reference["asset_id"]}))?;
        assert_eq!(restored["content"], "alpha\nbeta\ngamma\n");
        assert!(restored["__ref"].as_str().is_some_and(|id| id.starts_with("Ref#asset-")));
        assert_eq!(restored["content"].as_str().unwrap().lines().nth(1), Some("beta"));
        Ok(())
    }
}

pub fn rewind(tx: &rusqlite::Transaction, task: &str, start: i64) -> Result<()> {
    for (table, predicate) in [
        (
            "agent_context_assets",
            "json_extract(reference_json,'$.history_reference.event_seq')>=?2",
        ),
        ("agent_context_asset_deliveries", "event_seq>=?2"),
        (
            "agent_context_materials",
            "json_extract(data,'$.through')>=?2",
        ),
    ] {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if exists {
            tx.execute(
                &format!("DELETE FROM {table} WHERE task_id=?1 AND {predicate}"),
                params![task, start],
            )?;
        }
    }
    Ok(())
}

fn connection(root: &Path) -> Result<Connection> {
    let conn = crate::agent_service::open_db(root)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_context_assets(
        task_id TEXT NOT NULL, asset_id TEXT NOT NULL, reference_json TEXT NOT NULL,
        source_tool TEXT NOT NULL, source_arguments TEXT NOT NULL,
        payload_path TEXT NOT NULL, payload_hash TEXT NOT NULL,
        PRIMARY KEY(task_id,asset_id));
        CREATE TABLE IF NOT EXISTS agent_context_asset_deliveries(
        task_id TEXT NOT NULL,asset_id TEXT NOT NULL,event_seq INTEGER NOT NULL,
        PRIMARY KEY(task_id,asset_id,event_seq));",
    )?;
    Ok(conn)
}

pub fn is_material_tool(name: &str) -> bool {
    matches!(
        name,
        "read_attachment"
            | "list_dir"
            | "read_file"
            | "parallel_read_batch"
            | "read_file_lines"
            | "read_file_chunk"
            | "search_text"
            | "search_code_map"
            | "read_code_flow"
            | "read_skill"
            | "browser_read"
            | "browser_screenshot"
            | "view_image"
            | "analyze_images"
            | "browser_diagnostics"
            | "run_program"
            | "exec_command"
            | "run_readonly_command"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "write_stdin"
            | "run_project_script"
            | "get_project_process"
            | "http_probe"
    ) || (name.starts_with("read_") && name.ends_with("_symbol"))
        || name.contains("__") // Plugin results obey the same asset protocol.
        || name == "read_session_history"
}

/// Called after the unchanged original result has been committed to history.
/// Repeating the same read produces the same reference, not another identity.
pub fn register(
    root: &Path,
    task: &str,
    seq: i64,
    tool: &str,
    args: &Value,
    result: &Value,
) -> Result<Value> {
    let image_id = result["artifact_id"].as_str().or_else(|| {
        result
            .pointer("/visual_artifact/artifact_id")
            .and_then(Value::as_str)
    });
    let image = image_id
        .map(|id| crate::visual_artifacts::metadata(root, task, id))
        .transpose()?;
    let payload = image.as_ref().unwrap_or(result);
    let bytes = serde_json::to_vec(payload)?;
    let payload_hash = crate::visual_artifacts::hash(&bytes);
    let identity = if let Some(id) = image_id {
        json!({"task":task,"image":id})
    } else {
        json!({"task":task,"tool":tool,"arguments":args,"payload_hash":payload_hash})
    };
    let id = format!(
        "asset-{}",
        crate::visual_artifacts::hash(identity.to_string().as_bytes())
    );
    let mut conn = connection(root)?;
    if let Some(reference) = lookup(&conn, task, &id)? {
        return Ok(reference);
    }
    let relative = format!(".codex-workspace-mcp/assets/{payload_hash}.json");
    let path = crate::file_edit::workspace_path(root, &relative)?;
    std::fs::create_dir_all(path.parent().context("asset directory missing")?)?;
    // Publish a complete immutable blob. A crash while writing a temporary
    // file must not leave a truncated file at the permanent reference path.
    let temporary = path.with_extension(format!("{}.tmp", crate::agent_service::uuid_like()));
    let stored = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    crate::visual_artifacts::hash(&std::fs::read(&path)?) == payload_hash,
                    "asset file is incomplete or changed: {id}"
                );
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    })();
    let _ = std::fs::remove_file(&temporary);
    stored?;
    let source_path = result["path"]
        .as_str()
        .or_else(|| result["file_path"].as_str())
        .or_else(|| result.pointer("/data/path").and_then(Value::as_str))
        .or_else(|| result.pointer("/result/data/path").and_then(Value::as_str))
        .or_else(|| args["path"].as_str())
        .or_else(|| args["file_path"].as_str());
    let kind = if tool == "tool_arguments" {
        "tool_arguments"
    } else if image.is_some() {
        "image"
    } else if source_path.is_some()
        && (matches!(tool, "read_file" | "read_file_lines")
            || (tool.starts_with("read_") && tool.ends_with("_symbol")))
    {
        "file_snapshot"
    } else {
        "tool_output"
    };
    let mut reference = json!({"asset_id":id,"kind":kind,"content_hash":payload_hash,"bytes":bytes.len(),
        "snapshot_path":relative,"source_tool":tool,
        "history_reference":{"task_id":task,"event_seq":seq,"include_event":true},
        "read_reference":{"tool":"read_asset","arguments":{"asset_id":id}}});
    if tool == "shell_output" {
        reference["read_reference"]["arguments"]["max_chars"] = json!(MAX_PAGE_CHARS);
    }
    for key in [
        "project_id",
        "page_index",
        "revision",
        "expected_revision",
        "status",
        "task_id",
        "agent_id",
        "name",
    ] {
        if let Some(value) = result
            .get(key)
            .or_else(|| result.pointer(&format!("/data/{key}")))
            .or_else(|| result.pointer(&format!("/result/data/{key}")))
            .or_else(|| args.get(key))
            .filter(|v| !v.is_null() && !v.is_object() && !v.is_array())
        {
            reference[key] = value.clone();
        }
    }
    if tool == "tool_arguments" {
        reference["operation"] = result["tool_calls"][0]["function"]["name"].clone();
    }
    if tool == "read_skill" && result["skill"].is_string() {
        reference["skill"] = result["skill"].clone();
    }
    if let Some(paths) = args["paths"].as_array() {
        reference["source_paths"] = json!(paths);
    }
    if let Some(path) = source_path {
        reference["source_path"] = json!(path);
        reference["source_workspace"] = args
            .get("workspace_root")
            .filter(|v| v.is_string())
            .cloned()
            .unwrap_or_else(|| json!(root.to_string_lossy()));
        reference["source_version"] = result
            .get("code_hash")
            .or_else(|| result.get("content_hash"))
            .cloned()
            .unwrap_or(Value::Null);
        reference["source_range"] =
            json!({"start_line":result["start_line"],"end_line":result["end_line"]});
    }
    if let Some(image) = image {
        reference["artifact_id"] = image["artifact_id"].clone();
        reference["image_path"] = image["workspace_relative_path"].clone();
        reference["captured_at"] = image["captured_at"].clone();
    }
    if matches!(tool, "read_file" | "read_file_lines") {
        reference["refresh_reference"] =
            json!({"tool":"refresh_asset","arguments":{"asset_id":id}});
    }
    save_reference(&mut conn, task, seq, tool, args, reference)
}

/// Register a payload before its tool/result event exists. The caller must
/// rebind the returned asset to that result event once it has its final seq.
pub fn register_pending(
    root: &Path,
    task: &str,
    tool: &str,
    args: &Value,
    result: &Value,
) -> Result<Value> {
    register(root, task, -1, tool, args, result)
}


/// Register an already completed UTF-8 JSON snapshot without loading its body.
/// Copy into our own temporary inode so caller cleanup, reuse, or a different
/// source volume cannot change the immutable published asset.
pub(crate) fn register_file(
    root: &Path,
    task: &str,
    seq: i64,
    tool: &str,
    args: &Value,
    snapshot: &Path,
) -> Result<Value> {
    let directory = crate::file_edit::workspace_path(root, ".codex-workspace-mcp/assets")?;
    std::fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!("{}.tmp", crate::agent_service::uuid_like()));
    let result = (|| -> Result<Value> {
        let mut source = std::fs::File::open(snapshot).context("asset snapshot unavailable")?;
        let mut stored = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        let (payload_hash, bytes) = copy_snapshot(&mut source, &mut stored)?;
        stored.sync_all()?;
        drop(stored);
        let relative = format!(".codex-workspace-mcp/assets/{payload_hash}.json");
        let path = crate::file_edit::workspace_path(root, &relative)?;
        let identity = json!({"task":task,"tool":tool,"arguments":args,"payload_hash":payload_hash});
        let id = format!("asset-{}", crate::visual_artifacts::hash(identity.to_string().as_bytes()));
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let (existing, _) = copy_snapshot(&mut std::fs::File::open(&path)?, &mut std::io::sink())?;
                ensure!(existing == payload_hash, "asset file is incomplete or changed: {id}");
            }
            Err(error) => return Err(error.into()),
        }
        let mut reference = json!({
            "asset_id":id,"kind":"tool_output","content_hash":payload_hash,"bytes":bytes,
            "snapshot_path":relative,"source_tool":tool,
            "history_reference":{"task_id":task,"event_seq":seq,"include_event":true},
            "read_reference":{"tool":"read_asset","arguments":{"asset_id":id}}
        });
        if tool == "shell_output" {
            reference["read_reference"]["arguments"]["max_chars"] = json!(MAX_PAGE_CHARS);
        }
        for key in ["project_id", "page_index", "revision", "expected_revision", "status", "task_id", "agent_id", "name"] {
            if let Some(value) = args.get(key).filter(|v| !v.is_null() && !v.is_object() && !v.is_array()) {
                reference[key] = value.clone();
            }
        }
        if let Some(paths) = args["paths"].as_array() {
            reference["source_paths"] = json!(paths);
        }
        save_reference(&mut connection(root)?, task, seq, tool, args, reference)
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

fn copy_snapshot(reader: &mut impl Read, writer: &mut impl Write) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 { break; }
        writer.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok((format!("{:x}", hash.finalize()), bytes))
}

fn save_reference(
    conn: &mut Connection, task: &str, seq: i64, tool: &str, args: &Value, mut reference: Value,
) -> Result<Value> {
    let id = reference["asset_id"].as_str().context("asset id missing")?.to_owned();
    let relative = reference["snapshot_path"].as_str().context("snapshot path missing")?.to_owned();
    let payload_hash = reference["content_hash"].as_str().context("content hash missing")?.to_owned();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let owner: Option<String> = tx
        .query_row(
            "SELECT COALESCE(node_id,'n0') FROM agent_task_events WHERE task_id=?1 AND seq=?2",
            params![task, seq],
            |r| r.get(0),
        )
        .optional()?;
    reference["task_node_id"] = json!(owner);
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO agent_context_assets VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            task,
            id,
            reference.to_string(),
            tool,
            args.to_string(),
            relative,
            payload_hash
        ],
    )?;
    if inserted == 1 {
        crate::agent_service::append_event_tx(
            &tx,
            task,
            "asset/registered",
            &json!({"reference":reference,"task_node_id":owner}),
            None,
        )?;
    }
    let saved = lookup(&tx, task, &id)?.context("registered asset missing")?;
    tx.commit()?;
    Ok(saved)
}
fn lookup(conn: &Connection, task: &str, id: &str) -> Result<Option<Value>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT reference_json FROM agent_context_assets WHERE task_id=?1 AND asset_id=?2",
            params![task, id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|text| serde_json::from_str(&text).map_err(Into::into))
        .transpose()
}
pub fn reference(root: &Path, task: &str, id: &str, through: Option<i64>) -> Result<Value> {
    let result = lookup(&connection(root)?, task, id)?
        .context("ASSET_NOT_FOUND: use the asset_id supplied by this task")?;
    ensure!(
        through.is_none_or(|limit| result["history_reference"]["event_seq"]
            .as_i64()
            .is_some_and(|seq| seq <= limit)),
        "asset is beyond the frozen history boundary"
    );
    Ok(result)
}




/// Load handles once for assembling a child task; asset bodies stay on disk.
/// Always read the exact saved version. A live re-read requires refresh_asset.
pub fn read(root: &Path, task: &str, args: &Value, through: Option<i64>) -> Result<Value> {
    let id = args["asset_id"].as_str().context("asset_id is required")?;
    let reference = reference(root, task, id, through)?;
    ensure!(
        reference["storage"] != "plugin",
        "REMOTE_ASSET: read through the authenticated plugin resource adapter"
    );
    let path = crate::file_edit::workspace_path(
        root,
        reference["snapshot_path"]
            .as_str()
            .context("snapshot path missing")?,
    )?;
    if reference["source_tool"] == "shell_output" {
        return read_shell_snapshot(&reference, &path, args);
    }
    let bytes = std::fs::read(path)
        .context("asset snapshot unavailable; original history_reference is retained")?;
    ensure!(
        crate::visual_artifacts::hash(&bytes) == reference["content_hash"].as_str().unwrap_or(""),
        "ASSET_CONTENT_CHANGED: saved snapshot does not match its hash; use original history_reference"
    );
    saved_delivery(&reference, &String::from_utf8(bytes)?, args)
}

/// Restore an immutable snapshot by asset id without consulting its source path.
pub fn snapshot(root: &Path, task: &str, args: &Value) -> Result<Value> {
    let asset_id = args["asset_id"].as_str().context("asset_id is required")?;
    let restored = read(root, task, args, None)?;
    let value = restored.get("original_result").cloned().unwrap_or(restored);
    if let Value::String(content) = value {
        return Ok(json!({
            "__ref": format!("Ref#{asset_id}"),
            "asset_id": asset_id,
            "content": content,
            "complete": true,
            "version": "saved_snapshot",
        }));
    }
    if let Some(object) = value.as_object()
        && (object.get("path").and_then(Value::as_str).is_some()
            && object.get("content").and_then(Value::as_str).is_some()
            || object.get("exit_code").is_some())
    {
        let mut handle = object.clone();
        handle.insert("__ref".into(), json!(format!("Ref#{asset_id}")));
        handle.insert("asset_id".into(), json!(asset_id));
        return Ok(Value::Object(handle));
    }
    Ok(value)
}

fn read_shell_snapshot(reference: &Value, path: &Path, args: &Value) -> Result<Value> {
    let offset = match args.get("char_offset") {
        Some(value) => usize::try_from(value.as_u64().context("char_offset must be a nonnegative integer")?)?,
        None => 0,
    };
    let limit = match args.get("max_chars") {
        Some(value) => value.as_u64().context("max_chars must be an integer")?,
        None => MAX_PAGE_CHARS as u64,
    };
    ensure!(limit >= 1, "max_chars must be at least 1");
    let limit = (limit as usize).min(MAX_PAGE_CHARS);
    let mut file = std::fs::File::open(path)
        .context("asset snapshot unavailable; original history_reference is retained")?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut pending = Vec::with_capacity(buffer.len() + 4);
    let mut content = String::new();
    let mut total = 0usize;
    let mut returned = 0usize;
    let mut decoding_error = None;
    loop {
        let count = file.read(&mut buffer)?;
        hash.update(&buffer[..count]);
        if decoding_error.is_some() {
            if count == 0 { break; }
            continue;
        }
        pending.extend_from_slice(&buffer[..count]);
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) if count > 0 && error.error_len().is_none() => error.valid_up_to(),
            Err(error) => {
                decoding_error = Some(error);
                pending.clear();
                if count == 0 { break; }
                continue;
            }
        };
        for ch in std::str::from_utf8(&pending[..valid])?.chars() {
            if total >= offset && returned < limit {
                content.push(ch);
                returned += 1;
            }
            total += 1;
        }
        pending.drain(..valid);
        if count == 0 { break; }
    }
    ensure!(format!("{:x}", hash.finalize()) == reference["content_hash"].as_str().unwrap_or(""),
        "ASSET_CONTENT_CHANGED: saved snapshot does not match its hash; use original history_reference");
    if let Some(error) = decoding_error { return Err(error.into()); }
    ensure!(offset <= total, "asset char_offset exceeds total_chars={total}");
    let end = offset + returned;
    let mut result = json!({"asset_reference":reference,"content":content,"char_offset":offset,"total_chars":total,
        "returned_chars":returned,"next_char_offset":if end<total{Some(end)}else{None},"version":"saved_snapshot"});
    if offset == 0 && end == total {
        result["original_result"] = serde_json::from_str::<Value>(&content)?;
        result.as_object_mut().unwrap().remove("content");
    }
    Ok(result)
}
pub fn remote_delivery(reference: &Value, bytes: &[u8], args: &Value) -> Result<Value> {
    ensure!(
        crate::visual_artifacts::hash(bytes) == reference["content_hash"].as_str().unwrap_or(""),
        "REMOTE_ASSET_VERSION_CHANGED: link no longer contains the referenced SVG version; inspect the original history_reference or explicitly read the current file"
    );
    let text = std::str::from_utf8(bytes)?;
    let mut result = saved_delivery(reference,&json!({"path":reference["source_path"],"content":text,"content_hash":reference["content_hash"]}).to_string(),args)?;
    result["version"] = json!("verified_remote_content");
    Ok(result)
}
fn saved_delivery(reference: &Value, text: &str, args: &Value) -> Result<Value> {
    let offset = match args.get("char_offset") {
        Some(value) => value
            .as_u64()
            .context("char_offset must be a nonnegative integer")? as usize,
        None => 0,
    };
    let total = text.chars().count();
    ensure!(
        offset <= total,
        "asset char_offset exceeds total_chars={total}"
    );
    let limit = match args.get("max_chars") {
        Some(value) => {
            let limit = value.as_u64().context("max_chars must be an integer")?;
            ensure!(
                limit >= 1,
                "max_chars must be at least 1; omit it for the complete saved result"
            );
            Some((limit as usize).min(MAX_PAGE_CHARS))
        }
        None => None,
    };
    let content: String = text
        .chars()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let end = offset + content.chars().count();
    let mut result = json!({"asset_reference":reference,"content":content,"char_offset":offset,"total_chars":total,
        "returned_chars":end-offset,"next_char_offset":if end<total{Some(end)}else{None},"version":"saved_snapshot"});
    if offset == 0 && end == total {
        result["original_result"] = serde_json::from_str(&text)?;
        result.as_object_mut().unwrap().remove("content");
    }
    Ok(result)
}
