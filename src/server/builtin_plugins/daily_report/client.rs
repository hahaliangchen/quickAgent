//! HTTP client for the company daily-report system (RuoYi-Vue-Plus). Every
//! call reuses the stored bearer token; a 401 becomes `AUTH_EXPIRED` so the page
//! can show the login card again instead of a generic failure.
use super::{
    crypto,
    store::{Draft, Session},
};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

/// Marker the page and the AI tools look for before asking for a login again.
pub(super) const AUTH_EXPIRED: &str = "AUTH_EXPIRED";
/// RuoYi's client identifier; the server rejects requests without it.
pub(super) const DEFAULT_CLIENT_ID: &str = "e5cd7e4891bf95d1d19206ce24a7b32e";
/// The team server, used as the login form's default.
pub(super) const DEFAULT_BASE_URL: &str = "http://192.168.100.145:8882/prod-api";

fn expired(cause: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{AUTH_EXPIRED}: {cause}")
}

fn http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("codex-workspace-agent-daily-report")
        .build()
        .context("building the daily-report HTTP client")
}

fn endpoint(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_owned()
}

/// The `{base_url}/auth/login` payload, before encryption.
pub(super) struct LoginRequest {
    pub base_url: String,
    pub client_id: String,
    pub tenant_id: String,
    pub username: String,
    pub password: String,
    /// Optional override of the server's RSA request key.
    pub rsa_public_key: Option<String>,
}

/// Turn the RuoYi envelope into either its `data` or a clear error. The body of
/// a failed request carries the reason the user needs to see.
fn envelope(status: StatusCode, body: Value) -> Result<Value> {
    let code = body
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or(if status.is_success() { 200 } else { status.as_u16() as i64 });
    let message = body
        .get("msg")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if status == StatusCode::UNAUTHORIZED || code == 401 {
        return Err(expired(if message.is_empty() {
            "the company session expired".to_owned()
        } else {
            message
        }));
    }
    ensure!(
        status.is_success() && code == 200,
        "{}",
        if message.is_empty() {
            format!("the daily-report system answered {status}")
        } else {
            message
        }
    );
    // RuoYi returns `R<T>` with `{ code, data }` on single-object endpoints,
    // and `TableDataInfo` with top-level `{ code, rows, total }` (no `data`)
    // on paginated list endpoints.
    Ok(body
        .get("data")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or(body))
}

/// A body the server encrypted cannot be read without the key it wrapped for
/// us; when no response key is configured the request key is the one it used.
async fn read_body(response: reqwest::Response, request_key: &str) -> Result<(StatusCode, Value)> {
    let status = response.status();
    let wrapped = response
        .headers()
        .get("encrypt-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let raw = response.text().await.context("reading the daily-report answer")?;
    let body = match wrapped.filter(|key| !key.trim().is_empty()) {
        Some(wrapped) => {
            let key = crypto::unwrap_key(request_key, &wrapped).unwrap_or_else(|_| request_key.to_owned());
            crypto::decrypt(&raw, &key).unwrap_or(raw)
        }
        None => raw,
    };
    let value = serde_json::from_str(&body).unwrap_or_else(|_| json!({ "msg": body.trim() }));
    Ok((status, value))
}

/// Log in with a username and password, then read the account's identity.
/// `token` login skips the envelope and reuses an existing bearer token.
pub(super) async fn login(request: &LoginRequest) -> Result<Session> {
    let base_url = endpoint(&request.base_url);
    ensure!(!base_url.is_empty(), "the service address is required");
    let http = http_client()?;
    let key = crypto::random_key()?;
    let payload = json!({
        "tenantId": request.tenant_id,
        "username": request.username,
        "password": request.password,
        "clientId": request.client_id,
        "grantType": "password",
    });
    let encrypted = crypto::encrypt(&payload.to_string(), &key)?;
    let wrapped = crypto::wrap_key(
        request
            .rsa_public_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .unwrap_or(crypto::DEFAULT_RSA_PUBLIC_KEY),
        &key,
    )?;
    let response = http
        .post(format!("{base_url}/auth/login"))
        .header("encrypt-key", wrapped)
        .header("clientid", &request.client_id)
        .header("Content-Type", "application/json;charset=utf-8")
        .body(encrypted)
        .send()
        .await
        .context("the daily-report login request did not reach the server")?;
    let (status, body) = read_body(response, &key).await?;
    let data = envelope(status, body)?;
    let token = data
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .context("the login answer carried no access_token")?
        .to_owned();
    let session = Session {
        base_url,
        client_id: request.client_id.clone(),
        tenant_id: request.tenant_id.clone(),
        access_token: token,
        updated_at: now(),
        ..Session::default()
    };
    Api::new(session.clone())?.identify().await
}

/// A token entered by hand, kept when the encrypted login is unavailable.
pub(super) async fn login_with_token(
    base_url: &str,
    token: &str,
    client_id: &str,
    tenant_id: &str,
) -> Result<Session> {
    let base_url = endpoint(base_url);
    ensure!(!base_url.is_empty(), "the service address is required");
    ensure!(!token.trim().is_empty(), "the token is required");
    let session = Session {
        base_url,
        client_id: client_id.to_owned(),
        tenant_id: tenant_id.to_owned(),
        access_token: token.trim().to_owned(),
        updated_at: now(),
        ..Session::default()
    };
    Api::new(session.clone())?.identify().await
}

pub(super) fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Calls against the company server with a stored session.
pub(super) struct Api {
    http: Client,
    session: Session,
}

impl Api {
    pub(super) fn new(session: Session) -> Result<Self> {
        ensure!(
            !session.access_token.is_empty() && !session.base_url.is_empty(),
            "{AUTH_EXPIRED}: no stored daily-report session"
        );
        Ok(Self { http: http_client()?, session })
    }


    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let url = format!("{}{path}", self.session.base_url);
        let mut request = self
            .http
            .request(method, url)
            .header("clientid", &self.session.client_id)
            .bearer_auth(&self.session.access_token);
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("the daily-report request did not reach the server")?;
        let (status, body) = read_body(response, "").await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(expired(
                body.get("msg").and_then(Value::as_str).unwrap_or("the company session expired"),
            ));
        }
        envelope(status, body)
    }

    /// Read the account behind the token, which also proves the token works.
    async fn identify(self) -> Result<Session> {
        let mut session = self.session.clone();
        let data = self.call(Method::GET, "/system/user/getInfo", &[], None).await?;
        let user = data.get("user").cloned().unwrap_or(data);
        session.user_id = text(&user, &["userId", "id"]);
        session.user_name = text(&user, &["userName", "username"]);
        session.nick_name = text(&user, &["nickName", "nickname", "name"]);
        Ok(session)
    }

    /// Every project the report form may choose from, in server order.
    pub(super) async fn projects(&self) -> Result<Vec<Value>> {
        let data = self
            .call(
                Method::GET,
                "/system/project/list",
                &[("pageNum", "1".to_owned()), ("pageSize", "200".to_owned())],
                None,
            )
            .await?;
        let rows = data
            .get("rows")
            .or_else(|| data.get("list"))
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| data.as_array().cloned())
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|row| {
                let id = row
                    .get("projectId")
                    .or_else(|| row.get("id"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let name = text(&row, &["projectName", "name", "title"]);
                json!({ "project_id": id, "project_name": name })
            })
            .filter(|row| !row["project_name"].as_str().unwrap_or_default().is_empty())
            .collect())
    }

    /// Reports already on the server, filtered by year, month or one day.
    pub(super) async fn reports(&self, query: &[(&str, String)]) -> Result<Value> {
        let mut params = vec![
            ("pageNum", query_value(query, "pageNum").unwrap_or_else(|| "1".to_owned())),
            ("pageSize", query_value(query, "pageSize").unwrap_or_else(|| "20".to_owned())),
        ];
        for name in ["reportYear", "reportMonth", "reportDate", "userId"] {
            if let Some(value) = query_value(query, name) {
                params.push((name, value));
            }
        }
        let data = self.call(Method::GET, "/report/daily/list", &params, None).await?;
        let rows = data
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| data.as_array().cloned())
            .unwrap_or_default();
        Ok(json!({
            "total": data.get("total").cloned().unwrap_or_else(|| json!(rows.len())),
            "rows": rows,
        }))
    }

    /// One report with its project rows.
    pub(super) async fn report(&self, id: &str) -> Result<Value> {
        self.call(Method::GET, &format!("/report/daily/{id}"), &[], None)
            .await
    }

    /// Create or update the report, depending on the draft's action.
    pub(super) async fn submit(&self, draft: &Draft) -> Result<Value> {
        let updating = draft.action == "update" && draft.report_id.as_deref().is_some_and(|id| !id.is_empty());
        let method = if updating { Method::PUT } else { Method::POST };
        self.call(method, "/report/daily", &[], Some(draft.payload(&self.session)))
            .await
    }

    pub(super) async fn delete(&self, id: &str) -> Result<Value> {
        self.call(Method::DELETE, &format!("/report/daily/{id}"), &[], None)
            .await
    }
}

fn query_value(query: &[(&str, String)], name: &str) -> Option<String> {
    query
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.clone())
        .filter(|value| !value.trim().is_empty())
}

/// RuoYi answers ids as numbers and names as strings; both become text here.
fn text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key))
        .map(|value| match value {
            Value::String(text) => text.trim().to_owned(),
            Value::Number(number) => number.to_string(),
            _ => String::new(),
        })
        .unwrap_or_default()
}
