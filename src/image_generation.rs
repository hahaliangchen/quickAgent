use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageFormat;
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};
use std::io::Cursor;
use tokio_util::sync::CancellationToken;

const DEFAULT_MODEL: &str = "gpt-image-2";
const MAX_REFERENCES: usize = 16;

fn route(
    state: &crate::agent_service::AgentServiceState,
    args: &Value,
) -> Result<(String, String, String, String)> {
    let configured_provider = state.image_generation_settings.provider.as_deref();
    let provider = configured_provider
        .or_else(|| {
            state
                .provider_routes
                .get(&state.provider_name)
                .filter(|route| !route.messages)
                .map(|_| state.provider_name.as_str())
        })
        .or_else(|| {
            state
                .provider_routes
                .iter()
                .find(|(_, route)| !route.messages)
                .map(|(name, _)| name.as_str())
        });
    let model = args["model"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .or(state.image_generation_settings.model.as_deref())
        .unwrap_or(DEFAULT_MODEL)
        .to_owned();
    if let Some(provider) = provider {
        let selected = state
            .provider_routes
            .get(provider)
            .context("image generation provider is unavailable")?;
        ensure!(
            !selected.messages,
            "image generation requires an OpenAI-compatible provider route"
        );
        let upstream = selected
            .models
            .get(&model)
            .cloned()
            .unwrap_or_else(|| model.clone());
        return Ok((
            provider.to_owned(),
            selected.url.clone(),
            selected.api_key.clone(),
            upstream,
        ));
    }
    ensure!(
        !state.provider_url.trim().is_empty(),
        "image generation provider is not configured"
    );
    ensure!(
        state.provider_name.is_empty()
            || !state
                .provider_routes
                .get(&state.provider_name)
                .is_some_and(|route| route.messages),
        "image generation requires an OpenAI-compatible provider route"
    );
    let upstream = state
        .model_map
        .get(&model)
        .cloned()
        .unwrap_or_else(|| model.clone());
    Ok((
        state.provider_name.clone(),
        state.provider_url.clone(),
        state.api_key.clone(),
        upstream,
    ))
}

fn png_bytes(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let image =
        image::load_from_memory(bytes).context("image response is not a supported image")?;
    let (width, height) = (image.width(), image.height());
    let mut png = Vec::new();
    image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    Ok((png, width, height))
}

fn response_error(status: reqwest::StatusCode, body: &str, key: &str) -> anyhow::Error {
    let mut summary = body.chars().take(400).collect::<String>();
    if !key.is_empty() {
        summary = summary.replace(key, "[redacted]");
    }
    anyhow::anyhow!("image generation upstream returned {status}: {summary}")
}

async fn send_bytes(
    request: reqwest::RequestBuilder,
    cancel: &CancellationToken,
    key: &str,
) -> Result<Vec<u8>> {
    let response = tokio::select! {
        result = request.send() => result.context("image generation request failed")?,
        _ = cancel.cancelled() => bail!("image generation cancelled"),
    };
    let status = response.status();
    let body = tokio::select! {
        result = response.bytes() => result.context("image generation response could not be read")?,
        _ = cancel.cancelled() => bail!("image generation cancelled"),
    };
    ensure!(
        status.is_success(),
        response_error(status, &String::from_utf8_lossy(&body), key)
    );
    Ok(body.to_vec())
}

fn references(
    root: &std::path::Path,
    context: &crate::visual_artifacts::VisualContext,
    args: &Value,
) -> Result<Vec<Vec<u8>>> {
    let paths = args["reference_paths"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ids = args["reference_artifact_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ensure!(
        paths.iter().all(Value::is_string) && ids.iter().all(Value::is_string),
        "reference paths and artifact IDs must be strings"
    );
    ensure!(
        paths.len() + ids.len() <= MAX_REFERENCES,
        "at most {MAX_REFERENCES} reference images are allowed"
    );
    let mut output = Vec::with_capacity(paths.len() + ids.len());
    for raw in paths {
        let raw = raw.as_str().unwrap();
        let path = crate::file_edit::workspace_path(root, raw)
            .map_err(|error| anyhow::anyhow!("invalid reference path {raw:?}: {error}"))?;
        output.push(
            std::fs::read(path).with_context(|| format!("cannot read reference image {raw:?}"))?,
        );
    }
    for id in ids {
        let id = id.as_str().unwrap();
        let (metadata, bytes) = crate::visual_artifacts::read(root, context.task_id(), id)
            .with_context(|| format!("reference artifact {id:?} is unavailable in this task"))?;
        crate::visual_artifacts::authorize(&metadata, context)?;
        output.push(bytes);
    }
    Ok(output)
}

pub(crate) async fn execute(
    state: &crate::agent_service::AgentServiceState,
    context: &crate::visual_artifacts::VisualContext,
    workspace: &crate::tools::Workspace,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    ensure!(!cancel.is_cancelled(), "image generation cancelled");
    let prompt = args["prompt"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("prompt must not be empty")?;
    let size = args["size"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("auto");
    let quality = args["quality"]
        .as_str()
        .filter(|value| matches!(*value, "low" | "medium" | "high" | "auto"))
        .unwrap_or("auto");
    let background = args["background"]
        .as_str()
        .filter(|value| matches!(*value, "opaque" | "transparent" | "auto"))
        .unwrap_or("auto");
    let output_path = args["output_path"]
        .as_str()
        .filter(|value| !value.trim().is_empty());
    let output_target = if let Some(raw) = output_path {
        ensure!(
            state.permission_mode != crate::agent_service::PermissionMode::ReadOnly,
            "PERMISSION_DENIED: output_path is not allowed in read-only mode"
        );
        Some(
            crate::file_edit::workspace_path(workspace.root(), raw)
                .map_err(|error| anyhow::anyhow!("invalid output_path {raw:?}: {error}"))?,
        )
    } else {
        None
    };
    let reference_bytes = references(workspace.root(), context, args)?;
    let (provider, base_url, api_key, model) = route(state, args)?;
    let base_url = base_url.trim_end_matches('/');
    let mode = if reference_bytes.is_empty() {
        "generate"
    } else {
        "edit"
    };
    let body = json!({"model":model,"prompt":prompt,"size":size,"quality":quality,"background":background,"output_format":"png"});
    let raw = if reference_bytes.is_empty() {
        send_bytes(
            state
                .model_client
                .post(format!("{base_url}/images/generations"))
                .bearer_auth(&api_key)
                .json(&body),
            cancel,
            &api_key,
        )
        .await?
    } else {
        let mut form = Form::new()
            .text("model", model.clone())
            .text("prompt", prompt.to_owned())
            .text("size", size.to_owned())
            .text("quality", quality.to_owned())
            .text("background", background.to_owned())
            .text("output_format", "png");
        for (index, bytes) in reference_bytes.into_iter().enumerate() {
            let part = Part::bytes(bytes).file_name(format!("reference-{index}.png"));
            form = form.part("image[]", part);
        }
        send_bytes(
            state
                .model_client
                .post(format!("{base_url}/images/edits"))
                .bearer_auth(&api_key)
                .multipart(form),
            cancel,
            &api_key,
        )
        .await?
    };
    let response: Value =
        serde_json::from_slice(&raw).context("image generation returned invalid JSON")?;
    let item = response["data"]
        .as_array()
        .and_then(|items| items.first())
        .context("image generation returned no image")?;
    let revised_prompt = item["revised_prompt"].as_str().map(str::to_owned);
    let encoded = if let Some(value) = item["b64_json"].as_str() {
        STANDARD
            .decode(value)
            .context("image generation returned invalid base64")?
    } else if let Some(url) = item["url"].as_str() {
        // Provider image URLs may be signed or hosted elsewhere; never forward the provider key.
        send_bytes(state.model_client.get(url), cancel, "").await?
    } else {
        bail!("image generation response contained neither b64_json nor url")
    };
    let (png, width, height) = png_bytes(&encoded)?;
    let metadata = crate::visual_artifacts::save(
        workspace.root(),
        context,
        json!({"source":"vision.generate_image","provider":provider,"model":model,"mode":mode,"prompt":prompt}),
        &png,
    )?;
    if let Some(path) = &output_target {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &png)?;
    }
    let mut result = json!({"artifact_id":metadata["artifact_id"],"visual_artifact":metadata,"model":model,"mode":mode,"size":size,"width":width,"height":height,"bytes":png.len()});
    if let Some(raw) = output_path {
        result["output_path"] = json!(raw);
    }
    if let Some(revised) = revised_prompt {
        result["revised_prompt"] = json!(revised);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn png() -> Vec<u8> {
        let image = image::DynamicImage::new_rgb8(8, 6);
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn jpeg() -> Vec<u8> {
        let image = image::DynamicImage::new_rgb8(5, 4);
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        bytes
    }

    fn context(task: &str) -> crate::visual_artifacts::VisualContext {
        crate::visual_artifacts::VisualContext {
            identity: json!({
                "task_id": task,
                "request_id": 1,
                "runtime": "single_agent",
                "agent_id": null,
                "task_node_id": null
            }),
            ..Default::default()
        }
    }

    async fn mock_server(
        response: Value,
        seen: Arc<Mutex<Vec<(String, HeaderMap, Bytes)>>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let generations_seen = seen.clone();
        let generations_response = response.clone();
        let edits_seen = seen.clone();
        let edits_response = response;
        let app = Router::new()
            .route(
                "/v1/images/generations",
                post(move |headers: HeaderMap, body: Bytes| {
                    let seen = generations_seen.clone();
                    let response = generations_response.clone();
                    async move {
                        seen.lock()
                            .unwrap()
                            .push(("generations".into(), headers, body));
                        axum::Json(response)
                    }
                }),
            )
            .route(
                "/v1/images/edits",
                post(move |headers: HeaderMap, body: Bytes| {
                    let seen = edits_seen.clone();
                    let response = edits_response.clone();
                    async move {
                        seen.lock().unwrap().push(("edits".into(), headers, body));
                        axum::Json(response)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/v1"), task)
    }

    fn test_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("{name}-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[tokio::test]
    async fn generates_png_writes_output_and_sends_only_safe_request_data() {
        let root = test_root("image-generation");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (url, server) = mock_server(
            json!({
                "data": [{
                    "b64_json": STANDARD.encode(png()),
                    "revised_prompt": "revised"
                }]
            }),
            seen.clone(),
        )
        .await;
        let mut state =
            crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.provider_url = url;
        state.api_key = "top-secret".into();
        let result = execute(
            &state,
            &context("task"),
            &state.workspace,
            &json!({
                "prompt": "a house",
                "model": "gpt-image-1.5",
                "size": "1536x1024",
                "quality": "high",
                "background": "transparent",
                "output_path": "out/generated.png"
            }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(result["mode"], "generate");
        assert_eq!(result["model"], "gpt-image-1.5");
        assert_eq!(result["width"], 8);
        assert_eq!(result["height"], 6);
        assert_eq!(result["revised_prompt"], "revised");
        assert!(result["artifact_id"].as_str().is_some());
        assert!(!result.to_string().contains("top-secret"));
        assert!(root.join("out/generated.png").exists());
        let artifact_path = root.join(
            result["visual_artifact"]["workspace_relative_path"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(
            std::fs::read(artifact_path).unwrap()[..8],
            [137, 80, 78, 71, 13, 10, 26, 10]
        );

        let record = seen.lock().unwrap();
        assert_eq!(record.len(), 1);
        assert_eq!(record[0].0, "generations");
        assert_eq!(record[0].1["authorization"], "Bearer top-secret");
        let request: Value = serde_json::from_slice(&record[0].2).unwrap();
        assert_eq!(request["prompt"], "a house");
        assert_eq!(request["model"], "gpt-image-1.5");
        assert_eq!(request["size"], "1536x1024");
        assert_eq!(request["quality"], "high");
        assert_eq!(request["background"], "transparent");
        assert_eq!(request["output_format"], "png");
        assert!(!request.to_string().contains("top-secret"));
        drop(record);
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn edits_upload_path_and_task_artifact_references_and_enforce_boundaries() {
        let root = test_root("image-edit");
        let reference = png();
        std::fs::write(root.join("ref.png"), &reference).unwrap();
        let ctx = context("task");
        let reference_metadata = crate::visual_artifacts::save(
            &root,
            &ctx,
            json!({"source": "test-reference"}),
            &reference,
        )
        .unwrap();
        let reference_id = reference_metadata["artifact_id"].as_str().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (url, server) = mock_server(
            json!({"data": [{"b64_json": STANDARD.encode(png())}]}),
            seen.clone(),
        )
        .await;
        let mut state =
            crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.provider_url = url;
        let result = execute(
            &state,
            &ctx,
            &state.workspace,
            &json!({
                "prompt": "edit",
                "reference_paths": ["ref.png"],
                "reference_artifact_ids": [reference_id]
            }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["mode"], "edit");
        let record = seen.lock().unwrap();
        assert_eq!(record[0].0, "edits");
        assert_eq!(record[0].1["authorization"], "Bearer fake");
        let body = &record[0].2;
        assert!(
            body.windows(8)
                .filter(|window| *window == [137, 80, 78, 71, 13, 10, 26, 10])
                .count()
                >= 2
        );
        assert!(String::from_utf8_lossy(body).contains("name=\"model\""));
        assert!(String::from_utf8_lossy(body).contains("name=\"prompt\""));
        drop(record);
        assert!(
            execute(
                &state,
                &ctx,
                &state.workspace,
                &json!({"prompt": "x", "reference_paths": ["../bad.png"]}),
                &CancellationToken::new(),
            )
            .await
            .is_err()
        );
        state.permission_mode = crate::agent_service::PermissionMode::ReadOnly;
        assert!(
            execute(
                &state,
                &ctx,
                &state.workspace,
                &json!({"prompt": "x", "output_path": "out.png"}),
                &CancellationToken::new(),
            )
            .await
            .is_err()
        );
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn jpeg_response_is_normalized_to_png() {
        let root = test_root("image-jpeg");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (url, server) = mock_server(
            json!({"data": [{"b64_json": STANDARD.encode(jpeg())}]}),
            seen,
        )
        .await;
        let mut state =
            crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.provider_url = url;
        let result = execute(
            &state,
            &context("task"),
            &state.workspace,
            &json!({"prompt": "x"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let path = root.join(
            result["visual_artifact"]["workspace_relative_path"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(
            image::guess_format(std::fs::read(path).unwrap().as_slice()).unwrap(),
            ImageFormat::Png
        );
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }
}
