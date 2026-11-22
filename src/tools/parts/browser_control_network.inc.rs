const NETWORK_LIMIT: usize = 100;
const BODY_BYTES: usize = 4096;
type NetworkLog = std::sync::Arc<std::sync::Mutex<NetworkCapture>>;
type CdpReply = tokio::sync::oneshot::Sender<Result<Value>>;
type PageCommand = (&'static str, Value, CdpReply);

#[derive(Default)]
struct NetworkCapture {
    entries: std::collections::VecDeque<NetworkEntry>,
}
struct NetworkEntry {
    start: Option<f64>,
    finished: bool,
    current: bool,
    info: Value,
}

fn redact_headers(headers: &Value) -> Value {
    let Some(headers) = headers.as_object() else {
        return json!({});
    };
    Value::Object(
        headers
            .iter()
            .take(100)
            .map(|(key, value)| {
                let name = key.to_ascii_lowercase();
                let secret = [
                    "auth",
                    "cookie",
                    "token",
                    "secret",
                    "credential",
                    "session",
                    "api-key",
                    "apikey",
                    "csrf",
                    "xsrf",
                ]
                .iter()
                .any(|term| name.contains(term));
                let value = value.as_str().unwrap_or("");
                (
                    key.clone(),
                    if secret {
                        json!("[REDACTED]")
                    } else {
                        json!(&value[..value.floor_char_boundary(1024)])
                    },
                )
            })
            .collect(),
    )
}

fn network_url(raw: &Value) -> Value {
    let Some(mut url) = raw.as_str().and_then(|raw| reqwest::Url::parse(raw).ok()) else {
        return Value::Null;
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    json!(&url.as_str()[..url.as_str().floor_char_boundary(2000)])
}

fn merge_headers(target: &mut Value, from: &Value) {
    let dest = target.as_object_mut().expect("initialized header map");
    for (key, value) in redact_headers(from).as_object().unwrap() {
        if dest.len() < 100 || dest.contains_key(key) {
            dest.insert(key.clone(), value.clone());
        }
    }
}

fn set_response(entry: &mut Value, response: &Value) {
    entry["status"] = response["status"].clone();
    entry["mime_type"] = response["mimeType"].clone();
    merge_headers(&mut entry["response_headers"], &response["headers"]);
}

impl NetworkCapture {
    fn observe(&mut self, target: &str, event: &Value) {
        let p = &event["params"];
        let Some(id) = p["requestId"].as_str() else {
            return;
        };
        let previous = self.entries.iter_mut().rev().find(|entry| {
            entry.current && entry.info["request_id"] == id && entry.info["target_id"] == target
        });
        match event["method"].as_str().unwrap_or("") {
            "Network.requestWillBeSent"
                if matches!(p["type"].as_str(), Some("XHR" | "Fetch" | "Document")) =>
            {
                if let Some(prev) = previous {
                    prev.current = false;
                    if p["redirectResponse"].is_object() {
                        set_response(&mut prev.info, &p["redirectResponse"]);
                    }
                    prev.info["duration_ms"] = elapsed_ms(prev.start, p["timestamp"].as_f64());
                }
                if self.entries.len() == NETWORK_LIMIT {
                    self.entries.pop_front();
                }
                self.entries.push_back(NetworkEntry { start:p["timestamp"].as_f64(), finished:false, current:true, info:json!({
                    "target_id":target,"request_id":id,"type":p["type"],"method":p["request"]["method"],"url":network_url(&p["request"]["url"]),
                    "status":null,"duration_ms":null,"error_text":null,"mime_type":null,
                    "request_headers":redact_headers(&p["request"]["headers"]), "response_headers":{}
                }) });
            }
            "Network.requestWillBeSentExtraInfo" => {
                if let Some(prev) = previous {
                    merge_headers(&mut prev.info["request_headers"], &p["headers"]);
                }
            }
            "Network.responseReceivedExtraInfo" => {
                if let Some(prev) = previous {
                    merge_headers(&mut prev.info["response_headers"], &p["headers"]);
                }
            }
            "Network.responseReceived" => {
                if let Some(prev) = previous {
                    set_response(&mut prev.info, &p["response"]);
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                if let Some(prev) = previous {
                    prev.finished = p["errorText"].is_null();
                    prev.info["duration_ms"] = elapsed_ms(prev.start, p["timestamp"].as_f64());
                    prev.info["error_text"] = p["errorText"].clone();
                }
            }
            _ => {}
        }
    }
}

fn elapsed_ms(start: Option<f64>, end: Option<f64>) -> Value {
    match (start, end) {
        (Some(start), Some(end)) => json!(((end - start) * 1000.0).max(0.0).round()),
        _ => Value::Null,
    }
}

fn body_preview(body: &str, base64: bool) -> Value {
    // Base64 must end at a complete quartet; bytes of returned text are also bounded.
    let length = if base64 {
        body.len().min(BODY_BYTES) / 4 * 4
    } else {
        body.floor_char_boundary(BODY_BYTES)
    };
    json!({"body":&body[..length],"body_base64":base64,"body_truncated":length < body.len()})
}

async fn browser_network(session: &Session, args: &Value) -> Result<Value> {
    let filter = args["filter"].as_str().unwrap_or("");
    let min_status = args["min_status"].as_u64();
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(20)
        .clamp(1, NETWORK_LIMIT as u64) as usize;
    let include_body = args["include_body"] == true;
    let snapshot: Vec<_> = session
        .network
        .lock()
        .unwrap()
        .entries
        .iter()
        .rev()
        .filter(|entry| {
            entry.info["url"]
                .as_str()
                .is_some_and(|url| url.contains(filter))
                && min_status.is_none_or(|min| {
                    entry.info["status"]
                        .as_u64()
                        .is_some_and(|status| status >= min)
                })
        })
        .take(limit)
        .map(|entry| (entry.info.clone(), entry.finished && entry.current))
        .collect();
    let mut requests = Vec::with_capacity(snapshot.len());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    for (mut item, available) in snapshot {
        if include_body {
            let target = item["target_id"].as_str().unwrap_or("");
            let commands = if target == session.page_id {
                Some(&session.dialog_commands)
            } else {
                session
                    .inactive_watch
                    .get(target)
                    .map(|page| &page.commands)
            };
            if available && let Some(commands) = commands {
                let params = json!({"requestId":item["request_id"]});
                let reply =
                    timeout_at_page(deadline, commands, "Network.getResponseBody", params).await;
                if let Ok(reply) = reply
                    && let Some(body) = reply["result"]["body"].as_str()
                {
                    let preview = body_preview(body, reply["result"]["base64Encoded"] == true);
                    item.as_object_mut()
                        .unwrap()
                        .extend(preview.as_object().unwrap().clone());
                }
            }
            if item["body"].is_null() {
                item["body_unavailable"] = json!(true);
            }
        }
        requests.push(item);
    }
    Ok(
        json!({"ok":true,"requests":requests,"capacity":NETWORK_LIMIT,"body_limit_bytes":BODY_BYTES}),
    )
}

async fn timeout_at_page(
    deadline: tokio::time::Instant,
    sender: &tokio::sync::mpsc::Sender<PageCommand>,
    method: &'static str,
    params: Value,
) -> Result<Value> {
    let (reply, response) = tokio::sync::oneshot::channel();
    tokio::time::timeout_at(deadline, async {
        sender
            .send((method, params, reply))
            .await
            .context("browser page listener ended")?;
        response.await.context("browser page listener ended")?
    })
    .await
    .context("browser listener command timed out")?
}

struct InactiveWatch {
    dialog: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
    commands: tokio::sync::mpsc::Sender<PageCommand>,
    watch: tokio::task::JoinHandle<()>,
}
impl Drop for InactiveWatch {
    fn drop(&mut self) {
        self.watch.abort();
    }
}

fn blocked_browser_urls() -> Vec<String> {
    crate::tools::process_sandbox::host_control_ports()
        .into_iter()
        .chain([3000, 3001])
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|port| format!("*://*:{port}/*"))
        .collect()
}

struct DownloadCapture {
    dir: std::path::PathBuf,
    events: std::sync::Arc<std::sync::Mutex<DownloadEvents>>,
    watch: tokio::task::JoinHandle<()>,
}
impl Drop for DownloadCapture {
    fn drop(&mut self) {
        self.watch.abort();
    }
}

#[derive(Default)]
struct DownloadEvents {
    pending: std::collections::BTreeMap<String, (String, String)>,
    completed: std::collections::VecDeque<(String, String, String)>,
}

impl DownloadEvents {
    fn observe(&mut self, event: &Value) {
        let p = &event["params"];
        let Some(guid) = p["guid"].as_str().filter(|guid| {
            !guid.is_empty()
                && guid.len() <= 80
                && guid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) else {
            return;
        };
        if event["method"] == "Browser.downloadWillBegin" {
            if self.pending.len() < 100
                && let (Some(frame), Some(name)) =
                    (p["frameId"].as_str(), p["suggestedFilename"].as_str())
            {
                self.pending.insert(
                    guid.to_owned(),
                    (
                        frame.to_owned(),
                        name[..name.floor_char_boundary(256)].to_owned(),
                    ),
                );
            }
        } else if event["method"] == "Browser.downloadProgress"
            && matches!(p["state"].as_str(), Some("completed" | "canceled"))
            && let Some((frame, name)) = self.pending.remove(guid)
            && p["state"] == "completed"
        {
            if self.completed.len() == 100 {
                self.completed.pop_front();
            }
            self.completed.push_back((guid.to_owned(), frame, name));
        }
    }
}

fn download_directory(task: &str, session: &str) -> Result<std::path::PathBuf> {
    use sha2::{Digest, Sha256};
    let base = if cfg!(unix) {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let base = base.join("quickagent-downloads");
    let safe_task = if task.len() <= 128
        && !task.is_empty()
        && task
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        task.to_owned()
    } else {
        format!("task-{:x}", Sha256::digest(task.as_bytes()))
    };
    let task_dir = base.join(safe_task);
    let dir = task_dir.join(session);
    for path in [&base, &task_dir, &dir] {
        create_private_directory(path)?;
    }
    Ok(dir)
}

async fn arm_downloads(session: &mut Session, task: &str) -> Result<()> {
    if session.downloads.is_some() {
        return Ok(());
    }
    let Some(targets) = session.targets.as_ref() else {
        return Ok(());
    };
    let dir = download_directory(task, &session.session_id)?;
    let mut cdp = connect_cdp(targets.endpoint.as_str()).await?;
    cdp.call(
        "Browser.setDownloadBehavior",
        json!({"behavior":"allowAndName","downloadPath":dir,"eventsEnabled":true}),
    )
    .await?;
    let events = std::sync::Arc::new(std::sync::Mutex::new(DownloadEvents::default()));
    let observed = events.clone();
    let watch = tokio::spawn(async move {
        while let Ok(message) = recv(&mut cdp.stream).await {
            if let Ok(event) = serde_json::from_str::<Value>(&message) {
                observed.lock().unwrap().observe(&event);
            }
        }
    });
    session.downloads = Some(DownloadCapture { dir, events, watch });
    Ok(())
}

async fn report_downloads(session: &Session) -> Vec<Value> {
    let Some(capture) = &session.downloads else {
        return Vec::new();
    };
    let mut frames = session.frames.lock().unwrap().clone();
    if let Some(targets) = &session.targets {
        frames.extend(targets.owned.iter().cloned());
    }
    let mut events = capture.events.lock().unwrap();
    // The default context of attached Chrome can emit events from the user's other tabs.
    let mut ready = Vec::new();
    events.completed.retain(|(guid, frame, name)| {
        if !frames.contains(frame) {
            return false;
        }
        let path = capture.dir.join(guid);
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return true;
        }
        ready.push(json!({"path":path,"filename":name}));
        false
    });
    ready
}
