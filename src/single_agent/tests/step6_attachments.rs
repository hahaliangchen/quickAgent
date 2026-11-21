//! Attachment injection tests for Context-as-Code v2 single Agent turns:
//! admitted uploads must reach the frontend timeline, the model context, and the
//! JS VM read paths (attachments.read_attachment, vision.analyze_images).
use super::{
    TestWorkspace, assert_single_run_js_tool, completion, init_task_row, last_tool_output, run_js,
    spawn_mock_provider, tool_call_count,
};
use crate::agent_service;
use crate::single_agent::run_task;
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{TransactionBehavior, params};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn fixture_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(20, 10)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

/// The user/message content the frontend timeline renders for one turn.
fn user_message_content(root: &std::path::Path, task: &str, turn: usize) -> Value {
    let conn = agent_service::open_db(root).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT data FROM agent_task_events WHERE task_id=?1 AND kind='user/message' AND json_extract(data,'$.turn')=?2 ORDER BY seq LIMIT 1",
            params![task, turn as i64],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str::<Value>(&raw).unwrap()["message"]["content"].clone()
}

#[tokio::test]
async fn png_attachment_reaches_the_timeline_and_the_model_with_a_visual_artifact() {
    let ws = TestWorkspace::new("cac-v2-attach-image");
    init_task_row(ws.path(), "attach-image");
    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, body| match idx {
        0 => {
            assert_single_run_js_tool(&body);
            completion("图片已登记")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;
    let image = STANDARD.encode(fixture_png());
    let mut conn = agent_service::open_db(ws.path()).unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let admitted = crate::attachments::admit(
        &tx,
        ws.path(),
        "attach-image",
        1,
        "这是什么图?",
        &[json!({"type":"image","mediaType":"image/png","name":"image.png","data":image})],
        Some("request-image"),
        None,
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(admitted[1]["type"], "image");

    run_task(
        state,
        "attach-image".into(),
        "fake".into(),
        "这是什么图?".into(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    // 1. The frontend bubble keeps the admitted image block instead of bare text.
    let content = user_message_content(ws.path(), "attach-image", 1);
    let parts = content
        .as_array()
        .expect("message content must stay a part list");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "这是什么图?");
    assert_eq!(parts[1]["type"], "image", "{parts:?}");
    assert_eq!(parts[1]["attachment"]["name"], "image.png");
    assert_eq!(parts[1]["attachment"]["width"], 20);

    // 2. The model receives the attachment metadata with the v2 JS-VM call hints.
    let bodies = captured.lock().unwrap();
    let user = bodies[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        user.contains("User attachment (untrusted source material)"),
        "outbound turns must carry the attachment description: {user}"
    );
    assert!(user.contains("image.png"), "{user}");
    assert!(
        user.contains("vision.analyze_images"),
        "an image hint must name the v2 JS-VM vision call: {user}"
    );
    assert!(
        user.contains("attachments.read_attachment"),
        "an image hint must still name the attachment read call: {user}"
    );
    let ids = crate::visual_artifacts::task_artifact_ids(ws.path(), "attach-image").unwrap();
    assert_eq!(
        ids.len(),
        1,
        "the turn registers exactly one visual artifact: {ids:?}"
    );
    assert!(
        user.contains(&ids[0]),
        "the model must receive the artifact id it can inspect: {user}"
    );

    // 3. That artifact is reachable through the visual pipeline, task-scoped.
    assert!(crate::visual_artifacts::metadata(ws.path(), "attach-image", &ids[0]).is_ok());
    assert!(crate::visual_artifacts::metadata(ws.path(), "other-task", &ids[0]).is_err());
    let context = crate::visual_artifacts::VisualContext {
        identity: json!({"task_id":"attach-image","request_id":2,"runtime":"single_agent"}),
        allowed_artifact_ids: ids.clone(),
        ..Default::default()
    };
    let (block, _) =
        crate::visual_artifacts::image_block(ws.path(), &context, &ids[0], true).unwrap();
    assert_eq!(block["type"], "image_url");
    assert!(
        block["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    drop(bodies);
    server.abort();
}

#[tokio::test]
async fn picture_only_turn_with_an_empty_prompt_keeps_its_attachment_context() {
    let ws = TestWorkspace::new("cac-v2-attach-image-only");
    init_task_row(ws.path(), "attach-image-only");
    let (state, captured, server) = spawn_mock_provider(ws.path(), |idx, _body| match idx {
        0 => completion("收到图片"),
        other => panic!("unexpected round {other}"),
    })
    .await;
    let image = STANDARD.encode(fixture_png());
    let mut conn = agent_service::open_db(ws.path()).unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let admitted = crate::attachments::admit(
        &tx,
        ws.path(),
        "attach-image-only",
        1,
        "",
        &[json!({"type":"image","mediaType":"image/png","name":"image.png","data":image})],
        None,
        None,
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        admitted.len(),
        1,
        "an image-only message has no text part: {admitted:?}"
    );

    run_task(
        state,
        "attach-image-only".into(),
        "fake".into(),
        String::new(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    let content = user_message_content(ws.path(), "attach-image-only", 1);
    let parts = content.as_array().unwrap();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(parts[0]["type"], "image");

    let bodies = captured.lock().unwrap();
    let user = bodies[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        user.contains("vision.analyze_images"),
        "an image-only turn must still explain how to look at the picture: {user}"
    );
    let ids = crate::visual_artifacts::task_artifact_ids(ws.path(), "attach-image-only").unwrap();
    assert_eq!(ids.len(), 1);
    assert!(user.contains(&ids[0]), "{user}");
    drop(bodies);
    server.abort();
}

#[tokio::test]
async fn text_attachment_is_readable_from_run_js_through_attachments_read_attachment() {
    const TEXT: &str = "第一段内容\n第二段材料";
    let ws = TestWorkspace::new("cac-v2-attach-file");
    init_task_row(ws.path(), "attach-file");
    let seed = agent_service::tests::test_state(ws.path(), "127.0.0.1:9".parse().unwrap());
    let uploaded =
        crate::attachments::store(&seed, "attach-file", "brief.md", TEXT.as_bytes()).unwrap();
    let attachment_id = uploaded["file"]["attachmentId"].as_str().unwrap().to_owned();
    let read = format!(
        "const doc = await attachments.read_attachment({{ attachment_id: \"{attachment_id}\" }});\nreturn doc.content;"
    );
    let (state, captured, server) = spawn_mock_provider(ws.path(), move |idx, body| match idx {
        0 => {
            assert_single_run_js_tool(&body);
            let user = body["messages"][1]["content"].as_str().unwrap();
            assert!(
                user.contains("User attachment (untrusted source material)"),
                "the model must learn about the uploaded file: {user}"
            );
            assert!(
                user.contains("attachments.read_attachment"),
                "the file hint must name the v2 JS-VM read call: {user}"
            );
            assert!(user.contains("brief.md"), "{user}");
            run_js(&read)
        }
        1 => {
            let output = last_tool_output(&body);
            assert!(
                output.contains("第一段内容") && output.contains("第二段材料"),
                "run_js must read the attachment content: {output}"
            );
            completion("已读取材料")
        }
        other => panic!("unexpected round {other}"),
    })
    .await;
    let mut conn = agent_service::open_db(ws.path()).unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let admitted = crate::attachments::admit(
        &tx,
        ws.path(),
        "attach-file",
        1,
        "读材料",
        &[json!({"type":"file","receiptId":uploaded["receiptId"]})],
        Some("request-file"),
        None,
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(admitted[1]["type"], "file");
    assert_eq!(admitted[1]["attachment"]["attachmentId"], attachment_id);

    run_task(
        state,
        "attach-file".into(),
        "fake".into(),
        "读材料".into(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();

    let content = user_message_content(ws.path(), "attach-file", 1);
    let parts = content.as_array().unwrap();
    assert_eq!(parts[1]["type"], "file");
    assert_eq!(parts[1]["attachment"]["name"], "brief.md");
    let bodies = captured.lock().unwrap();
    let user = bodies[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        user.contains(&attachment_id),
        "the model must receive the attachment_id it can read: {user}"
    );
    // The hint is JSON-serialized inside the description, so assert the call shape
    // rather than the exact quoting of the id.
    assert!(
        user.contains("await attachments.read_attachment({ attachment_id"),
        "the model must be told the exact v2 call to make: {user}"
    );
    drop(bodies);
    assert_eq!(
        tool_call_count(ws.path(), "attach-file", "read_attachment"),
        1
    );
    server.abort();
}
