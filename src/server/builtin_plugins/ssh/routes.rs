use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use futures::TryStreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::{ReaderStream, StreamReader};

use super::{
    client,
    credentials::Secret,
    live::{self, LiveMessage},
    pty,
    sftp::Sftp,
    store::{
        Action, AuthType, HostKeyFingerprint, HostKeyMismatch, NewServer, ProfileUpdate, Server,
        Store,
    },
};

const MAX_UPLOAD: u64 = 1 << 30;
const TICKET_AGE: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct Workbench {
    directory: PathBuf,
    tickets: Arc<Mutex<HashMap<String, Ticket>>>,
    connections: Arc<Semaphore>,
}

struct Ticket {
    id: String,
    issued: Instant,
    purpose: TicketPurpose,
}

enum TicketPurpose {
    Terminal(Access),
    AiStream(String),
}

pub(crate) fn router() -> Router {
    with_directory(crate::server::ai_proxy::config_dir())
}

pub(super) fn with_directory(directory: PathBuf) -> Router {
    Router::new()
        .route("/agent/ssh/servers", get(list).post(create))
        .route(
            "/agent/ssh/servers/{id}",
            get(profile).patch(update).delete(remove),
        )
        .route("/agent/ssh/servers/{id}/credential", put(credential))
        .route("/agent/ssh/servers/{id}/test", post(test_connection))
        .route("/agent/ssh/servers/{id}/host-key", put(confirm_key))
        .route("/agent/ssh/servers/{id}/history", get(history))
        .route(
            "/agent/ssh/servers/{id}/terminal-ticket",
            post(terminal_ticket),
        )
        .route("/agent/ssh/servers/{id}/terminal", get(terminal))
        .route(
            "/agent/ssh/servers/{id}/ai-stream-ticket",
            post(ai_stream_ticket),
        )
        .route("/agent/ssh/servers/{id}/ai-stream", get(ai_stream))
        .route("/agent/ssh/servers/{id}/sftp/list", get(sftp_list))
        .route("/agent/ssh/servers/{id}/sftp/preview", get(sftp_preview))
        .route("/agent/ssh/servers/{id}/sftp/download", get(sftp_download))
        .route(
            "/agent/ssh/servers/{id}/sftp/upload",
            put(sftp_upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/agent/ssh/servers/{id}/sftp/mutate", post(sftp_mutate))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(browser_only))
        .with_state(Workbench {
            directory,
            tickets: Arc::default(),
            connections: Arc::new(Semaphore::new(12)),
        })
}

async fn browser_only(request: Request, next: Next) -> Response {
    let headers = request.headers();
    let same_site = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        == Some("same-origin");
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let scheme = request.uri().scheme_str().unwrap_or("http"); // The local listener serves plain HTTP.
    let origin_matches = headers.get(header::ORIGIN).is_none_or(|origin| {
        origin
            .to_str()
            .ok()
            .zip(host)
            .is_some_and(|(origin, host)| origin.split_once("://") == Some((scheme, host)))
    });
    // Chromium omits Fetch Metadata on a WebSocket handshake. Require its
    // exact Origin as well as the single-use ticket checked in the handler.
    let path = request.uri().path();
    let upgrade = (path.ends_with("/terminal") || path.ends_with("/ai-stream"))
        && headers
            .get(header::UPGRADE)
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if !origin_matches
        || host.is_none()
        || !(if upgrade {
            headers.contains_key(header::ORIGIN)
        } else {
            same_site
        })
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"SSH workbench requires a same-origin local browser"})),
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

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        if let Some(mismatch) = error.downcast_ref::<HostKeyMismatch>() {
            return Self(
                StatusCode::CONFLICT,
                json!({"code":"HOST_KEY_MISMATCH","error":mismatch.to_string(),"trusted":mismatch.trusted,"received":mismatch.received}),
            );
        }
        Self::new(StatusCode::BAD_REQUEST, error.to_string())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}
impl ApiError {
    fn new(status: StatusCode, error: impl Into<String>) -> Self {
        Self(status, json!({"error":error.into()}))
    }
}

impl Workbench {
    async fn db<T: Send + 'static>(
        &self,
        query: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> ApiResult<T> {
        let dir = self.directory.clone();
        Ok(
            tokio::task::spawn_blocking(move || query(&mut Store::open(&dir)?))
                .await
                .map_err(anyhow::Error::from)??,
        )
    }

    async fn find(&self, id: String) -> ApiResult<Server> {
        self.db(move |store| store.get(&id))
            .await?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "SSH server not found"))
    }

    async fn connection(
        &self,
        id: &str,
        approved: Option<bool>,
    ) -> ApiResult<(OwnedSemaphorePermit, client::Connection)> {
        let profile = self.find(id.to_owned()).await?;
        if profile.require_approval && approved == Some(false) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "Explicit confirmation is required for this server",
            ));
        }
        let permit = self.connections.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many open SSH connections",
            )
        })?;
        Ok((
            permit,
            client::connect(self.directory.clone(), id.to_owned()).await?,
        ))
    }

    async fn sftp(
        &self,
        id: &str,
        approved: Option<bool>,
    ) -> ApiResult<(OwnedSemaphorePermit, Sftp)> {
        let (permit, connection) = self.connection(id, approved).await?;
        Ok((permit, Sftp::open(connection).await?))
    }

    async fn record(
        &self,
        id: String,
        action: Action,
        access: Access,
        path: Option<String>,
    ) -> ApiResult<()> {
        self.db(move |store| {
            store.record_usage(
                &id,
                action,
                access.task_id.as_deref(),
                access.workspace_root.as_deref(),
                path.as_deref(),
            )
        })
        .await
    }
}

async fn list(State(state): State<Workbench>) -> ApiResult<Json<Vec<Server>>> {
    Ok(Json(state.db(|store| store.list()).await?))
}

#[derive(Deserialize)]
struct Create {
    #[serde(flatten)]
    profile: NewServer,
    credential: Option<String>,
}

async fn create(
    State(state): State<Workbench>,
    Json(input): Json<Create>,
) -> ApiResult<(StatusCode, Json<Server>)> {
    let server = state
        .db(move |store| {
            let secret = input.credential.map(Secret::new).transpose()?;
            ensure!(
                input.profile.auth_type != AuthType::Agent || secret.is_none(),
                "SSH agent profiles do not take a saved credential"
            );
            let id = store.create(input.profile)?;
            if let Some(secret) = secret
                && let Err(error) = store.set_credential(&id, Some(secret))
            {
                let _ = store.delete(&id);
                return Err(error);
            }
            store.get(&id)?.context("SSH server disappeared")
        })
        .await?;
    Ok((StatusCode::CREATED, Json(server)))
}

async fn profile(
    State(state): State<Workbench>,
    Path(id): Path<String>,
) -> ApiResult<Json<Server>> {
    Ok(Json(state.find(id).await?))
}

async fn update(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Json(input): Json<ProfileUpdate>,
) -> ApiResult<Json<Server>> {
    state
        .db({
            let id = id.clone();
            move |store| store.update_details(&id, input)
        })
        .await?;
    profile(State(state), Path(id)).await
}

async fn remove(State(state): State<Workbench>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    if state.db(move |store| store.delete(&id)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "SSH server not found"))
    }
}

#[derive(Deserialize)]
struct Credential {
    value: Option<String>,
}
async fn credential(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Json(input): Json<Credential>,
) -> ApiResult<StatusCode> {
    state
        .db(move |store| store.set_credential(&id, input.value.map(Secret::new).transpose()?))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn test_connection(
    State(state): State<Workbench>,
    Path(id): Path<String>,
) -> ApiResult<Json<Server>> {
    let (_permit, conn) = state.connection(&id, None).await?;
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        conn.disconnect(
            russh::Disconnect::ByApplication,
            "Connection test complete",
            "",
        ),
    )
    .await;
    profile(State(state), Path(id)).await
}

#[derive(Deserialize)]
struct ConfirmKey {
    trusted: String,
    received: String,
}
async fn confirm_key(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Json(input): Json<ConfirmKey>,
) -> ApiResult<StatusCode> {
    state
        .db(move |store| {
            store.confirm_host_key(
                &id,
                HostKeyFingerprint::parse(&input.trusted)?,
                HostKeyFingerprint::parse(&input.received)?,
            )
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn history(State(state): State<Workbench>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    state.find(id.clone()).await?;
    Ok(Json(
        json!({"history":state.db(move |store| store.history(&id, 50)).await?}),
    ))
}

#[derive(Default, Deserialize)]
struct Access {
    #[serde(default)]
    approved: bool,
    workspace_root: Option<String>,
    task_id: Option<String>,
}

async fn terminal_ticket(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Json(access): Json<Access>,
) -> ApiResult<Json<Value>> {
    let server = state.find(id.clone()).await?;
    if server.require_approval && !access.approved {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Explicit confirmation is required for this server",
        ));
    }
    issue_ticket(&state, id, TicketPurpose::Terminal(access))
}

#[derive(Deserialize)]
struct AiStreamRequest {
    task_id: String,
}

async fn ai_stream_ticket(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Json(request): Json<AiStreamRequest>,
) -> ApiResult<Json<Value>> {
    let task = request.task_id.trim();
    if task.is_empty() || task.len() > 256 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid task ID"));
    }
    state.find(id.clone()).await?;
    issue_ticket(&state, id, TicketPurpose::AiStream(task.to_owned()))
}

fn issue_ticket(state: &Workbench, id: String, purpose: TicketPurpose) -> ApiResult<Json<Value>> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot issue SSH workbench ticket",
        )
    })?;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let key = URL_SAFE_NO_PAD.encode(bytes);
    let mut tickets = state.tickets.lock().unwrap();
    tickets.retain(|_, ticket| ticket.issued.elapsed() < TICKET_AGE);
    if tickets.len() >= 32 {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many pending SSH workbench tickets",
        ));
    }
    tickets.insert(
        key.clone(),
        Ticket {
            id,
            issued: Instant::now(),
            purpose,
        },
    );
    Ok(Json(json!({"ticket":key})))
}

#[derive(Deserialize)]
struct TicketQuery {
    ticket: Option<String>,
}

fn consume_ticket(
    state: &Workbench,
    id: &str,
    query: TicketQuery,
    ai_stream: bool,
) -> ApiResult<TicketPurpose> {
    let key = query
        .ticket
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "SSH workbench ticket required"))?;
    let mut tickets = state.tickets.lock().unwrap();
    let valid = tickets.get(&key).is_some_and(|ticket| {
        ticket.id == id
            && ticket.issued.elapsed() < TICKET_AGE
            && matches!(ticket.purpose, TicketPurpose::AiStream(_)) == ai_stream
    });
    if !valid {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "SSH workbench ticket is expired or invalid",
        ));
    }
    Ok(tickets.remove(&key).unwrap().purpose)
}

async fn terminal(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Query(query): Query<TicketQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let TicketPurpose::Terminal(access) = consume_ticket(&state, &id, query, false)? else {
        unreachable!()
    };
    Ok(ws
        .max_frame_size(64 * 1024)
        .max_message_size(64 * 1024)
        .on_upgrade(move |mut socket| async move {
            let attempt = tokio::select! {
                result = open_terminal(&state, id, access) => result,
                _ = socket.recv() => return,
            };
            match attempt {
                Ok((_permit, connection, channel)) => {
                    let _ = pty::relay(socket, channel, connection).await;
                }
                Err(error) => {
                    let _ =
                        pty::message(&mut socket, json!({"type":"error","detail":error.1})).await;
                }
            }
        }))
}

async fn ai_stream(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Query(query): Query<TicketQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let TicketPurpose::AiStream(task) = consume_ticket(&state, &id, query, true)? else {
        unreachable!()
    };
    Ok(ws
        .max_frame_size(64 * 1024)
        .max_message_size(64 * 1024)
        .on_upgrade(move |socket| relay_ai_stream(socket, task, id)))
}

async fn relay_ai_stream(mut socket: WebSocket, task: String, id: String) {
    let (replay, mut live) = live::subscribe(&task, &id);
    if pty::message(&mut socket, json!({"type":"ready"}))
        .await
        .is_err()
    {
        return;
    }
    if !replay.is_empty() && socket.send(Message::Binary(replay.into())).await.is_err() {
        return;
    }
    let period = Duration::from_secs(20);
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    loop {
        let message = tokio::select! {
            received = socket.recv() => match received {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                Some(Ok(_)) => continue, // Incoming frames are never forwarded or interpreted.
            },
            update = live.recv() => match update {
                Ok(LiveMessage::Bytes(bytes)) => Message::Binary(axum::body::Bytes::from_owner(bytes)),
                Ok(LiveMessage::Activity { action, remote_path, detail }) =>
                    Message::Text(json!({"type":"activity", "action":action, "remote_path":remote_path, "detail":detail}).to_string().into()),
                // A stalled or evicted subscriber must reopen with a fresh replay, not miss bytes silently.
                Err(_) => break,
            },
            _ = ping.tick() => Message::Ping(Default::default()),
        };
        if socket.send(message).await.is_err() {
            break;
        }
    }
}

async fn open_terminal(
    state: &Workbench,
    id: String,
    access: Access,
) -> ApiResult<(
    OwnedSemaphorePermit,
    client::Connection,
    russh::Channel<russh::client::Msg>,
)> {
    let (permit, connection) = state.connection(&id, Some(access.approved)).await?;
    let channel = pty::open(&connection).await?;
    state.record(id, Action::Terminal, access, None).await?;
    Ok((permit, connection, channel))
}

#[derive(Deserialize)]
struct RemotePath {
    path: String,
    #[serde(default)]
    approved: bool,
    workspace_root: Option<String>,
    task_id: Option<String>,
}
impl RemotePath {
    fn access(&self) -> Access {
        Access {
            approved: self.approved,
            workspace_root: self.workspace_root.clone(),
            task_id: self.task_id.clone(),
        }
    }
}
fn valid_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && path.len() <= 4096 && !path.contains('\0'),
        "Invalid remote path"
    );
    Ok(())
}

async fn sftp_list(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Query(q): Query<RemotePath>,
) -> ApiResult<Json<Value>> {
    valid_path(&q.path)?;
    let (_permit, sftp) = state.sftp(&id, None).await?;
    let (path, entries) = sftp.list(&q.path).await?;
    Ok(Json(json!({"path":path,"entries":entries})))
}

async fn sftp_preview(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Query(q): Query<RemotePath>,
) -> ApiResult<Json<Value>> {
    valid_path(&q.path)?;
    let (_permit, sftp) = state.sftp(&id, None).await?;
    let (text, truncated) = sftp.preview(&q.path).await?;
    state
        .record(id, Action::ReadFile, q.access(), Some(q.path))
        .await?;
    Ok(Json(json!({"text":text,"truncated":truncated})))
}

async fn sftp_download(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Query(q): Query<RemotePath>,
) -> ApiResult<Response> {
    valid_path(&q.path)?;
    let (permit, sftp) = state.sftp(&id, None).await?;
    let file = sftp
        .session
        .open(&q.path)
        .await
        .map_err(anyhow::Error::from)?;
    let stream = ReaderStream::with_capacity(file, 32 * 1024).map_ok(move |chunk| {
        let _keep_alive = (&sftp, &permit);
        chunk
    });
    state
        .record(id, Action::Download, q.access(), Some(q.path.clone()))
        .await?;
    let name = q
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("download");
    let name: String = name.bytes().map(|byte| format!("%{byte:02X}")).collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename*=UTF-8''{name}"),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

async fn sftp_upload(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Query(q): Query<RemotePath>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<Value>> {
    valid_path(&q.path)?;
    let size = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::LENGTH_REQUIRED,
                "Upload requires Content-Length",
            )
        })?;
    if size > MAX_UPLOAD {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Upload exceeds 1 GiB",
        ));
    }
    let (permit, sftp) = state.sftp(&id, Some(q.approved)).await?;
    // Own the reader in a task so a disconnected HTTP caller does not interrupt cleanup.
    let bytes = tokio::spawn(async move {
        let _permit = permit;
        let mut input = StreamReader::new(body.into_data_stream().map_err(std::io::Error::other));
        let input = tokio::io::AsyncReadExt::take(&mut input, size + 1);
        let bytes = sftp.upload(&q.path, input, size).await?;
        state
            .record(id, Action::Upload, q.access(), Some(q.path))
            .await?;
        Ok::<_, ApiError>(bytes)
    })
    .await
    .map_err(anyhow::Error::from)??;
    Ok(Json(json!({"bytes":bytes})))
}

#[derive(Deserialize)]
struct Mutation {
    path: String,
    #[serde(flatten)]
    operation: Operation,
    #[serde(flatten)]
    access: Access,
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Operation {
    Mkdir,
    Rename { destination: String },
    RemoveFile,
    RemoveDir,
}

async fn sftp_mutate(
    State(state): State<Workbench>,
    Path(id): Path<String>,
    Json(input): Json<Mutation>,
) -> ApiResult<StatusCode> {
    valid_path(&input.path)?;
    if let Operation::Rename { destination } = &input.operation {
        valid_path(destination)?;
    }
    let (_permit, sftp) = state.sftp(&id, Some(input.access.approved)).await?;
    match input.operation {
        Operation::Mkdir => sftp.session.create_dir(&input.path).await,
        Operation::Rename { destination } => sftp.session.rename(&input.path, destination).await,
        Operation::RemoveFile => sftp.session.remove_file(&input.path).await,
        Operation::RemoveDir => sftp.session.remove_dir(&input.path).await,
    }
    .map_err(anyhow::Error::from)?;
    state
        .record(id, Action::WriteFile, input.access, Some(input.path))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
