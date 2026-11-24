//! Loopback HTTP surface of the daily-report plugin. The page talks only to
//! these routes; the company server and the bearer token stay on this side.
use super::{
    activity,
    client::{self, Api, LoginRequest, DEFAULT_BASE_URL, DEFAULT_CLIENT_ID},
    store::{Draft, Store},
};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as RoutePath, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone)]
struct Settings {
    directory: PathBuf,
}

/// Production wiring: the same directory the AI tools use.
pub(crate) fn router() -> Router {
    let directory = Store::data_dir().unwrap_or_else(|_| {
        crate::server::ai_proxy::config_dir().join("daily-report")
    });
    with_directory(directory)
}

pub(super) fn with_directory(directory: PathBuf) -> Router {
    Router::new()
        .route("/api/daily-report/status", get(status))
        .route("/api/daily-report/login", post(login))
        .route("/api/daily-report/logout", post(logout))
        .route("/api/daily-report/projects", get(projects))
        .route("/api/daily-report/reports", get(reports).post(create_report))
        .route("/api/daily-report/reports/{id}", get(report).delete(remove_report))
        .route("/api/daily-report/reports/update", put(update_report))
        .route("/api/daily-report/draft", get(draft).put(save_draft).delete(drop_draft))
        .route("/api/daily-report/guide", get(guide).put(save_guide))
        .route("/api/daily-report/today-activity", get(today_activity))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn(local_browser))
        .with_state(Settings { directory })
}

/// Loopback browser only: this router lives on the loopback listener and
/// nowhere else, so a page from another origin cannot drive the company
/// session. The dev server proxies to this listener, so the origin and host
/// ports differ; both sides must still name a loopback host.
async fn local_browser(request: Request, next: Next) -> Response {
    let headers = request.headers();
    let loopback = |value: &str| {
        value
            .parse::<axum::http::uri::Authority>()
            .is_ok_and(|authority| matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]"))
    };
    let local_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(loopback);
    let origin_ok = headers.get(header::ORIGIN).is_none_or(|value| {
        value
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("http://"))
            .is_some_and(loopback)
    });
    if !local_host || !origin_ok {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "the daily report requires a local browser" })),
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

#[derive(Debug)]
struct ApiError(StatusCode, Value);
type ApiResult<T> = std::result::Result<T, ApiError>;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

fn failed(error: anyhow::Error) -> ApiError {
    let text = format!("{error:#}");
    if text.contains(client::AUTH_EXPIRED) {
        let message = text.split_once(AUTH_AFTER).map(|(_, rest)| rest.trim().to_owned()).unwrap_or(text);
        return ApiError(StatusCode::UNAUTHORIZED, json!({ "error": message, "auth_expired": true }));
    }
    ApiError(StatusCode::BAD_REQUEST, json!({ "error": text }))
}

const AUTH_AFTER: &str = "AUTH_EXPIRED: ";

fn store(settings: &Settings) -> ApiResult<Store> {
    Store::open(settings.directory.clone()).map_err(|error| {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("{error:#}") }))
    })
}

fn api(settings: &Settings) -> ApiResult<Api> {
    let session = store(settings)?
        .session()
        .map_err(failed)?
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, json!({ "error": "未登录日报系统", "auth_expired": true })))?;
    Api::new(session).map_err(failed)
}

async fn status(State(settings): State<Settings>) -> ApiResult<Json<Value>> {
    Ok(Json(store(&settings)?.status().map_err(failed)?))
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    tenant_id: String,
    #[serde(default)]
    rsa_public_key: String,
}

async fn login(State(settings): State<Settings>, Json(body): Json<LoginBody>) -> ApiResult<Json<Value>> {
    let store = store(&settings)?;
    let client_id = if body.client_id.trim().is_empty() {
        DEFAULT_CLIENT_ID.to_owned()
    } else {
        body.client_id.trim().to_owned()
    };
    let tenant_id = if body.tenant_id.trim().is_empty() {
        "000000".to_owned()
    } else {
        body.tenant_id.trim().to_owned()
    };
    // An empty address means the team server, which is what the form shows.
    let base_url = if body.base_url.trim().is_empty() {
        DEFAULT_BASE_URL.to_owned()
    } else {
        body.base_url.trim().to_owned()
    };
    let session = if body.token.trim().is_empty() {
        if body.username.trim().is_empty() || body.password.is_empty() {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                json!({ "error": "请填写用户名和密码" }),
            ));
        }
        client::login(&LoginRequest {
            base_url,
            client_id,
            tenant_id,
            username: body.username.trim().to_owned(),
            password: body.password.clone(),
            rsa_public_key: Some(body.rsa_public_key.clone()).filter(|key| !key.trim().is_empty()),
        })
        .await
        .map_err(failed)?
    } else {
        client::login_with_token(&base_url, &body.token, &client_id, &tenant_id)
            .await
            .map_err(failed)?
    };
    store.save_session(&session).map_err(failed)?;
    Ok(Json(store.status().map_err(failed)?))
}

async fn logout(State(settings): State<Settings>) -> ApiResult<Json<Value>> {
    let store = store(&settings)?;
    store.logout().map_err(failed)?;
    Ok(Json(json!({ "logged_out": true })))
}

async fn projects(State(settings): State<Settings>) -> ApiResult<Json<Value>> {
    let projects = api(&settings)?.projects().await.map_err(failed)?;
    Ok(Json(json!({ "projects": projects })))
}

async fn reports(
    State(settings): State<Settings>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let mut filters: Vec<(&str, String)> = Vec::new();
    for name in ["reportYear", "reportMonth", "reportDate", "userId", "pageSize", "pageNum"] {
        if let Some(value) = query.get(name).filter(|value| !value.trim().is_empty()) {
            filters.push((name, value.clone()));
        }
    }
    Ok(Json(api(&settings)?.reports(&filters).await.map_err(failed)?))
}

async fn report(State(settings): State<Settings>, RoutePath(id): RoutePath<String>) -> ApiResult<Json<Value>> {
    Ok(Json(api(&settings)?.report(&id).await.map_err(failed)?))
}

/// Both writes share one body: the draft as the page or the AI filled it in.
async fn create_report(State(settings): State<Settings>, Json(body): Json<Draft>) -> ApiResult<Json<Value>> {
    submit(&settings, body, false).await
}

async fn update_report(State(settings): State<Settings>, Json(body): Json<Draft>) -> ApiResult<Json<Value>> {
    submit(&settings, body, true).await
}

async fn submit(settings: &Settings, mut draft: Draft, update: bool) -> ApiResult<Json<Value>> {
    let store = store(settings)?;
    if update {
        draft.action = "update".to_owned();
    }
    if draft.projects.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, json!({ "error": "至少填写一个项目的工作内容" })));
    }
    let answer = api(settings)?.submit(&draft).await.map_err(failed)?;
    // The submitted report is the record now; a stale draft would resubmit it.
    store.clear_draft().map_err(failed)?;
    Ok(Json(json!({ "action": draft.action, "answer": answer })))
}

async fn remove_report(State(settings): State<Settings>, RoutePath(id): RoutePath<String>) -> ApiResult<Json<Value>> {
    Ok(Json(api(&settings)?.delete(&id).await.map_err(failed)?))
}

async fn draft(State(settings): State<Settings>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "draft": store(&settings)?.draft().map_err(failed)? })))
}

async fn save_draft(State(settings): State<Settings>, Json(mut body): Json<Draft>) -> ApiResult<Json<Value>> {
    let store = store(&settings)?;
    if body.origin.is_empty() {
        body.origin = "manual".to_owned();
    }
    body.updated_at = client::now();
    store.save_draft(&body).map_err(failed)?;
    Ok(Json(json!({ "draft": body })))
}

async fn drop_draft(State(settings): State<Settings>) -> ApiResult<Json<Value>> {
    store(&settings)?.clear_draft().map_err(failed)?;
    Ok(Json(json!({ "cleared": true })))
}

async fn guide(State(settings): State<Settings>) -> ApiResult<Json<Value>> {
    let store = store(&settings)?;
    let body = store.guide().map_err(failed)?;
    Ok(Json(json!({
        "content": body,
        "path": store.guide_path().display().to_string(),
        "guide_dir": store.root().display().to_string(),
    })))
}

#[derive(Deserialize)]
struct GuideBody {
    #[serde(default)]
    content: String,
}

async fn save_guide(State(settings): State<Settings>, Json(body): Json<GuideBody>) -> ApiResult<Json<Value>> {
    let store = store(&settings)?;
    store.write_guide(&body.content).map_err(failed)?;
    Ok(Json(json!({ "saved": true, "bytes": body.content.len() })))
}

async fn today_activity(
    State(_settings): State<Settings>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let workspace = query.get("workspace_root").map(String::as_str).unwrap_or_default();
    if workspace.trim().is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, json!({ "error": "需要 workspace_root 才能汇总今日工作" })));
    }
    let date = query.get("date").map(String::as_str);
    let summary: Result<Value> = activity::today(std::path::Path::new(workspace), date);
    Ok(Json(summary.map_err(failed)?))
}
