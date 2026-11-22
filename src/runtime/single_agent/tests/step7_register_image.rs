//! Local image registration: a screenshot a test, a script or a Playwright run
//! already saved must reach the visual chain without a browser round trip.
use super::{TestWorkspace, completion, init_task_row, run_js, spawn_mock_provider, tool_call_count};
use crate::server::agent_service;
use crate::runtime::single_agent::run_task;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn fixture_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

fn context(task: &str) -> crate::media::visual_artifacts::VisualContext {
    crate::media::visual_artifacts::VisualContext {
        identity: json!({"task_id": task, "request_id": 1, "node_id": "register-image-test"}),
        turn: Some(1),
        ..Default::default()
    }
}

fn state(ws: &TestWorkspace) -> agent_service::AgentServiceState {
    agent_service::tests::test_state(ws.path(), "127.0.0.1:0".parse().unwrap())
}

#[tokio::test]
async fn js_vm_register_image_makes_a_workspace_screenshot_analysable() {
    let ws = TestWorkspace::new("cac-v2-register-image");
    init_task_row(ws.path(), "register-image");
    std::fs::write(ws.path().join("shot.png"), fixture_png(20, 10)).unwrap();
    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, _body| match idx {
        0 => run_js(
            "const registered = await vision.register_image({ path: \"shot.png\" }); registered;",
        ),
        1 => run_js(
            "const viewed = await vision.view_image({ artifact_id: registered.artifact_id }); viewed.visual_artifact.artifact_id;",
        ),
        2 => completion("登记并读取了图片"),
        other => panic!("unexpected round {other}"),
    })
    .await;
    run_task(
        state,
        "register-image".into(),
        "fake".into(),
        "看看这张截图".into(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    // 1. The call is a host tool call, recorded like any other.
    assert_eq!(tool_call_count(ws.path(), "register-image", "register_image"), 1);
    assert_eq!(tool_call_count(ws.path(), "register-image", "view_image"), 1);

    // 2. It saved exactly one task-owned artifact holding the file's pixels.
    let ids = crate::media::visual_artifacts::task_artifact_ids(ws.path(), "register-image").unwrap();
    assert_eq!(ids.len(), 1, "{ids:?}");
    let meta = crate::media::visual_artifacts::metadata(ws.path(), "register-image", &ids[0]).unwrap();
    assert_eq!(meta["source"], "vision.register_image");
    assert_eq!(meta["width"], 20);
    assert_eq!(meta["height"], 10);
    assert_eq!(meta["normalization"]["source_width"], 20);
    assert_eq!(meta["mime_type"], "image/png");

    // 3. The visual chain reads those registered pixels, task-scoped.
    let (read, bytes) =
        crate::media::visual_artifacts::read(ws.path(), "register-image", &ids[0]).unwrap();
    assert_eq!(read["artifact_id"], ids[0]);
    assert!(!bytes.is_empty());
    assert!(
        crate::media::visual_artifacts::read(ws.path(), "other-task", &ids[0]).is_err(),
        "another task must not read it"
    );
    assert_eq!(
        super::assistant_messages(ws.path(), "register-image"),
        vec!["登记并读取了图片"]
    );
}

#[tokio::test]
async fn a_screenshot_in_the_temp_directory_registers_from_an_absolute_path() {
    let ws = TestWorkspace::new("cac-v2-register-image-temp");
    init_task_row(ws.path(), "temp-image");
    let file = std::env::temp_dir().join(format!(
        "register-image-{}.png",
        agent_service::uuid_like()
    ));
    std::fs::write(&file, fixture_png(8, 6)).unwrap();
    let state = state(&ws);
    let out = crate::media::register_image::execute(
        &state,
        &context("temp-image"),
        &json!({"path": file.display().to_string()}),
    )
    .await
    .unwrap();
    let _ = std::fs::remove_file(&file);

    assert_eq!(out["width"], 8);
    assert_eq!(out["height"], 6);
    assert_eq!(out["source_width"], 8);
    let id = out["artifact_id"].as_str().unwrap();
    assert_eq!(out["visual_artifact"]["artifact_id"], id);
    assert!(
        crate::media::visual_artifacts::metadata(ws.path(), "temp-image", id).is_ok(),
        "the registered copy belongs to the conversation"
    );
}

#[tokio::test]
async fn register_image_refuses_non_images_missing_files_and_outside_paths() {
    let ws = TestWorkspace::new("cac-v2-register-image-refused");
    init_task_row(ws.path(), "refused");
    std::fs::write(ws.path().join("notes.txt"), b"just text").unwrap();
    let state = state(&ws);
    let call = async |path: String| -> String {
        format!(
            "{:#}",
            crate::media::register_image::execute(&state, &context("refused"), &json!({"path": path}))
                .await
                .unwrap_err()
        )
    };

    // A real file that is not a picture, decided by its bytes and not its name.
    let error = call(
        ws.path().join("notes.txt").display().to_string()
    )
    .await;
    assert!(error.contains("not a supported image file"), "{error}");

    let renamed = ws.path().join("pretend.png");
    std::fs::write(&renamed, b"still not an image").unwrap();
    let error = call(renamed.display().to_string()).await;
    assert!(
        error.contains("not a supported image file"),
        "an image extension must not launder non-image bytes: {error}"
    );

    // Nothing was registered by the refusals.
    let error = call("missing.png".into()).await;
    assert!(error.contains("does not exist"), "{error}");
    assert!(
        crate::media::visual_artifacts::task_artifact_ids(ws.path(), "refused")
            .unwrap()
            .is_empty()
    );

    // A path outside the workspace and the temp directory is refused before any
    // bytes are read. Guarded, because a build directory may itself live in tmp.
    let outside = std::env::current_exe().unwrap().canonicalize().unwrap();
    let temp = std::env::temp_dir().canonicalize().unwrap();
    let workspace = ws.path().canonicalize().unwrap();
    if !outside.starts_with(&temp) && !outside.starts_with(&workspace) {
        let error = call(outside.display().to_string()).await;
        assert!(
            error.contains("must stay inside the workspace or the system temp directory"),
            "{error}"
        );
    }
}
