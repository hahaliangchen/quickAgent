use super::{
    impact::{self, ImpactQuery},
    model::*,
    package::graph_view,
    store::Store,
    verify,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) fn router() -> Router {
    Router::new()
        .route("/api/blueprint/state", get(state))
        .route("/api/blueprint/graph", post(create_graph))
        .route(
            "/api/blueprint/graph/{id}",
            get(read_graph).put(update_graph).delete(delete_graph),
        )
        .route("/api/blueprint/graph/{id}/layout", put(save_layout))
        .route("/api/blueprint/entities", put(upsert_entities))
        .route(
            "/api/blueprint/entities/{id}",
            axum::routing::delete(delete_entity),
        )
        .route("/api/blueprint/impact", get(read_impact))
        .route("/api/blueprint/verify", get(read_verify))
        .route("/api/blueprint/source", get(read_source))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn(local_browser))
}

// Same-origin browser only; this router is mounted outside permissive CORS
// and exclusively on a loopback listener.
async fn local_browser(request: Request, next: Next) -> Response {
    let headers = request.headers();
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let local_host = host
        .and_then(|h| h.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|h| matches!(h.host(), "localhost" | "127.0.0.1" | "[::1]"));
    let origin_ok = headers
        .get(header::ORIGIN)
        .is_none_or(|v| v.to_str().ok().and_then(|v| v.strip_prefix("http://")) == host);
    if !local_host
        || !origin_ok
        || headers
            .get("sec-fetch-site")
            .is_none_or(|v| v != "same-origin")
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Blueprint requires a same-origin local browser"})),
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Deserialize)]
struct Root {
    workspace_root: PathBuf,
}
#[derive(Deserialize)]
struct SourceQuery {
    workspace_root: PathBuf,
    file_path: String,
    line_start: usize,
    line_end: Option<usize>,
    context_lines: Option<usize>,
}
#[derive(Deserialize)]
struct Impact {
    workspace_root: PathBuf,
    #[serde(flatten)]
    args: ImpactQuery,
}
#[derive(Deserialize)]
struct Layout {
    nodes: Vec<GraphNode>,
}

async fn run(root: PathBuf, op: impl FnOnce(&Store) -> Result<Value> + Send + 'static) -> Response {
    match tokio::task::spawn_blocking(move || {
        ensure!(
            root.is_absolute(),
            "workspace_root must be an absolute directory"
        );
        op(&Store::open(&root)?)
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(err)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":err.to_string()})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"blueprint operation failed"})),
        )
            .into_response(),
    }
}
async fn state(Query(Root { workspace_root }): Query<Root>) -> Response {
    run(workspace_root, |store| {
        let manifest = store.manifest()?;
        let (entities, graphs) = contents(store, manifest.as_ref())?;
        let verify = verify::verify(store, manifest.as_ref(), &entities, &graphs);
        let mut references = std::collections::BTreeMap::<&str, Vec<&str>>::new();
        for graph in &graphs {
            for node in &graph.nodes {
                references.entry(&node.entity_id).or_default().push(&graph.meta.id);
            }
        }
        Ok(json!({"exists":manifest.is_some(),"graphs":manifest.as_ref().map(|m| m.graphs.as_slice()).unwrap_or_default(),"manifest":manifest,"entities":entities,"verify":verify,"references":references}))
    }).await
}
fn contents(
    store: &Store,
    manifest: Option<&Manifest>,
) -> Result<(Vec<Entity>, Vec<BlueprintGraph>)> {
    match manifest {
        Some(m) => Ok((store.entities()?.entities, store.all_graphs(m)?)),
        None => Ok((Vec::new(), Vec::new())),
    }
}
async fn read_graph(Query(root): Query<Root>, Path(id): Path<String>) -> Response {
    run(root.workspace_root, move |store| {
        graph_view(store, &id, true)
    })
    .await
}
async fn create_graph(Query(root): Query<Root>, Json(args): Json<GraphUpdate>) -> Response {
    run(root.workspace_root, move |store| {
        let id = args.id.clone().context("id is required")?;
        Ok(serde_json::to_value(store.upsert_graph(&id, args)?)?)
    })
    .await
}
async fn update_graph(
    Query(root): Query<Root>,
    Path(id): Path<String>,
    Json(args): Json<GraphUpdate>,
) -> Response {
    run(root.workspace_root, move |store| {
        Ok(serde_json::to_value(store.upsert_graph(&id, args)?)?)
    })
    .await
}
async fn delete_graph(Query(root): Query<Root>, Path(id): Path<String>) -> Response {
    run(root.workspace_root, move |store| {
        store.delete_graph(&id)?;
        Ok(json!({"ok":true}))
    })
    .await
}
async fn save_layout(
    Query(root): Query<Root>,
    Path(id): Path<String>,
    Json(args): Json<Layout>,
) -> Response {
    run(root.workspace_root, move |store| {
        store.save_layout(&id, args.nodes)?;
        Ok(json!({"ok":true}))
    })
    .await
}
async fn upsert_entities(Query(root): Query<Root>, Json(args): Json<EntityUpdate>) -> Response {
    run(root.workspace_root, move |store| {
        Ok(serde_json::to_value(store.upsert_entities(args)?)?)
    })
    .await
}
async fn delete_entity(Query(root): Query<Root>, Path(id): Path<String>) -> Response {
    run(root.workspace_root, move |store| {
        if store.manifest()?.is_some() {
            store.upsert_entities(EntityUpdate {
                remove_entity_ids: vec![id],
                ..Default::default()
            })?;
        }
        Ok(json!({"ok":true}))
    })
    .await
}
async fn read_impact(Query(q): Query<Impact>) -> Response {
    run(q.workspace_root, move |store| {
        Ok(serde_json::to_value(impact::impact(store, q.args)?)?)
    })
    .await
}
async fn read_verify(Query(root): Query<Root>) -> Response {
    run(root.workspace_root, |store| {
        let manifest = store.manifest()?;
        let (entities, graphs) = contents(store, manifest.as_ref())?;
        Ok(serde_json::to_value(verify::verify(
            store,
            manifest.as_ref(),
            &entities,
            &graphs,
        ))?)
    })
    .await
}
async fn read_source(Query(q): Query<SourceQuery>) -> Response {
    run(q.workspace_root, move |store| {
        Ok(serde_json::to_value(verify::read_source_snippet(
            store,
            &q.file_path,
            q.line_start,
            q.line_end.unwrap_or(q.line_start),
            q.context_lines.unwrap_or(4),
        )?)?)
    })
    .await
}
