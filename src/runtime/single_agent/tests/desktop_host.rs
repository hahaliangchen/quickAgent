use super::{
    TestWorkspace, completion, init_task_row, last_tool_output, run_js, spawn_mock_provider,
};
use crate::server::{
    agent_service::{self, PermissionMode},
    builtin_plugins::desktop::tests::mock_catalog,
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn desktop_js_vm_dispatches_and_keeps_contracts_out_of_the_static_header() {
    let work = TestWorkspace::new("desktop-vm");
    let task = "desktop-vm";
    init_task_row(work.path(), task);
    let (_runtime, catalog) = mock_catalog();
    let (mut state,captured,server) = spawn_mock_provider(work.path(),|step, request| match step {
        0 => run_js("const row=desktop.list_windows({}).windows[0]; const types=host.contracts({methods:['desktop.mouse']}); [row.title, types.declarations];"),
        1 => {
            let last = last_tool_output(&request);
            assert!(last.contains("Editor") && last.contains("mouse"), "{last}");
            run_js("await desktop.promises.mouse({action:'click',x:0,y:0,window_id:'x1'});")
        }
        2 => {
            let last = last_tool_output(&request);
            assert!(last.contains("permission_denied"), "{last}");
            run_js("const out=await desktop.promises.screenshot_window({window_id:'x1',grid:true}); out.artifact_id;")
        }
        3 => {
            let last = last_tool_output(&request);
            assert!(last.contains("visual-"), "{last}");
            completion("done")
        }
        _ => panic!("unexpected request {step}"),
    }).await;
    state.permission_mode = PermissionMode::ReadOnly;
    state.tool_catalog = Some(catalog);
    super::super::run_task(
        state,
        task.into(),
        "fake".into(),
        "desktop regression".into(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let header = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(header.contains("desktop") && !header.contains("screenshot_window"));
    let artifacts = crate::media::visual_artifacts::task_artifact_ids(work.path(), task).unwrap();
    assert_eq!(artifacts.len(), 1);
}

#[tokio::test]
async fn desktop_write_requires_a_fresh_human_decision_in_ask_and_auto_review() {
    let work = TestWorkspace::new("desktop-approval");
    let (_runtime, catalog) = mock_catalog();
    for (mode, task) in [
        (PermissionMode::Ask, "desktop-ask"),
        (PermissionMode::AutoReview, "desktop-review"),
    ] {
        init_task_row(work.path(), task);
        let (mut state, captured, server) =
            spawn_mock_provider(work.path(), |_, _| completion("unexpected reviewer call")).await;
        state.permission_mode = mode;
        state.tool_catalog = Some(catalog.clone());
        let db = agent_service::open_db(work.path()).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS tool_approval_grants(scope TEXT PRIMARY KEY)")
            .unwrap();
        db.execute(
            "INSERT INTO tool_approval_grants(scope) VALUES (?1)",
            [format!("task:{task}")],
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let session = super::super::Session {
            agent_id: None,
            task_node_id: None,
            plugin: None,
            state: &state,
            task,
            model: "fake",
            turn: 1,
            cancel: &cancel,
            ban_execution: false,
            scope: "main".into(),
            parent: None,
            input_budget: 100_000,
        };
        let raw = super::super::business_tool(
            &session,
            "node",
            "desktop__mouse",
            json!({"action":"click","window_id":"x1","x":1,"y":2}),
            "call",
        );
        let deny = async {
            loop {
                let mut stmt = db.prepare("SELECT data FROM agent_task_events WHERE task_id=?1 AND kind='approval/request'").unwrap();
                let mut rows = stmt.query([task]).unwrap();
                if let Some(row) = rows.next().unwrap() {
                    let data: Value =
                        serde_json::from_str(&row.get::<_, String>(0).unwrap()).unwrap();
                    assert_eq!(data["once_only"], true);
                    assert_eq!(data["tool"], "desktop__mouse");
                    db.execute(
                        "INSERT INTO tool_approvals VALUES (?1,'call','deny')",
                        [task],
                    )
                    .unwrap();
                    break;
                }
                drop(rows);
                drop(stmt);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(raw, deny) })
                .await
                .unwrap();
        assert!(result.unwrap_err().to_string().contains("USER_DENIED"));
        assert!(captured.lock().unwrap().is_empty());
        server.abort();
    }
}
