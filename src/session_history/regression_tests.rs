use super::*;
use rusqlite::{Connection, params};
fn setup() -> (std::path::PathBuf, Connection) {
    let root = std::env::temp_dir().join(format!(
        "review-repro-{}",
        crate::agent_service::uuid_like()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let c = crate::agent_service::open_db(&root).unwrap();
    for task in ["a", "b"] {
        c.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES (?1,'test','test','running',1,1)",[task]).unwrap();
    }
    (root, c)
}
fn event(c: &Connection, task: &str, seq: i64, time: i64, kind: &str, data: Value) {
    c.execute(
        "INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES (?1,?2,?3,?4,?5)",
        params![task, seq, time, kind, data.to_string()],
    )
    .unwrap();
}
#[tokio::test]
async fn review_1_project_reference_hidden_by_snapshot() {
    let (root, c) = setup();
    event(
        &c,
        "a",
        1,
        100,
        "user/message",
        json!({"turn":1,"message":{"content":"start"}}),
    );
    event(
        &c,
        "b",
        1,
        150,
        "user/message",
        json!({"turn":1,"message":{"content":"needle_rare"}}),
    );
    event(
        &c,
        "a",
        2,
        200,
        "tool/call",
        json!({"name":"read_session_history","callId":"h1","arguments":"{}"}),
    );
    event(
        &c,
        "b",
        2,
        100,
        "user/message",
        json!({"turn":1,"message":{"content":"needle_rare newer"}}),
    );
    event(
        &c,
        "a",
        3,
        400,
        "tool/call",
        json!({"name":"read_session_history","callId":"h2","arguments":"{}"}),
    );
    let result = read_for_call(
        &root,
        "a",
        &json!({"scope":"project","query":"needle_rare"}),
        Some("h1"),
    )
    .await
    .unwrap();
    let reference = result["records"][0]["read_reference"].clone();
    let readback = read_for_call(&root, "a", &reference, Some("h2"))
        .await
        .unwrap();
    assert_eq!(reference["task_id"], "b");
    assert_eq!(reference["event_seq"], 1);
    assert_eq!(reference["snapshot_seq"], 1);
    assert_eq!(readback["records"][0]["seq"], 1);
}
#[tokio::test]
async fn review_2_reused_call_id_hides_non_history_tool_result() {
    let (root, c) = setup();
    event(
        &c,
        "a",
        1,
        1,
        "tool/call",
        json!({"name":"read_session_history","callId":"same","arguments":"{}"}),
    );
    event(
        &c,
        "a",
        2,
        2,
        "tool/result",
        json!({"message":{"toolCallId":"same"},"meta":{"result":{"records":[]}}}),
    );
    event(
        &c,
        "a",
        3,
        3,
        "tool/call",
        json!({"name":"read_file","callId":"same","arguments":"{}"}),
    );
    event(
        &c,
        "a",
        4,
        4,
        "tool/result",
        json!({"message":{"toolCallId":"same"},"meta":{"result":{"data":"real"}}}),
    );
    let normal = read(&root, "a", &json!({"event_seq":4})).await.unwrap();
    let audit = read(
        &root,
        "a",
        &json!({"event_seq":4,"audit_history_reads":true}),
    )
    .await
    .unwrap();
    assert_eq!(normal["records"][0]["seq"], 4);
    assert_eq!(audit["records"][0]["seq"], 4);
}
#[tokio::test]
async fn review_3_resumed_task_has_no_turn_two_directory() {
    let (root, c) = setup();
    event(
        &c,
        "a",
        1,
        1,
        "user/message",
        json!({"turn":1,"model_content":"first","message":{"content":"first"}}),
    );
    event(
        &c,
        "a",
        2,
        2,
        "task/node_planned",
        json!({"turn":1,"kind":"task","node_id":"main","parent_id":"n0","title":"working"}),
    );
    event(
        &c,
        "a",
        3,
        3,
        "user/message",
        json!({"turn":2,"model_content":"continue","message":{"content":"continue"}}),
    );
    event(
        &c,
        "a",
        4,
        4,
        "task/node_resumed",
        json!({"turn":2,"kind":"task","node_id":"main","parent_id":"n0","title":"working"}),
    );
    event(
        &c,
        "a",
        5,
        5,
        "tool/call",
        json!({"turn":2,"task_node_id":"main","name":"read_file","callId":"c","arguments":"{}"}),
    );
    let r = read(&root, "a", &json!({"view":"index","turn":2}))
        .await
        .unwrap();
    assert_eq!(r["entries"].as_array().unwrap().len(), 2);
    assert_eq!(r["entries"][1]["kind"], "task");
    let details = read(&root, "a", &r["entries"][1]["expand_this_turn"])
        .await
        .unwrap();
    assert_eq!(details["entries"][0]["seq"], 5);
}
#[tokio::test]
async fn continued_active_task_without_resumed_event_remains_navigable() {
    let (root, c) = setup();
    event(
        &c,
        "a",
        1,
        1,
        "user/message",
        json!({"turn":1,"message":{"content":"first"}}),
    );
    event(
        &c,
        "a",
        2,
        2,
        "task/node_planned",
        json!({"turn":1,"kind":"task","node_id":"main","parent_id":"n0","title":"work"}),
    );
    event(
        &c,
        "a",
        3,
        3,
        "task/node_planned",
        json!({"turn":1,"kind":"task","node_id":"child","parent_id":"main","title":"child work"}),
    );
    event(
        &c,
        "a",
        4,
        4,
        "user/message",
        json!({"turn":2,"message":{"content":"continue"}}),
    );
    event(
        &c,
        "a",
        5,
        5,
        "tool/call",
        json!({"turn":2,"task_node_id":"child","name":"read_file","callId":"c","arguments":"{}"}),
    );
    event(
        &c,
        "a",
        6,
        6,
        "assistant/message",
        json!({"turn":2,"task_node_id":"child","message":{"content":"done"}}),
    );
    let turn = read(&root, "a", &json!({"view":"index","turn":2,"limit":1}))
        .await
        .unwrap();
    assert_eq!(turn["entries"][0]["kind"], "user");
    let page = read(&root, "a", &turn["next_page"]).await.unwrap();
    assert_eq!(page["entries"][0]["node_id"], "main");
    assert_eq!(page["entries"][0]["inferred_from_activity"], true);
    let parent = read(&root, "a", &page["entries"][0]["expand_this_turn"])
        .await
        .unwrap();
    assert_eq!(parent["entries"][0]["node_id"], "child");
    let child = read(&root, "a", &parent["entries"][0]["expand_this_turn"])
        .await
        .unwrap();
    assert_eq!(child["entries"][0]["label"], "read_file");
    assert_eq!(child["entries"][1]["label"], "done");
    let before = read(
        &root,
        "a",
        &json!({"view":"index","turn":2,"snapshot_seq":4}),
    )
    .await
    .unwrap();
    assert_eq!(before["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn review_4_programs_after_seq_is_ignored() {
    let (root, c) = setup();
    for seq in 1..=3 {
        event(
            &c,
            "a",
            seq,
            seq,
            "program/start",
            json!({"turn":1,"source":format!("code {seq}")}),
        );
    }
    let first = read(&root, "a", &json!({"programs":true,"limit":1}))
        .await
        .unwrap();
    let next = read(&root, "a", &first["next_page"]).await.unwrap();
    assert_eq!(first["programs"][0]["event_seq"], 1);
    assert_eq!(first["more"], true);
    assert_eq!(next["programs"][0]["event_seq"], 2);
}
#[tokio::test]
async fn review_5_visual_async_association_uses_wrong_path() {
    let (root, c) = setup();
    event(
        &c,
        "a",
        1,
        1,
        "tool/call",
        json!({"turn":1,"name":"analyze_images","callId":"v1","arguments":"{}"}),
    );
    event(
        &c,
        "a",
        2,
        2,
        "tool/result",
        json!({"message":{"toolCallId":"v1"},"meta":{"result":{"status":"pending"}}}),
    );
    event(
        &c,
        "a",
        3,
        3,
        "tool/async_result",
        json!({"tool":"analyze_images","original_call_id":"v1","meta":{"result":{"answer":"visible"}}}),
    );
    let r = read(&root, "a", &json!({"event_seq":1})).await.unwrap();
    assert_eq!(r["records"][0]["tool_result"]["seq"], 3);
    let directory = read(&root, "a", &json!({"view":"index","turn":1}))
        .await
        .unwrap();
    assert_eq!(directory["entries"][0]["result_reference"]["event_seq"], 3);
    assert!(
        r["records"][0]["tool_result"]["content"]
            .as_str()
            .unwrap()
            .contains("visible")
    );
}

#[tokio::test]
async fn overlapping_reused_ids_do_not_hide_an_unrelated_result() {
    let (root, c) = setup();
    event(
        &c,
        "a",
        1,
        1,
        "tool/call",
        json!({"name":"read_file","callId":"same"}),
    );
    event(
        &c,
        "a",
        2,
        2,
        "tool/call",
        json!({"name":"read_session_history","callId":"same"}),
    );
    event(
        &c,
        "a",
        3,
        3,
        "tool/result",
        json!({"message":{"toolCallId":"same"},"meta":{"result":{"data":"ordinary"}}}),
    );
    let result = read(&root, "a", &json!({"event_seq":3})).await.unwrap();
    assert_eq!(result["records"][0]["seq"], 3);
    assert!(result["records"][0].get("tool_call").is_none());
    let first = read(&root, "a", &json!({"event_seq":1})).await.unwrap();
    assert!(first["records"][0].get("tool_result").is_none());
}
