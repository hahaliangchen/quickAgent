use rusqlite::{Connection, OptionalExtension, Result, params};
use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock}};

#[derive(PartialEq, Eq)]
struct DbIdentity {
    #[cfg(unix)]
    device_inode: (u64, u64),
    created: Option<std::time::SystemTime>,
}

fn identity(path: &Path) -> std::io::Result<DbIdentity> {
    // Stat only: do not open a second OS descriptor for a live SQLite inode.
    let metadata = path.metadata()?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(DbIdentity {
        #[cfg(unix)]
        device_inode: (metadata.dev(), metadata.ino()),
        created: metadata.created().ok(),
    })
}

#[derive(Default)]
struct Initialized {
    file: Option<DbIdentity>,
    base: bool,
    agent: bool,
}

type SchemaCache = Mutex<HashMap<PathBuf, Arc<Mutex<Initialized>>>>;
static SCHEMAS: OnceLock<SchemaCache> = OnceLock::new();

pub(crate) fn open_initialized(
    root: &Path,
    agent_schema: Option<fn(&Connection, &Path) -> anyhow::Result<()>>,
) -> anyhow::Result<Connection> {
    let directory = root.join(".codex-workspace-mcp");
    std::fs::create_dir_all(&directory)?;
    let path = directory.canonicalize()?.join("codex_state.db");
    let path = path.canonicalize().unwrap_or(path);
    let slot = SCHEMAS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
        .entry(path.clone()).or_default().clone();
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    let before = identity(&path).ok();
    let mut conn = Connection::open(&path)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let file = identity(&path)?;
    anyhow::ensure!(before.as_ref().is_none_or(|previous| previous == &file), "database replaced while opening; retry");
    if before.is_none() || slot.file.as_ref() != Some(&file) {
        *slot = Initialized { file: Some(file), ..Default::default() };
    }
    if !slot.base {
        let mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
            anyhow::ensure!(mode.eq_ignore_ascii_case("wal"), "could not enable SQLite WAL");
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        base_schema(&tx)?;
        tx.commit()?;
        slot.base = true;
    }
    if let Some(schema) = agent_schema.filter(|_| !slot.agent) {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        schema(&tx, root)?;
        tx.commit()?;
        slot.agent = true;
    }
    Ok(conn)
}

/// Called with the schema transaction held; all identifiers are internal constants.
pub(crate) fn add_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
        [table, column], |row| row.get(0),
    )?;
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"), [])?;
    }
    Ok(!exists)
}

tokio::task_local! {
    static ALLOW_PROJECT_ENV_READ: bool;
}
thread_local! {
    static THREAD_ALLOW_PROJECT_ENV_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn allow_project_env_read() -> bool {
    ALLOW_PROJECT_ENV_READ.try_with(|v| *v).unwrap_or(false)
        || THREAD_ALLOW_PROJECT_ENV_READ.with(|c| c.get())
}

pub(crate) async fn with_allow_project_env_read<F: std::future::Future>(
    allow: bool,
    fut: F,
) -> F::Output {
    ALLOW_PROJECT_ENV_READ.scope(allow, fut).await
}

pub(crate) fn with_thread_allow_project_env_read<R>(allow: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            THREAD_ALLOW_PROJECT_ENV_READ.with(|c| c.set(self.0));
        }
    }
    let prev = THREAD_ALLOW_PROJECT_ENV_READ.with(|c| c.replace(allow));
    let _guard = Reset(prev);
    f()
}

/// Ordinary file I/O must never open SQLite's own state files. On Unix,
/// closing any descriptor for the DB/SHM inode releases this process's POSIX
/// locks, including locks held by SQLite on other threads. A subsequent
/// connection can then reset SHM while the writer still has it mapped.
pub(crate) fn check_source_access(path: &Path) -> std::io::Result<()> {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let state_dir = resolved.parent().and_then(Path::file_name).and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".codex-workspace-mcp"));
    let state_file = resolved.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
        ["codex_state.db", "codex_state.db-wal", "codex_state.db-shm", "codex_state.db-journal"]
            .iter().any(|reserved| name.eq_ignore_ascii_case(reserved))
    });
    if state_dir && state_file {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "SQLITE_STATE_FILE: host database files are accessed through SQLite, not source-file tools"));
    }
    if is_secret_file(&resolved, &crate::server::ai_proxy::secret_files()) {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "SECRET_FILE: this file holds API keys and is never readable by tools; the host applies the credentials itself. Model settings are in providers.yml and agent-generation.yml beside it"));
    }
    if is_generic_secret_path(&resolved) && !allow_project_env_read() {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "SECRET_FILE: environment and credential files are not accessible through source-file tools"));
    }
    Ok(())
}

/// Whether a resolved path is one of the secret files, compared after links
/// resolve so another spelling or a symlink reaches the same answer.
fn is_generic_secret_path(path: &Path) -> bool {
    let is_secret_component = |component: &std::ffi::OsStr| {
        let value = component.to_string_lossy();
        let value = if cfg!(windows) { value.to_ascii_lowercase() } else { value.into_owned() };
        value == ".ssh"
    };
    if path.components().any(|part| is_secret_component(part.as_os_str())) {
        return true;
    }
    let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else { return false; };
    let name = if cfg!(windows) { name.to_ascii_lowercase() } else { name.to_owned() };
    if name == ".env" || (name.starts_with(".env.") && !["example", "sample", "template"].iter().any(|suffix| name.ends_with(suffix))) {
        return true;
    }
    matches!(name.as_str(), "credentials" | "credentials.json" | "service-account.json" | "id_rsa" | "id_ecdsa" | "id_ed25519" | "id_dsa")
        || [".pem", ".key", ".p12", ".pfx"].iter().any(|suffix| name.ends_with(suffix))
}

fn is_secret_file(resolved: &Path, secrets: &[std::path::PathBuf]) -> bool {
    secrets.iter().any(|secret| {
        let secret = secret.canonicalize().unwrap_or_else(|_| secret.clone());
        if cfg!(windows) {
            secret.to_string_lossy().eq_ignore_ascii_case(&resolved.to_string_lossy())
        } else {
            secret == resolved
        }
    })
}

pub fn init_db(workspace_root: &Path) -> Result<Connection> {
    open_initialized(workspace_root, None).map_err(|error| {
        error.downcast::<rusqlite::Error>().unwrap_or_else(|error| {
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN), Some(error.to_string()))
        })
    })
}

fn base_schema(conn: &Connection) -> Result<()> {
    crate::index::symbol_index_state::schema(conn)?;

    // Create memories table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            time_unix INTEGER NOT NULL,
            workspace_root TEXT NOT NULL,
            summary TEXT NOT NULL,
            implementation TEXT NOT NULL,
            tests TEXT NOT NULL,
            risks TEXT NOT NULL,
            files_changed TEXT NOT NULL
        )",
        [],
    )?;
    // Older workspaces already have the original memories table.
    for (column, definition) in [
        ("source_task_id", "TEXT"),
        ("kind", "TEXT NOT NULL DEFAULT 'work'"),
        ("applies_to", "TEXT NOT NULL DEFAULT ''"),
        ("source_refs", "TEXT NOT NULL DEFAULT '[]'"),
    ] {
        add_column(conn, "memories", column, definition)?;
    }
    let legacy_memory_index: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='idx_memories_source_task'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if legacy_memory_index.is_some_and(|sql| !sql.contains("kind")) {
        conn.execute("DROP INDEX IF EXISTS idx_memories_source_task", [])?;
    }
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_source_task
         ON memories(workspace_root, source_task_id) WHERE source_task_id IS NOT NULL AND kind <> 'observer'",
        [],
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS observer_memory_sources (
        workspace_root TEXT NOT NULL, summary TEXT NOT NULL, implementation TEXT NOT NULL,
        sources TEXT NOT NULL, PRIMARY KEY(workspace_root,summary,implementation));",
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS architecture_memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_root TEXT NOT NULL,
            area TEXT NOT NULL,
            summary TEXT NOT NULL,
            key_symbols TEXT NOT NULL,
            key_files TEXT NOT NULL,
            boundaries TEXT NOT NULL,
            common_tasks TEXT NOT NULL,
            risks TEXT NOT NULL,
            created_at_unix INTEGER NOT NULL,
            updated_at_unix INTEGER NOT NULL,
            UNIQUE(workspace_root, area)
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_architecture_memories_workspace
         ON architecture_memories(workspace_root, updated_at_unix)",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS symbol_business_contexts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_root TEXT NOT NULL,
            symbol_id TEXT NOT NULL,
            symbol_name TEXT NOT NULL,
            language TEXT NOT NULL,
            file_path TEXT NOT NULL,
            belongs_to_area TEXT NOT NULL,
            business_role TEXT NOT NULL,
            common_tasks TEXT NOT NULL,
            read_when TEXT NOT NULL,
            avoid_when TEXT NOT NULL,
            risks TEXT NOT NULL,
            confidence REAL NOT NULL,
            updated_at_unix INTEGER NOT NULL,
            UNIQUE(workspace_root, symbol_id)
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_symbol_business_contexts_search
         ON symbol_business_contexts(workspace_root, symbol_name, belongs_to_area)",
        [],
    )?;

    crate::index::symbol_description::schema(conn)?;

    // Create rust_symbols table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS rust_symbols (
            id TEXT NOT NULL,
            workspace_root TEXT NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            file_path TEXT NOT NULL,
            module_path TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            signature TEXT NOT NULL,
            docstring TEXT NOT NULL,
            visibility TEXT NOT NULL,
            impl_type TEXT,
            trait_name TEXT,
            calls_json TEXT NOT NULL,
            PRIMARY KEY (workspace_root, id)
        )",
        [],
    )?;

    // Create go_symbols table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS go_symbols (
            id TEXT NOT NULL,
            workspace_root TEXT NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            package_name TEXT NOT NULL,
            file_path TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            signature TEXT NOT NULL,
            docstring TEXT NOT NULL,
            receiver TEXT,
            receiver_name TEXT,
            receiver_type TEXT,
            calls_json TEXT NOT NULL,
            file_imports_json TEXT NOT NULL,
            PRIMARY KEY (workspace_root, id)
        )",
        [],
    )?;

    // Create ts_symbols table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ts_symbols (
            id TEXT NOT NULL,
            workspace_root TEXT NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            file_path TEXT NOT NULL,
            scope_path TEXT NOT NULL,
            parent_id TEXT,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            signature TEXT NOT NULL,
            docstring TEXT NOT NULL,
            export INTEGER NOT NULL,
            export_names_json TEXT NOT NULL,
            calls_json TEXT NOT NULL,
            import_bindings_json TEXT NOT NULL,
            imports_json TEXT NOT NULL,
            re_exports_json TEXT NOT NULL,
            PRIMARY KEY (workspace_root, id)
        )",
        [],
    )?;

    // Create python_symbols table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS python_symbols (
            id TEXT NOT NULL,
            workspace_root TEXT NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            file_path TEXT NOT NULL,
            class_name TEXT,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            signature TEXT NOT NULL,
            docstring TEXT NOT NULL,
            decorators_json TEXT NOT NULL,
            calls_json TEXT NOT NULL,
            file_imports_json TEXT NOT NULL,
            PRIMARY KEY (workspace_root, id)
        )",
        [],
    )?;

    migrate_legacy_symbol_keys(conn)?;

    for language in ["rust", "ts", "python", "go"] {
        conn.execute_batch(&format!("CREATE INDEX IF NOT EXISTS idx_{language}_workspace_file ON {language}_symbols(workspace_root,file_path,start_line); CREATE INDEX IF NOT EXISTS idx_{language}_workspace_name ON {language}_symbols(workspace_root,name);"))?;
    }
    conn.execute("CREATE INDEX IF NOT EXISTS idx_ts_workspace_parent ON ts_symbols(workspace_root,parent_id)",[])?;

    // Create index_metadata table — tracks when each language index was last built
    conn.execute(
        "CREATE TABLE IF NOT EXISTS index_metadata (
            workspace_root TEXT NOT NULL,
            lang TEXT NOT NULL,
            generated_at_unix INTEGER NOT NULL,
            PRIMARY KEY (workspace_root, lang)
        )",
        [],
    )?;

    // Indexes for fast searching
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_name ON rust_symbols(name)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_go_name ON go_symbols(name)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ts_name ON ts_symbols(name)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_py_name ON python_symbols(name)",
        [],
    )?;

    Ok(())
}

/// Older symbol tables used `id` as a global primary key even though IDs are
/// relative to a workspace. Preserve their rows and IDs while scoping the key.
fn migrate_legacy_symbol_keys(conn: &Connection) -> Result<()> {
    let mut legacy = Vec::new();
    for table in ["rust_symbols", "go_symbols", "ts_symbols", "python_symbols"] {
        let schema: String = conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )?;
        if schema.contains("id TEXT PRIMARY KEY") {
            legacy.push((table, schema));
        }
    }
    if legacy.is_empty() {
        return Ok(());
    }

    for (table, schema) in legacy {
        let columns = &schema[schema.find('(').expect("symbol table has columns") + 1
            ..schema
                .rfind(')')
                .expect("symbol table has closing parenthesis")];
        let columns = columns.replacen("id TEXT PRIMARY KEY", "id TEXT NOT NULL", 1);
        let migrated = format!("{table}_scope_key_migration");
        conn.execute_batch(&format!(
            "CREATE TABLE {migrated} ({columns}, PRIMARY KEY (workspace_root, id));
             INSERT INTO {migrated} SELECT * FROM {table};
             DROP TABLE {table};
             ALTER TABLE {migrated} RENAME TO {table};"
        ))?;
    }
    Ok(())
}

/// Record (or update) the timestamp at which a language index was last fully built.
pub fn upsert_index_metadata(
    conn: &Connection,
    workspace_root: &str,
    lang: &str,
    generated_at_unix: u64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO index_metadata (workspace_root, lang, generated_at_unix) VALUES (?, ?, ?)
         ON CONFLICT(workspace_root, lang) DO UPDATE SET generated_at_unix = excluded.generated_at_unix",
        params![workspace_root, lang, generated_at_unix as i64],
    )?;
    Ok(())
}

/// Return the stored `generated_at_unix` for a language index, or `None` if it has never been built.
pub fn get_index_generated_at(conn: &Connection, workspace_root: &str, lang: &str) -> Option<u64> {
    conn.query_row(
        "SELECT generated_at_unix FROM index_metadata WHERE workspace_root = ? AND lang = ?",
        params![workspace_root, lang],
        |row| row.get::<_, i64>(0),
    )
    .ok()
    .map(|v| v as u64)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn schema_reinitializes_when_the_db_at_the_same_path_is_replaced() {
        use crate::server::agent_service::open_db;
        let root = std::env::temp_dir().join(format!("db_replace_{}", crate::server::agent_service::uuid_like()));
        let conn = open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('old','','test','completed',0,0)", []).unwrap();
        drop(conn);
        let path = root.join(".codex-workspace-mcp/codex_state.db");
        std::fs::rename(&path, path.with_extension("old")).unwrap();
        let replacement = rusqlite::Connection::open(&path).unwrap();
        replacement.execute("CREATE TABLE marker(id INTEGER)", []).unwrap();
        drop(replacement);
        let conn = open_db(&root.join(".")).unwrap();
        for table in ["marker", "memories", "agent_tasks", "agent_request_contexts"] {
            let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)", [table], |row| row.get(0)).unwrap();
            assert!(exists, "missing {table}");
        }
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM agent_tasks", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        let mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0)).unwrap();
        assert_eq!(mode, "wal");
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Several delegates of one conversation write its events at the same time: every write
    /// waits its turn instead of failing with "database is locked", and no sequence number repeats.
    #[test]
    fn concurrent_writers_wait_for_the_lock_instead_of_failing() {
        let root =
            std::env::temp_dir().join(format!("db_lock_{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        {
            let conn = crate::server::agent_service::open_db(&root).unwrap();
            conn.execute(
                "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','p','m','running',0,0)",
                [],
            )
            .unwrap();
        }
        let (writers, each) = (8, 40);
        let threads: Vec<_> = (0..writers)
            .map(|writer| {
                let root = root.clone();
                std::thread::spawn(move || -> Result<(), String> {
                    let mut conn =
                        crate::server::agent_service::open_db(&root).map_err(|e| e.to_string())?;
                    for step in 0..each {
                        // Read the next sequence number, then write: the pattern that fails in a deferred transaction.
                        let tx = conn
                            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                            .map_err(|e| e.to_string())?;
                        crate::server::agent_service::append_event_tx(
                            &tx,
                            "task",
                            "test/event",
                            &json!({"writer":writer,"step":step}),
                            None,
                        )
                        .map_err(|e| e.to_string())?;
                        tx.commit().map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        let conn = crate::server::agent_service::open_db(&root).unwrap();
        let (count, distinct): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COUNT(DISTINCT seq) FROM agent_task_events WHERE task_id='task'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (count, distinct),
            ((writers * each) as i64, (writers * each) as i64)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The same work in deferred transactions shows why they are not used for read-then-write.
    #[test]
    fn a_deferred_read_then_write_fails_while_another_connection_writes() {
        let root =
            std::env::temp_dir().join(format!("db_lock_{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let mut reader = crate::server::agent_service::open_db(&root).unwrap();
        let writer = crate::server::agent_service::open_db(&root).unwrap();
        writer
            .execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('a','p','m','running',0,0)", [])
            .unwrap();
        let tx = reader.transaction().unwrap();
        let _: i64 = tx
            .query_row("SELECT COUNT(*) FROM agent_tasks", [], |row| row.get(0))
            .unwrap();
        writer
            .execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('b','p','m','running',0,0)", [])
            .unwrap();
        let upgrade = tx.execute("UPDATE agent_tasks SET status='done' WHERE id='a'", []);
        assert!(upgrade.unwrap_err().to_string().contains("locked"));
        drop(tx);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn secret_files_are_matched_through_links_and_other_spellings() {
        let root = std::env::temp_dir().join(format!("secret_files_{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(root.join("grow")).unwrap();
        let secret = root.join("grow/credentials.yaml");
        std::fs::write(&secret, "refs: {}\n").unwrap();
        std::fs::write(root.join("grow/providers.yml"), "[]\n").unwrap();
        let secrets = vec![secret.clone(), root.join("absent/.env")];
        let resolve = |path: &std::path::Path| path.canonicalize().unwrap();
        assert!(super::is_secret_file(&resolve(&secret), &secrets));
        assert!(super::is_secret_file(&resolve(&root.join("grow/../grow/credentials.yaml")), &secrets));
        assert!(!super::is_secret_file(&resolve(&root.join("grow/providers.yml")), &secrets), "model settings stay readable");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&secret, root.join("link.yaml")).unwrap();
            assert!(super::is_secret_file(&resolve(&root.join("link.yaml")), &secrets), "a symlink reaches the same file");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn full_access_allows_project_env_read_while_keeping_host_db_protected() {
        let root = std::env::temp_dir().join(format!(
            "env_read_scope_{}",
            crate::server::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(root.join(".codex-workspace-mcp")).unwrap();
        let env_file = root.join(".env");
        let db_file = root.join(".codex-workspace-mcp/codex_state.db");
        std::fs::write(&env_file, "DB_HOST=127.0.0.1\n").unwrap();
        std::fs::write(&db_file, b"").unwrap();

        assert!(super::check_source_access(&env_file).is_err());
        super::with_thread_allow_project_env_read(true, || {
            assert!(super::check_source_access(&env_file).is_ok());
            assert!(super::check_source_access(&db_file).is_err());
        });
        assert!(super::check_source_access(&env_file).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
