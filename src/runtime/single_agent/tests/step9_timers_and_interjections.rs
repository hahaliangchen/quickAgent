//! Regression tests for:
//! 1. Production JS-VM timer driving (`setTimeout`) and graceful recovery on unsettled Promises.
//! 2. Mid-turn user interjections delivered append-only without breaking KV-cache prefix bytes.
//! 3. Cross-project external path detection for newer file, verify, and git host tools.

use super::{
    TestWorkspace, assistant_messages, completion, init_task_row, run_js, spawn_mock_provider,
};
use crate::runtime::single_agent::{
    host_tools::{detect_outside_edit, outside_exec_or_index_paths, outside_read_paths},
    run_task,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn js_vm_set_timeout_resolves_cleanly_in_production_driver() {
    let ws = TestWorkspace::new("cac-v2-timer-driver");
    std::fs::write(ws.path().join("hello.txt"), "hi").unwrap();
    init_task_row(ws.path(), "timer-task");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, _body| match idx {
        0 => run_js(
            "const f = io.read({ path: \"hello.txt\" });\n\
             const woke = await new Promise(r => setTimeout(() => r(\"woke:\" + f.slice(1, 1)), 25));\n\
             woke;",
        ),
        1 => completion("定时器执行完成"),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "timer-task".into(),
        "fake".into(),
        "测试 setTimeout".into(),
        6,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let step1_tool_output = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    assert!(
        step1_tool_output.contains("woke:"),
        "expected timer resolution in tool output, got: {step1_tool_output}"
    );
    assert_eq!(
        assistant_messages(ws.path(), "timer-task"),
        vec!["定时器执行完成"]
    );
}

#[tokio::test]
async fn js_vm_unsettled_promise_recovers_as_program_error_instead_of_killing_task() {
    let ws = TestWorkspace::new("cac-v2-unsettled-promise");
    init_task_row(ws.path(), "deadlock-task");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, _body| match idx {
        0 => run_js("await new Promise(() => {});"),
        1 => completion("已从挂起 Promise 中恢复"),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "deadlock-task".into(),
        "fake".into(),
        "测试未 resolve 的 Promise".into(),
        6,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let step1_tool_output = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    assert!(
        step1_tool_output.contains("unresolved promise")
            || step1_tool_output.contains("UNSETTLED_PROMISE"),
        "expected unresolved promise recovery error, got: {step1_tool_output}"
    );
    assert_eq!(
        assistant_messages(ws.path(), "deadlock-task"),
        vec!["已从挂起 Promise 中恢复"]
    );
}

#[tokio::test]
async fn js_vm_excessive_timer_recovers_as_program_error_instead_of_hanging() {
    let ws = TestWorkspace::new("cac-v2-timer-limit");
    init_task_row(ws.path(), "timer-limit-task");

    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, _body| match idx {
        0 => run_js("await new Promise(r => setTimeout(r, 600000));"),
        1 => completion("已改用短等待"),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "timer-limit-task".into(),
        "fake".into(),
        "测试超长定时器保护".into(),
        6,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let step1_tool_output = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    assert!(
        step1_tool_output.contains("TIMER_LIMIT_EXCEEDED"),
        "expected TIMER_LIMIT_EXCEEDED recovery error, got: {step1_tool_output}"
    );
    assert_eq!(
        assistant_messages(ws.path(), "timer-limit-task"),
        vec!["已改用短等待"]
    );
}

#[tokio::test]
async fn mid_turn_interjection_is_delivered_append_only_and_catches_final_reply() {
    let ws = TestWorkspace::new("cac-v2-interjection");
    init_task_row(ws.path(), "interject-task");
    let conn = crate::server::agent_service::open_db(ws.path()).unwrap();
    conn.execute(
        "UPDATE agent_tasks SET status='running' WHERE id='interject-task'",
        [],
    )
    .unwrap();
    drop(conn);

    let root_for_mock = ws.path().to_path_buf();
    let (state, captured, server) = spawn_mock_provider(ws.path(), move |idx, _body| match idx {
        0 => {
            // Submit an interjection while the first JS step is about to run.
            crate::context::interjections::submit(&root_for_mock, "interject-task", "改用 B 方案")
                .unwrap();
            run_js("const x = 1 + 1; x;")
        }
        1 => {
            // Submit a second interjection just as the model tries to emit a FinalReply.
            crate::context::interjections::submit(
                &root_for_mock,
                "interject-task",
                "再顺便检查下日志",
            )
            .unwrap();
            completion("已切到 B 方案")
        }
        2 => completion("已切到 B 方案并检查了日志"),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "interject-task".into(),
        "fake".into(),
        "先按 A 方案做".into(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 3, "second interjection must intercept FinalReply and run round 2");

    let req0_msgs = requests[0]["messages"].as_array().unwrap();
    let req1_msgs = requests[1]["messages"].as_array().unwrap();
    let req2_msgs = requests[2]["messages"].as_array().unwrap();

    // KV-cache prefix invariant: earlier messages stay byte-identical across steps.
    assert_eq!(&req1_msgs[..req0_msgs.len()], &req0_msgs[..]);
    assert_eq!(&req2_msgs[..req1_msgs.len()], &req1_msgs[..]);

    // First interjection arrived as a `user` message right after step 1's tool response.
    let last_req1 = req1_msgs.last().unwrap();
    assert_eq!(last_req1["role"], "user");
    assert!(
        last_req1["content"]
            .as_str()
            .unwrap_or_default()
            .contains("改用 B 方案")
    );

    // Second interjection preserved the draft reply and appended the follow-up user message.
    assert_eq!(req2_msgs[req2_msgs.len() - 2]["role"], "assistant");
    assert_eq!(req2_msgs[req2_msgs.len() - 2]["content"], "已切到 B 方案");
    let last_req2 = req2_msgs.last().unwrap();
    assert_eq!(last_req2["role"], "user");
    assert!(
        last_req2["content"]
            .as_str()
            .unwrap_or_default()
            .contains("再顺便检查下日志")
    );

    // All interjections are marked delivered in SQLite.
    let conn = crate::server::agent_service::open_db(ws.path()).unwrap();
    assert!(!crate::context::interjections::pending(&conn, "interject-task").unwrap());
    assert_eq!(
        assistant_messages(ws.path(), "interject-task"),
        vec!["已切到 B 方案并检查了日志"]
    );
}

#[test]
fn outside_workspace_helpers_cover_new_file_verify_and_git_tools() {
    let ws_main = TestWorkspace::new("cac-v2-cross-main");
    let ws_other = TestWorkspace::new("cac-v2-cross-other");
    std::fs::write(ws_other.path().join("app.ts"), "export const x = 1;\n").unwrap();
    let workspace = crate::tools::Workspace::new(ws_main.path()).unwrap();

    let other_file = ws_other.path().join("app.ts");
    let other_dir = ws_other.path().join("new_sub");

    // delete_path & create_dir detected as outside edits.
    let del = detect_outside_edit(
        &workspace,
        "delete_path",
        &json!({"path": other_file.to_string_lossy()}),
    )
    .unwrap();
    assert!(del.is_some(), "delete_path outside workspace must be detected");

    let mk = detect_outside_edit(
        &workspace,
        "create_dir",
        &json!({"path": other_dir.to_string_lossy()}),
    )
    .unwrap();
    assert!(mk.is_some(), "create_dir outside workspace must be detected");

    // verify_check / verify_test / rename_path / apply_edits / git_commit detected as outside exec paths.
    for (tool, args) in [
        ("verify_check", json!({"path": ws_other.path().to_string_lossy()})),
        ("verify_test", json!({"path": ws_other.path().to_string_lossy()})),
        (
            "rename_path",
            json!({"path": other_file.to_string_lossy(), "to": ws_other.path().join("app2.ts").to_string_lossy()}),
        ),
        (
            "apply_edits",
            json!({"files": [{"path": other_file.to_string_lossy(), "edits": []}]}),
        ),
        (
            "git_commit",
            json!({"paths": [other_file.to_string_lossy()], "message": "test"}),
        ),
    ] {
        let found = outside_exec_or_index_paths(workspace.root(), tool, &args);
        assert!(
            !found.is_empty(),
            "outside_exec_or_index_paths must detect external target for {tool}"
        );
    }

    // git_blame detected as outside read path.
    let blame = outside_read_paths(
        workspace.root(),
        &[],
        "git_blame",
        &json!({"path": other_file.to_string_lossy(), "start_line": 1, "end_line": 1}),
    );
    assert!(!blame.is_empty(), "git_blame outside workspace must be detected");
}
