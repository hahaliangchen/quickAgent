use super::{TestWorkspace, completion, init_task_row, run_js};
use crate::runtime::single_agent::run_task;
use axum::{Json, Router, body::Bytes, routing::post};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageFormat;
use serde_json::{Value, json};
use std::{io::Cursor, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use tokio_util::sync::CancellationToken;

fn fixture_png() -> Vec<u8> {
    let image = image::DynamicImage::new_rgb8(10, 7);
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    bytes
}

#[tokio::test]
async fn js_vm_generate_image_then_view_image_round_trip() {
    let ws = TestWorkspace::new("cac-v2-image-js");
    init_task_row(ws.path(), "image-js-task");
    let chat_turn = Arc::new(AtomicUsize::new(0));
    let image = STANDARD.encode(fixture_png());
    let app = Router::new()
        .route("/v1/chat/completions", post({
            let chat_turn = chat_turn.clone();
            move |Json(_body): Json<Value>| {
                let index = chat_turn.fetch_add(1, Ordering::SeqCst);
                async move {
                    Json(match index {
                        0 => run_js("const generated = await vision.generate_image({ prompt: \"a test icon\" }); generated;"),
                        1 => run_js("const viewed = await vision.view_image({ artifact_id: generated.artifact_id }); viewed;"),
                        2 => completion("generated and viewed"),
                        other => panic!("unexpected chat request {other}"),
                    })
                }
            }
        }))
        .route("/v1/images/generations", post(move |_body: Bytes| {
            let image = image.clone();
            async move { Json(json!({"data":[{"b64_json":image}]})) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut state = crate::server::agent_service::tests::test_state(ws.path(), address);
    state.provider_url = format!("http://{address}/v1");
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    run_task(state, "image-js-task".into(), "fake-model".into(), "Generate and inspect an image".into(), 8, CancellationToken::new(), 1, false).await.unwrap();
    server.abort();
    assert_eq!(chat_turn.load(Ordering::SeqCst), 3);
    assert_eq!(super::tool_call_count(ws.path(), "image-js-task", "generate_image"), 1);
    assert_eq!(super::tool_call_count(ws.path(), "image-js-task", "view_image"), 1);
    assert_eq!(super::assistant_messages(ws.path(), "image-js-task"), vec!["generated and viewed"]);
}
