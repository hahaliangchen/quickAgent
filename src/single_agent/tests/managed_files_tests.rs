//! Extracted unit tests for `single_agent::managed_files`.

use super::*;

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!("agent-managed-files-{}", crate::agent_service::uuid_like()));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn shell_takeover_preserves_a_baseline_replaced_during_the_interval() -> Result<()> {
    let root = root();
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let workspace = state.workspace.root();
    let work = workspace.to_string_lossy();
    save(&root, "task", &work, "document.txt", Some(b"before"), true)?;
    let matching = state.shell_matching_baselines("task", workspace, |path, bytes| {
        path == "document.txt" && bytes == Some(b"before".as_slice())
    })?;
    assert_eq!(matching, vec!["document.txt"]);
    save(&root, "task", &work, "document.txt", Some(b"new host baseline"), true)?;
    let attribution = crate::workspace_changes::ShellAttribution::new("test-cas", &["@main".into()])?;
    state.shell_adopt_baseline("task", workspace, 1, "document.txt", Some(b"before"), Some(b"shell after"), &attribution)?;
    assert_eq!(baseline(&root, "task", &work, "document.txt")?.flatten().as_deref(), Some(b"new host baseline".as_slice()));
    assert_eq!(write_clock(&root, "task")?, 0);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

fn commit_as(root: &Path, task: &str, owner: &str, content: &str, turn: usize) -> Result<()> {
    let p = prepare(root, task, root, "write_file", json!({"path":"document.txt","content":content}))?;
    fs::write(root.join("document.txt"), content)?;
    assert!(p.committed(root, task)?);
    p.record_writer(root, task, owner, turn)
}

fn adopt_shell(state: &crate::agent_service::AgentServiceState, id: &str, owners: &[&str]) -> Result<()> {
    let workspace = state.workspace.root();
    let work = workspace.to_string_lossy();
    save(workspace, "task", &work, "document.txt", Some(b"before"), true)?;
    fs::write(workspace.join("document.txt"), "shell")?;
    let owners = owners.iter().map(|owner| (*owner).to_owned()).collect::<Vec<_>>();
    let attribution = crate::workspace_changes::ShellAttribution::new(id, &owners)?;
    state.shell_adopt_baseline("task", workspace, 1, "document.txt", Some(b"before"), Some(b"shell"), &attribution)
}

#[test]
fn a_single_shell_owner_can_write_again_without_a_false_parallel_conflict() -> Result<()> {
    let root = root();
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let started = write_clock(&root, "task")?;
    adopt_shell(&state, "own-shell", &["a", "a"])?;
    let pending = write_as(&root, "after shell")?;
    assert!(!pending.parallel_conflict(&root, "task", "a", started)?);
    assert!(pending.parallel_conflict_result(&root, "task", "a", started)?.is_none());
    let other = pending.parallel_conflict_result(&root, "task", "b", started)?.context("another owner must still conflict")?;
    assert_eq!(other["reason"], "parallel");
    assert_eq!(other["writer_owner"], "a");
    assert_eq!(other["shell_interval"]["owners"], json!(["a"]));
    assert_eq!(other["shell_interval"]["shared"], false);
    assert!(pending.parallel_conflict_result(&root, "task", "b", write_clock(&root, "task")?)?.is_none());
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn shared_shell_writers_keep_the_barrier_and_report_unknown_authorship() -> Result<()> {
    let root = root();
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let started = write_clock(&root, "task")?;
    adopt_shell(&state, "mixed-shell", &["b", "a", "b", "@main"])?;
    for owner in ["a", "b", "outsider"] {
        let pending = write_as(&root, &format!("from {owner}"))?;
        let result = pending.parallel_conflict_result(&root, "task", owner, started)?.context("shared version barrier missing")?;
        assert_eq!(result["reason"], "shared_interval");
        assert_eq!(result["writer_owner"], "@shared_shell:mixed-shell");
        assert_eq!(result["shell_interval"]["owners"], json!(["@main", "a", "b"]));
        assert_eq!(result["shell_interval"]["attribution"], "unknown");
        assert!(result["message"].as_str().unwrap().contains("who changed it is unknown"));
        assert!(!result["message"].as_str().unwrap().contains("Another function"));
        assert_eq!(fs::read_to_string(result["candidate_path"].as_str().unwrap())?, format!("from {owner}"));
    }
    assert_eq!(fs::read_to_string(root.join("document.txt"))?, "shell");
    assert!(write_as(&root, "shell")?.parallel_conflict_result(&root, "task", "a", started)?.is_none());
    // An owner's later precise edit must not hide the shared write that
    // happened after this frame started.
    commit_as(&root, "task", "a", "own later edit", 1)?;
    let result = write_as(&root, "replace")?.parallel_conflict_result(&root, "task", "a", started)?.context("lost shared barrier")?;
    assert_eq!(result["reason"], "shared_interval");
    assert!(write_as(&root, "new frame")?.parallel_conflict_result(&root, "task", "a", write_clock(&root, "task")?)?.is_none());
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn ordinary_writes_do_not_inherit_stale_shell_provenance() -> Result<()> {
    let root = root();
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    adopt_shell(&state, "first-shell", &["a"])?;
    commit_as(&root, "task", "a", "ordinary write", 1)?;
    let result = write_as(&root, "from b")?.parallel_conflict_result(&root, "task", "b", 0)?.context("missing other writer")?;
    assert_eq!(result["reason"], "parallel");
    assert!(result.get("shell_interval").is_none());
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

fn write_as(root: &Path, content: &str) -> Result<Prepared> {
    prepare(root, "task", root, "write_file", json!({"path":"document.txt","content":content}))
}

fn session<'a>(state: &'a crate::agent_service::AgentServiceState, cancel: &'a tokio_util::sync::CancellationToken, node: Option<&str>) -> super::super::Session<'a> {
    super::super::Session {
        agent_id: None, task_node_id: node.map(str::to_owned), plugin: None,
        state, task: "task", model: "fake", turn: 1, cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    }
}

/// A parallel function's frame that started at write version `clock`.
fn parallel_frame(conn: &rusqlite::Connection, id: &str, clock: i64) -> Result<()> {
    let node = json!({"node_id":id,"parent_id":null,"kind":"temp","ordinal":0,"title":id,"goal":id,"inputs":{},"executor":"inline","agent_id":null,"status":"running","spawned_from":null,"briefing":"","result":null,"review":null,"first_seq":0,"last_seq":0,"metadata":{"parallel":true,"parallel_owner":id,"file_write_clock":clock}});
    conn.execute("INSERT INTO agent_task_nodes(task_id,node_id,parent_id,executor,status,data) VALUES ('task',?1,NULL,'inline','running',?2)", params![id, node.to_string()])?;
    Ok(())
}

#[test]
fn only_another_functions_write_after_this_one_started_conflicts() -> Result<()> {
    let root = root();
    fs::write(root.join("document.txt"), "base")?;
    let early = write_clock(&root, "task")?;
    commit_as(&root, "task", "a", "from A", 1)?;
    let later = write_clock(&root, "task")?;
    let pending = write_as(&root, "from B")?;
    assert!(pending.parallel_conflict(&root, "task", "b", early)?, "B started before A wrote");
    assert!(!pending.parallel_conflict(&root, "task", "b", later)?, "started after A wrote, in order or in a later turn");
    assert!(!pending.parallel_conflict(&root, "task", "a", early)?, "a function's own writes never block it");
    assert!(!write_as(&root, "from A")?.parallel_conflict(&root, "task", "b", early)?, "writing back the current content loses nothing");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_functions_own_later_write_does_not_hide_another_writer() -> Result<()> {
    let root = root();
    fs::write(root.join("document.txt"), "base")?;
    let started = write_clock(&root, "task")?;
    commit_as(&root, "task", "a", "from A", 1)?;
    commit_as(&root, "task", "b", "from A + local change", 1)?;
    assert!(write_as(&root, "stale B")?.parallel_conflict(&root, "task", "b", started)?);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn parallel_functions_and_the_main_program_through_the_file_tools() -> Result<()> {
    let root = root();
    fs::write(root.join("document.txt"), "base")?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    parallel_frame(&conn, "a", 0)?;
    parallel_frame(&conn, "b", 0)?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let (a, b, main) = (session(&state, &cancel, Some("a")), session(&state, &cancel, Some("b")), session(&state, &cancel, None));
    let write = |content: &str| json!({"path":"document.txt","content":content});
    super::super::business_tool(&b, "node", "read_file", json!({"path":"document.txt"}), "read b").await?;
    let first = super::super::business_tool(&a, "node", "write_file", write("from A"), "write a").await?;
    assert_eq!(first["changed"], true, "{first}");
    let refused = super::super::business_tool(&b, "node", "write_file", write("from B"), "write b").await?;
    assert_eq!(refused["error_code"], "ASSET_CONFLICT", "{refused}");
    assert_eq!(refused["reason"], "parallel");
    assert_eq!(refused["base_known"], false);
    assert_eq!(refused["current_preview"]["text"], "from A");
    assert_eq!(refused["candidate_preview"]["text"], "from B");
    assert_eq!(fs::read_to_string(refused["candidate_path"].as_str().unwrap())?, "from B");
    assert!(refused["message"].as_str().unwrap().contains("edit or replace"), "the function is told how to go on: {refused}");
    // The host does not record what a function read: reading again does not lift it.
    super::super::business_tool(&b, "node", "read_file", json!({"path":"document.txt"}), "read b again").await?;
    let again = super::super::business_tool(&b, "node", "write_file", write("from A and B"), "write b again").await?;
    assert_eq!(again["error_code"], "ASSET_CONFLICT", "{again}");
    // A function that starts after A wrote may replace the file.
    parallel_frame(&conn, "c", write_clock(&root, "task")?)?;
    let c = session(&state, &cancel, Some("c"));
    let replaced = super::super::business_tool(&c, "node", "write_file", write("from C"), "write c").await?;
    assert_eq!(replaced["changed"], true, "{replaced}");
    // The main program's writes count as another writer's too.
    let by_main = super::super::business_tool(&main, "node", "write_file", write("from main"), "write main").await?;
    assert_eq!(by_main["changed"], true, "{by_main}");
    let after_main = super::super::business_tool(&c, "node", "write_file", write("from C again"), "write c again").await?;
    assert_eq!(after_main["error_code"], "ASSET_CONFLICT", "{after_main}");
    assert_eq!(fs::read_to_string(root.join("document.txt"))?, "from main");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn conflict_preserves_current_and_proposed_content() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "original\n")?;
    let result = json!({"code_hash":crate::symbol_description::content_hash(b"original\n")});
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &result)?;
    fs::write(&path, "external\n")?;
    let prepared = prepare(&root, "task", &root, "edit_file", json!({"path":"document.txt","edits":[{"old_text":"original","new_text":"agent"}]}))?;
    assert!(prepared.conflict()?);
    let saved = prepared.preserve(&root, "task")?;
    assert_eq!(fs::read_to_string(&path)?, "external\n");
    assert_eq!(fs::read_to_string(saved["candidate_path"].as_str().unwrap())?, "agent\n");
    assert_eq!(saved["error_code"], "ASSET_CONFLICT");
    assert_eq!(saved["candidate"], "file");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_change_summary_shows_only_the_changed_lines() {
    let summary = change_summary(Some(b"a\nb\nc\nd\n"), Some(b"a\nB\nB2\nd\n"));
    assert_eq!(summary, json!({"kind":"modified","first_line":2,"lines_before":2,"lines_after":2,"before":"b\nc\n","after":"B\nB2\n"}));
    assert_eq!(change_summary(None, Some(b"x")), json!({"kind":"created"}));
    assert_eq!(change_summary(Some(b"x"), None), json!({"kind":"deleted"}));
    let long = "y".repeat(5000);
    assert_eq!(change_summary(Some(b"x"), Some(long.as_bytes()))["after"]["truncated"], true);
}

/// 2026-11-13: another agent edited a file the task was editing; the
/// task's next write failed and ended the whole turn. Now the AI is told
/// before its next request, once per version, and nothing stops.
#[tokio::test]
async fn the_ai_is_told_once_when_its_files_change_outside() -> Result<()> {
    let root = root();
    fs::write(root.join("document.txt"), "first\n")?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let session = session(&state, &cancel, None);
    super::super::business_tool(&session, "node", "read_file", json!({"path":"document.txt"}), "read").await?;
    let mut messages = Vec::new();
    super::super::external_changes::announce(&session, &mut messages).await;
    assert!(messages.is_empty(), "nothing changed yet");
    fs::write(root.join("document.txt"), "first\nsecond\n")?;
    super::super::external_changes::announce(&session, &mut messages).await;
    assert_eq!(messages.len(), 1);
    let note: Value = serde_json::from_str(messages[0]["content"].as_str().unwrap())?;
    assert_eq!(note["files"][0]["path"], "document.txt");
    assert_eq!(note["files"][0]["change"]["after"], "second\n");
    super::super::external_changes::announce(&session, &mut messages).await;
    assert_eq!(messages.len(), 1, "the same version is told once");
    fs::write(root.join("document.txt"), "first\nthird\n")?;
    super::super::external_changes::announce(&session, &mut messages).await;
    assert_eq!(messages.len(), 2, "a newer version is told again");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn successful_writes_update_baseline_without_model_hash() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "first")?;
    let result = json!({"code_hash":crate::symbol_description::content_hash(b"first")});
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &result)?;
    let prepared = prepare(&root, "task", &root, "write_file", json!({"path":"document.txt","content":"second"}))?;
    assert!(!prepared.conflict()?);
    assert_eq!(prepared.args["expected_code_hash"], crate::symbol_description::content_hash(b"first"));
    fs::write(&path, "second")?;
    assert!(prepared.committed(&root, "task")?);
    let next = prepare(&root, "task", &root, "write_file", json!({"path":"document.txt","content":"third"}))?;
    assert!(!next.conflict()?);
    assert_eq!(next.args["expected_code_hash"], crate::symbol_description::content_hash(b"second"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn subsequent_read_does_not_replace_original_baseline() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "before")?;
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &json!({"code_hash":crate::symbol_description::content_hash(b"before")}))?;
    fs::write(&path, "external")?;
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &json!({"code_hash":crate::symbol_description::content_hash(b"external")}))?;
    let prepared = prepare(&root, "task", &root, "write_file", json!({"path":"document.txt","content":"agent"}))?;
    assert!(prepared.conflict()?);
    fs::remove_dir_all(root)?;
    Ok(())
}

/// 2026-11-11: a file the task read in turn 1 was changed by another
/// session hours later; in turn 7 the AI read the new version and edited
/// it, and the turn failed on the six-hour-old baseline.
#[test]
fn a_change_made_between_turns_is_the_next_turns_starting_point() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "original\n")?;
    fs::write(root.join("untouched.txt"), "same\n")?;
    let hash = |bytes: &[u8]| json!({"code_hash":crate::symbol_description::content_hash(bytes)});
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &hash(b"original\n"))?;
    observe(&root, "task", &root, &json!({"path":"untouched.txt"}), &hash(b"same\n"))?;
    // Between turns: another session changes the file.
    fs::write(&path, "between\n")?;
    assert_eq!(refresh_baselines(&root, "task")?, vec!["document.txt".to_owned()], "only the changed file moves");
    let edit = prepare(&root, "task", &root, "edit_file", json!({"path":"document.txt","edits":[{"old_text":"between","new_text":"agent"}]}))?;
    assert!(!edit.conflict()?, "the new turn edits what is there now");
    // A change made while the turn runs still stops it.
    fs::write(&path, "during\n")?;
    let edit = prepare(&root, "task", &root, "edit_file", json!({"path":"document.txt","edits":[{"old_text":"during","new_text":"agent"}]}))?;
    assert!(edit.conflict()?);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn an_edit_written_against_an_external_change_stops_as_a_conflict() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "original\n")?;
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &json!({"code_hash":crate::symbol_description::content_hash(b"original\n")}))?;
    fs::write(&path, "external\n")?;
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &json!({"code_hash":crate::symbol_description::content_hash(b"external\n")}))?;
    // The edit fits only the external version, never the task's baseline.
    let prepared = prepare(&root, "task", &root, "edit_file", json!({"path":"document.txt","edits":[{"old_text":"external","new_text":"agent"}]}))?;
    assert!(prepared.conflict()?);
    let saved = prepared.preserve(&root, "task")?;
    assert_eq!(saved["error_code"], "ASSET_CONFLICT");
    assert_eq!(saved["candidate"], "request");
    assert_eq!(fs::read_to_string(&path)?, "external\n");
    let request: Value = serde_json::from_str(&fs::read_to_string(saved["candidate_path"].as_str().unwrap())?)?;
    assert_eq!(request["edits"][0]["old_text"], "external");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn agent_tools_manage_versions_and_preserve_a_conflicting_edit() -> Result<()> {
    let root = root();
    fs::write(root.join("document.txt"), "first")?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let session = super::super::Session {
        agent_id: None, task_node_id: None, plugin: None,
        state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    };
    super::super::business_tool(&session, "node", "read_file", json!({"path":"document.txt"}), "read").await.context("read through agent")?;
    let written = super::super::business_tool(&session, "node", "write_file", json!({"path":"document.txt","content":"second"}), "write").await.context("write through agent")?;
    assert_eq!(written["changed"], true);
    fs::write(root.join("document.txt"), "external")?;
    let conflict = super::super::business_tool(&session, "node", "edit_file", json!({"path":"document.txt","edits":[{"old_text":"second","new_text":"proposed"}]}), "edit").await.context("conflicting edit through agent")?;
    assert_eq!(conflict["error_code"], "ASSET_CONFLICT");
    assert_eq!(fs::read_to_string(root.join("document.txt"))?, "external");
    assert_eq!(fs::read_to_string(conflict["candidate_path"].as_str().unwrap())?, "proposed");
    // The AI is shown what changed and decides; the turn goes on, and its
    // next edit works on the version it was shown.
    assert_eq!(conflict["external_change"]["before"], "second", "{conflict}");
    assert_eq!(conflict["external_change"]["after"], "external");
    assert!(conflict["message"].as_str().unwrap().contains("Decide"), "{conflict}");
    let adapted = super::super::business_tool(&session, "node", "edit_file", json!({"path":"document.txt","edits":[{"old_text":"external","new_text":"external + proposed"}]}), "edit again").await?;
    assert_eq!(adapted["changed"], true, "{adapted}");
    assert_eq!(fs::read_to_string(root.join("document.txt"))?, "external + proposed");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[tokio::test]
async fn agent_reads_an_accessible_file_outside_the_workspace_and_in_full_access_writes_it() -> Result<()> {
    let outside = root();
    let root = root();
    fs::write(outside.join("settings.yaml"), "model: deepseek\nmax_tokens: 8192\n")?;
    fs::create_dir_all(outside.join(".codex-workspace-mcp"))?;
    fs::write(outside.join(".codex-workspace-mcp/codex_state.db"), "not for tools")?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let session = super::super::Session {
        agent_id: None, task_node_id: None, plugin: None,
        state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    };
    let file = outside.join("settings.yaml").to_string_lossy().into_owned();
    let read = super::super::business_tool(&session, "node", "read_file", json!({"path":file}), "read outside").await.context("read outside")?;
    assert!(read.to_string().contains("max_tokens: 8192"), "{read}");
    let lines = super::super::business_tool(&session, "node", "read_file_lines", json!({"path":file,"start_line":1,"end_line":1}), "lines outside").await.context("lines outside")?;
    assert!(lines.to_string().contains("model: deepseek"), "{lines}");
    let edit = super::super::business_tool(&session, "node", "write_file", json!({"path":file,"content":"changed"}), "write outside").await;
    // Full access may edit outside the project (design guide §9).
    assert_eq!(edit?["changed"], true);
    assert_eq!(fs::read_to_string(outside.join("settings.yaml"))?, "changed");
    let database = outside.join(".codex-workspace-mcp/codex_state.db").to_string_lossy().into_owned();
    let denied = super::super::business_tool(&session, "node", "read_file", json!({"path":database}), "read state").await;
    assert!(denied.as_ref().map(|value| value.to_string()).unwrap_or_else(|error| format!("{error:#}")).contains("SQLITE_STATE_FILE"), "{denied:?}");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
    Ok(())
}

async fn answer_outside_read(root: &Path, decision: &str) -> Result<String> {
    for _ in 0..200 {
        let conn = crate::agent_service::open_db(root)?;
        let pending: Option<String> = conn.query_row("SELECT call_id FROM tool_approval_pending WHERE task_id='task'", [], |r| r.get(0)).optional().unwrap_or(None);
        if let Some(call) = pending {
            conn.execute("INSERT OR IGNORE INTO tool_approvals(task_id,call_id,decision) VALUES ('task',?1,?2)", params![call, decision])?;
            return Ok(call);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("no approval was requested")
}

#[tokio::test]
async fn reading_outside_the_workspace_waits_for_approval_unless_full_access() -> Result<()> {
    for mode in [crate::agent_service::PermissionMode::Ask, crate::agent_service::PermissionMode::ReadOnly] {
        let outside = root();
        let root = root();
        fs::write(outside.join("settings.yaml"), "model: deepseek\n")?;
        fs::write(root.join("inside.txt"), "inside\n")?;
        let mut state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
        state.permission_mode = mode;
        let conn = crate::agent_service::open_db(&root)?;
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let session = super::super::Session {
            agent_id: None, task_node_id: None, plugin: None,
            state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
            ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
        };
        let file = outside.join("settings.yaml").to_string_lossy().into_owned();
        super::super::business_tool(&session, "node", "read_file", json!({"path":"inside.txt"}), "inside").await.context("inside read needs no approval")?;
        assert!(!conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_task_events WHERE kind='approval/request')", [], |r| r.get::<_, bool>(0))?);
        let (read, _) = tokio::join!(
            super::super::business_tool(&session, "node", "read_file", json!({"path":file}), "outside once"),
            answer_outside_read(&root, "once"));
        assert!(read?.to_string().contains("model: deepseek"));
        let (denied, _) = tokio::join!(
            super::super::business_tool(&session, "node", "list_dir", json!({"path":outside.to_string_lossy()}), "outside denied"),
            answer_outside_read(&root, "deny"));
        assert!(format!("{:#}", denied.unwrap_err()).contains("USER_DENIED: the user declined reading"));
        let (granted, _) = tokio::join!(
            super::super::business_tool(&session, "node", "read_file_lines", json!({"path":file,"start_line":1,"end_line":1}), "outside session"),
            answer_outside_read(&root, "session"));
        granted?;
        super::super::business_tool(&session, "node", "search_text", json!({"path":outside.to_string_lossy(),"query":"deepseek"}), "granted for the session").await.context("session grant covers later reads")?;
        let request: Value = serde_json::from_str(&conn.query_row("SELECT data FROM agent_task_events WHERE kind='approval/request' ORDER BY seq LIMIT 1", [], |r| r.get::<_, String>(0))?)?;
        assert_eq!(request["outside_paths"], json!([file]));
        assert!(!conn.query_row("SELECT EXISTS(SELECT 1 FROM tool_approval_grants WHERE scope LIKE 'shell:%')", [], |r| r.get::<_, bool>(0))?, "a read grant is not a command grant");
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
    Ok(())
}

#[tokio::test]
async fn auto_review_decides_clear_cases_and_hands_unclear_ones_to_the_user() -> Result<()> {
    use axum::{Json, Router, routing::post};
    use std::{collections::VecDeque, sync::{Arc, Mutex}};
    let replies = Arc::new(Mutex::new(VecDeque::from([
        "{\"decision\":\"allow\",\"reason\":\"读取用户要查的模型配置\"}".to_owned(),
        "{\"decision\":\"deny\",\"reason\":\"与用户的要求无关\"}".to_owned(),
        "{\"decision\":\"ask\",\"reason\":\"不确定用户是否想让它看这个目录\"}".to_owned(),
        "我觉得可以".to_owned(),
    ])));
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new().route("/v1/chat/completions", post({
        let (replies, seen) = (replies.clone(), seen.clone());
        move |Json(body): Json<Value>| {
            seen.lock().unwrap().push(body);
            let reply = replies.lock().unwrap().pop_front().unwrap_or_default();
            async move {
                let chunk = json!({"choices":[{"delta":{"content":reply},"finish_reason":"stop"}]});
                ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let outside = root();
    let root = root();
    fs::write(outside.join("settings.yaml"), "model: deepseek\n")?;
    let mut state = crate::agent_service::tests::test_state(&root, address);
    state.permission_mode = crate::agent_service::PermissionMode::AutoReview;
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    conn.execute("INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('task',1,0,'user/message',?1)",
        [json!({"model_content":"帮我整理 ~/.dsh 里配置的模型"}).to_string()])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let session = super::super::Session {
        agent_id: None, task_node_id: None, plugin: None,
        state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    };
    let file = outside.join("settings.yaml").to_string_lossy().into_owned();
    let count = |kind: &str| conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind=?1", [kind], |r| r.get::<_, i64>(0)).unwrap();

    let read = super::super::business_tool(&session, "node", "read_file", json!({"path":file}), "allowed").await.context("allowed by the reviewer")?;
    assert!(read.to_string().contains("model: deepseek"));
    assert_eq!(count("approval/request"), 0, "a clear allow needs no card");
    let prompt = seen.lock().unwrap()[0].to_string();
    assert!(prompt.contains("帮我整理 ~/.dsh 里配置的模型") && prompt.contains("settings.yaml"), "{prompt}");

    let denied = super::super::business_tool(&session, "node", "list_dir", json!({"path":outside.to_string_lossy()}), "denied").await;
    assert!(format!("{:#}", denied.unwrap_err()).contains("AI_REVIEW_DENIED"));
    assert_eq!(count("approval/request"), 0);

    let (asked, _) = tokio::join!(
        super::super::business_tool(&session, "node", "read_file", json!({"path":file}), "unclear"),
        answer_outside_read(&root, "once"));
    asked?;
    let request: Value = serde_json::from_str(&conn.query_row("SELECT data FROM agent_task_events WHERE kind='approval/request' ORDER BY seq LIMIT 1", [], |r| r.get::<_, String>(0))?)?;
    assert_eq!(request["review_reason"], "不确定用户是否想让它看这个目录");

    let (unreadable, _) = tokio::join!(
        super::super::business_tool(&session, "node", "read_file", json!({"path":file}), "unreadable review"),
        answer_outside_read(&root, "deny"));
    assert!(format!("{:#}", unreadable.unwrap_err()).contains("USER_DENIED"), "an unreadable review goes to the user");
    assert_eq!(count("approval/review"), 4);
    server.abort();

    let offline = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let mut offline = offline;
    offline.permission_mode = crate::agent_service::PermissionMode::AutoReview;
    let session = super::super::Session {
        agent_id: None, task_node_id: None, plugin: None,
        state: &offline, task: "task", model: "fake", turn: 1, cancel: &cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    };
    let (failed, _) = tokio::join!(
        super::super::business_tool(&session, "node", "read_file", json!({"path":file}), "review failed"),
        answer_outside_read(&root, "once"));
    failed.context("a failed review falls back to the user's card")?;
    drop(conn);
    drop(state);
    drop(offline);
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
    Ok(())
}

#[tokio::test]
async fn a_failed_edit_tells_the_agent_what_the_file_has() -> Result<()> {
    let root = root();
    fs::write(root.join("bundler.js"), "function build() {\n  const out = [];\n  return out.join('\\n');\n}\n")?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let main = session(&state, &cancel, None);
    let stale = json!({"path":"bundler.js","edits":[{"old_text":"  const out = [];\n  return out.join(',');","new_text":"  return '';"}]});
    let failed = super::super::business_tool(&main, "node", "edit_file", stale, "edit").await;
    let message = format!("{:#}", failed.expect_err("old_text is not in the file"));
    assert!(message.contains("old_text not found") && message.contains("2|   const out = [];") && message.contains("3|   return out.join('\\n');"), "{message}");
    assert_eq!(fs::read_to_string(root.join("bundler.js"))?, "function build() {\n  const out = [];\n  return out.join('\\n');\n}\n");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn a_commands_changes_become_the_tasks_baselines_but_not_another_tasks() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "first")?;
    let read = json!({"code_hash":crate::symbol_description::content_hash(b"first")});
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &read)?;
    observe(&root, "other", &root, &json!({"path":"document.txt"}), &read)?;
    let untouched = untouched(&root, "task", &root)?;
    fs::write(&path, "command")?;
    untouched.adopt(&root, "task", None, 1)?;
    let edit = prepare(&root, "task", &root, "edit_file", json!({"path":"document.txt","edits":[{"old_text":"command","new_text":"agent"}]}))?;
    assert!(!edit.conflict()?);
    assert_eq!(edit.proposed, b"agent");
    let other = prepare(&root, "other", &root, "write_file", json!({"path":"document.txt","content":"other"}))?;
    assert!(other.conflict()?);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_change_made_outside_before_the_command_still_conflicts() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "first")?;
    observe(&root, "task", &root, &json!({"path":"document.txt"}), &json!({"code_hash":crate::symbol_description::content_hash(b"first")}))?;
    fs::write(&path, "outside")?;
    let untouched = untouched(&root, "task", &root)?;
    fs::write(&path, "outside+command")?;
    untouched.adopt(&root, "task", None, 1)?;
    let prepared = prepare(&root, "task", &root, "write_file", json!({"path":"document.txt","content":"agent"}))?;
    assert!(prepared.conflict()?);
    assert_eq!(prepared.base.as_deref(), Some(b"first".as_slice()));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn a_parallel_functions_command_counts_as_its_write() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "first")?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    parallel_frame(&conn, "one", 0)?;
    parallel_frame(&conn, "two", 0)?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let (one, two) = (session(&state, &cancel, Some("one")), session(&state, &cancel, Some("two")));
    super::super::business_tool(&one, "node", "read_file", json!({"path":"document.txt"}), "read").await?;
    let ran = super::super::business_tool_inner(&one, "node", "run_program", json!({"program":"python","args":["-c","import pathlib; pathlib.Path('document.txt').write_text('command')"]}), "run", true, false).await?;
    if ran["outcome"] == "spawn_failed" {
        // Python is not installed here.
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        return Ok(());
    }
    assert_eq!(ran["process_success"], true, "{ran}");
    let refused = super::super::business_tool(&two, "node", "write_file", json!({"path":"document.txt","content":"from two"}), "write two").await?;
    assert_eq!(refused["error_code"], "ASSET_CONFLICT", "{refused}");
    assert_eq!(refused["reason"], "parallel");
    let own = super::super::business_tool(&one, "node", "write_file", json!({"path":"document.txt","content":"from one"}), "write one").await?;
    assert_eq!(own["changed"], true, "{own}");
    assert_eq!(fs::read_to_string(&path)?, "from one");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

/// 2026-11-11: updating a few lines of ~/.config/grow took a module and a
/// shell each time, and rebuilding the YAML in code lost a provider.
#[tokio::test]
async fn full_access_edits_a_file_outside_the_project_in_place() -> Result<()> {
    let root = root();
    let outside = self::root();
    let config = outside.join("providers.yml");
    fs::write(&config, "deepseek:\n  contextWindow: 262144\nqwen: kept\n")?;
    let mut state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let path = config.to_string_lossy().into_owned();
    let edit = |old: &str, new: &str| json!({"path":path,"edits":[{"old_text":old,"new_text":new}]});
    // ReadOnly always refuses external edits; Ask mode gates on user confirmation (including workspace_root overrides).
    state.permission_mode = crate::agent_service::PermissionMode::ReadOnly;
    {
        let session = super::super::Session {
            agent_id: None, task_node_id: None, plugin: None,
            state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
            ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
        };
        let refused = super::super::business_tool(&session, "node", "edit_file", edit("262144", "1048576"), "ro").await;
        assert!(refused.is_err(), "{refused:?}");
    }
    state.permission_mode = crate::agent_service::PermissionMode::Ask;
    {
        let root_for_deny = root.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let conn = crate::agent_service::open_db(&root_for_deny).unwrap();
            let _ = conn.execute(
                "CREATE TABLE IF NOT EXISTS tool_approvals(task_id TEXT NOT NULL,call_id TEXT NOT NULL,decision TEXT NOT NULL,PRIMARY KEY(task_id,call_id))",
                [],
            );
            conn.execute("INSERT INTO tool_approvals VALUES ('task','ask-deny','deny')", []).unwrap();
        });
        let session = super::super::Session {
            agent_id: None, task_node_id: None, plugin: None,
            state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
            ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
        };
        let confused = json!({
            "workspace_root": outside.to_string_lossy(),
            "path": "providers.yml",
            "edits": [{"old_text": "262144", "new_text": "1048576"}]
        });
        let refused = super::super::business_tool(&session, "node", "edit_file", confused, "ask-deny").await;
        assert!(format!("{refused:?}").contains("USER_DENIED"), "{refused:?}");
        assert!(fs::read_to_string(&config)?.contains("262144"));

        let root_for_allow = root.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let conn = crate::agent_service::open_db(&root_for_allow).unwrap();
            conn.execute("INSERT INTO tool_approvals VALUES ('task','ask-allow','once')", []).unwrap();
        });
        let new_sub = outside.join("nested/created.txt");
        let allowed = super::super::business_tool(
            &session,
            "node",
            "write_file",
            json!({"path": new_sub.to_string_lossy(), "content": "cross-project ok"}),
            "ask-allow",
        )
        .await?;
        assert_eq!(allowed["changed"], true, "{allowed}");
        assert_eq!(fs::read_to_string(&new_sub)?, "cross-project ok");
    }
    state.permission_mode = crate::agent_service::PermissionMode::FullAccess;
    let session = super::super::Session {
        agent_id: None, task_node_id: None, plugin: None,
        state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    };
    let edited = super::super::business_tool(&session, "node", "edit_file", edit("262144", "1048576"), "edit").await?;
    assert_eq!(edited["changed"], true, "{edited}");
    assert_eq!(fs::read_to_string(&config)?, "deepseek:\n  contextWindow: 1048576\nqwen: kept\n", "only the edited text changed");
    let recorded: i64 = conn.query_row("SELECT count(*) FROM agent_file_changes WHERE task_id='task'", [], |r| r.get(0)).unwrap_or(0);
    assert_eq!(recorded, 0, "the project's turn record is about the project");
    // It keeps a baseline like a project file: a change made beside it stops the next edit.
    fs::write(&config, "deepseek:\n  contextWindow: 1048576\nqwen: changed by someone\n")?;
    let conflict = super::super::business_tool(&session, "node", "edit_file", edit("1048576", "2000000"), "again").await?;
    assert_eq!(conflict["error_code"], "ASSET_CONFLICT", "{conflict}");
    // Host state files stay closed whatever the mode.
    fs::create_dir_all(outside.join(".codex-workspace-mcp"))?;
    let state_file = outside.join(".codex-workspace-mcp/codex_state.db");
    fs::write(&state_file, "x")?;
    let closed = super::super::business_tool(&session, "node", "write_file", json!({"path":state_file.to_string_lossy(),"content":"y"}), "state").await;
    assert!(format!("{closed:?}").contains("SQLITE_STATE_FILE"), "{closed:?}");
    assert_eq!(fs::read_to_string(&state_file)?, "x");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
    Ok(())
}

#[tokio::test]
async fn a_command_the_task_runs_changes_its_files_as_its_own_edit() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "first")?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let session = super::super::Session {
        agent_id: None, task_node_id: None, plugin: None,
        state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    };
    let python = |code: &str| json!({"program":"python","args":["-c",format!("import pathlib; p = pathlib.Path('document.txt'); {code}")]});
    super::super::business_tool(&session, "node", "write_file", json!({"path":"document.txt","content":"second"}), "write").await?;
    let ran = super::super::business_tool_inner(&session, "node", "run_program", python("p.write_text('command')"), "run", true, false).await?;
    if ran["outcome"] == "spawn_failed" {
        // Python is not installed here.
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        return Ok(());
    }
    assert_eq!(ran["process_success"], true, "{ran}");
    let edited = super::super::business_tool(&session, "node", "edit_file", json!({"path":"document.txt","edits":[{"old_text":"command","new_text":"agent"}]}), "edit").await?;
    assert_eq!(edited["changed"], true, "{edited}");
    assert_eq!(fs::read_to_string(&path)?, "agent");
    // A change made outside before the next command still stops the edit after it.
    fs::write(&path, "outside")?;
    super::super::business_tool_inner(&session, "node", "run_program", python("p.write_text(p.read_text() + '+command')"), "run again", true, false).await?;
    let conflict = super::super::business_tool(&session, "node", "edit_file", json!({"path":"document.txt","edits":[{"old_text":"agent","new_text":"again"}]}), "edit again").await?;
    assert_eq!(conflict["error_code"], "ASSET_CONFLICT", "{conflict}");
    assert_eq!(fs::read_to_string(&path)?, "outside+command");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[tokio::test]
async fn what_a_background_process_changes_is_not_taken_as_the_tasks_own() -> Result<()> {
    let root = root();
    let path = root.join("document.txt");
    fs::write(&path, "first")?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    fs::write(root.join("package.json"), r#"{"scripts":{"dev":"node writer.js"}}"#)?;
    // The file is written before the server listens, so before the launch returns.
    fs::write(root.join("writer.js"), format!(
        "require('fs').writeFileSync('document.txt', 'background'); require('http').createServer((q, r) => r.end('ready')).listen({port}, '127.0.0.1');"
    ))?;
    let state = crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
    let conn = crate::agent_service::open_db(&root)?;
    conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)", [])?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let session = super::super::Session {
        agent_id: None, task_node_id: None, plugin: None,
        state: &state, task: "task", model: "fake", turn: 1, cancel: &cancel,
        ban_execution: false, scope: "main".into(), parent: None, input_budget: 100_000,
    };
    super::super::business_tool(&session, "node", "read_file", json!({"path":"document.txt"}), "read").await?;
    let launch = json!({"project_path":".","script":"dev","background":true,"ready_url":format!("http://127.0.0.1:{port}/"),"wait_seconds":30});
    let launched = match super::super::business_tool_inner(&session, "node", "run_project_script", launch, "launch", true, false).await {
        Ok(launched) => launched,
        // Node and npm are not installed here.
        Err(error) if format!("{error:#}").contains("runtime_bootstrap") => {
            drop(conn);
            drop(state);
            let _ = fs::remove_dir_all(root);
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let written = fs::read_to_string(&path)?;
    let conflict = super::super::business_tool(&session, "node", "edit_file", json!({"path":"document.txt","edits":[{"old_text":"background","new_text":"agent"}]}), "edit").await?;
    super::super::business_tool_inner(&session, "node", "stop_project_process", json!({"process_id":launched["process_id"]}), "stop", true, false).await?;
    assert_eq!(launched["running"], true, "{launched}");
    assert_eq!(written, "background");
    assert_eq!(conflict["error_code"], "ASSET_CONFLICT", "{conflict}");
    assert_eq!(fs::read_to_string(&path)?, "background");
    drop(conn);
    drop(state);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

