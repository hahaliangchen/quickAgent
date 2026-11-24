//! Tests for the internal daily report. They run only with the feature on:
//! `cargo test --features internal-daily-report daily_report`.
use super::{
    activity, client,
    crypto::{self, DEFAULT_RSA_PUBLIC_KEY},
    package,
    store::{Draft, DraftProject, Session, Store},
};
use axum::{Json, Router, extract::State, routing::get, routing::post};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A private key generated for this test alone; it is not the team server's.
const TEST_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIICdgIBADANBgkqhkiG9w0BAQEFAASCAmAwggJcAgEAAoGBALgmXBwV30LO/Q7M
XetInmmobykws6ZCoSOBDIeivmqgXxGOaD+C1+KMbejv6JpLN0t3FSBtn5StD3yR
rQc9fUUmPiMyemPk3CdwEzUmCU1GzSaTD/svKoBDos8dmtUpe/69PbkH2TGh+ERU
Tgb8g2C8eA23WGIdnKIoKIR2R3yNAgMBAAECgYEAhZpvIYFM5e++8cmcKmQZapbl
b4ECFhIpTtUF/jI47KtFzjr/r3SCpL81ztagb7E6gOn4zfo62Wdl4P7gBmy6QQIH
2A6qfpqU9W3mZrJcWNiXWL4KWdIC4t7PI3l4a24ZPLRSwTzmtiOSSFIB2S9UexJV
gT0cVS3M4jToVSJ7bgECQQDd+4POMPsSWPP408/6MlVacZczN4f3sUOO+U1/3MpF
pW/3O5yX3SCtsDy3X9RRuVJlPq9D5PKihJ068MNnQMVhAkEA1F6mw981PMuxDJSA
tjxt76xVvHov6hGkwlkqDAeKXs5LVd9dae7prnsBn9kCvi8yw8NabmGn5N5CYZBX
wN5arQJAK1aefhQMAKIDR9I6qZNIHLdOmL4/unkOQoyjt9HoX76T5IXr5a5TYfWl
dPutnSL8Vi3t0+iQAnC3rbkxvSflIQJAcz5Z6Pj7ft5aWaVMo5fvYJx4eiskTTT9
36lbM+Pzprb0Esl/e7j2foSpwQCWiHY8ahdcv4GnBn/GLgOSW0tOkQJAIz2s6u4n
sNFS9rCrT/wYrCQL4Cg/JLxRmiEzcDWHIe2Mr/jsIJh16c9z4WJW7NvtkUP4OOx6
L6lbqLLJdNpJZA==
-----END PRIVATE KEY-----";
/// Public half of the key above, as Base64 SubjectPublicKeyInfo.
const TEST_PUBLIC_KEY: &str = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQC4JlwcFd9Czv0OzF3rSJ5pqG8pMLOmQqEjgQyHor5qoF8Rjmg/gtfijG3o7+iaSzdLdxUgbZ+UrQ98ka0HPX1FJj4jMnpj5NwncBM1JglNRs0mkw/7LyqAQ6LPHZrVKXv+vT25B9kxofhEVE4G/INgvHgNt1hiHZyiKCiEdkd8jQIDAQAB";
/// The AES key both sides shared when the ciphertext below was produced.
const TEST_AES_KEY: &str = "abcdefghijklmnopqrstuvwxyz012345";
/// Browser output: crypto.js AES-256-ECB + PKCS7, Base64.
const TEST_AES_CIPHER: &str = "/RUI4qJ8oD4H03B5NeuidllcnSs1WVR4oY4UJkbg3bblxCK04uF50kiL0kj8NcaJSeo+1LhFhBeHZR7iSEy/0A==";
const TEST_AES_PLAIN: &str = "{\"tenantId\":\"000000\",\"username\":\"test\",\"password\":\"p@ss\"}";
/// OpenSSL output of RSA PKCS1v15 over TEST_AES_KEY with TEST_PUBLIC_KEY.
const TEST_WRAPPED_KEY: &str = "hhNSh0LWIZqIKnr6l4jM5sPuuuT77De9N78xAxTnyhAbBuWEEO3ZLXGwGLuUhP9MFtppmso5zLMY6ce056w7rUOOzTNk2bpRXTufJ9ewvMrXw5EdeIxXz1HG542C+hPiwe9GB7CFnPLMedGGBVsLAKStV1dmINhh24vIThP+OvI=";

fn temp_dir(label: &str) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("daily-report-{label}-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn draft() -> Draft {
    Draft {
        report_date: "2026-11-24".to_owned(),
        projects: vec![
            DraftProject {
                project_id: Some(1001),
                project_name: "雅砻江知识运营管理".to_owned(),
                work_content: "1. 具体任务：日报插件\n2. 今日完成：后端与页面".to_owned(),
            },
            DraftProject {
                project_id: Some(1002),
                project_name: "数据中台".to_owned(),
                work_content: "整理指标口径".to_owned(),
            },
        ],
        problems: "无".to_owned(),
        remark: String::new(),
        overtime_hours: 1.5,
        leave_hours: 0.0,
        reimbursement: 0.0,
        reimbursement_reason: String::new(),
        action: "create".to_owned(),
        report_id: None,
        origin: "ai".to_owned(),
        updated_at: String::new(),
    }
}

/// The envelope has to stay byte-compatible with the browser build, otherwise
/// the company server decrypts a body full of noise and rejects the login.
#[test]
fn aes_envelope_matches_the_browser_implementation() {
    assert_eq!(crypto::decrypt(TEST_AES_CIPHER, TEST_AES_KEY).unwrap(), TEST_AES_PLAIN);
    assert_eq!(crypto::encrypt(TEST_AES_PLAIN, TEST_AES_KEY).unwrap(), TEST_AES_CIPHER);
    // A wrong key must not silently produce garbage that the server accepts.
    assert!(crypto::decrypt(TEST_AES_CIPHER, &"z".repeat(32)).is_err());
    // A rejected key length is a configuration error, not a panic.
    assert!(crypto::encrypt("x", "short").is_err());
}

#[test]
fn rsa_wraps_base64_login_key_matching_ruoyi_protocol() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, pkcs8::DecodePrivateKey};

    let wrapped = crypto::wrap_key(TEST_PUBLIC_KEY, TEST_AES_KEY).unwrap();
    // Verify the raw RSA plaintext is Base64(TEST_AES_KEY), as RuoYi's Java
    // `DecryptRequestBodyWrapper` expects (`decryptByBase64(decryptByRsa(...))`).
    let private = RsaPrivateKey::from_pkcs8_pem(TEST_PRIVATE_KEY.trim()).unwrap();
    let raw = private
        .decrypt(Pkcs1v15Encrypt, &STANDARD.decode(&wrapped).unwrap())
        .unwrap();
    assert_eq!(String::from_utf8(raw).unwrap(), STANDARD.encode(TEST_AES_KEY.as_bytes()));
    let unwrapped = crypto::unwrap_key(TEST_PRIVATE_KEY, &wrapped).unwrap();
    assert_eq!(unwrapped, TEST_AES_KEY);
    // The 512-bit company public key must also wrap a 44-byte Base64(32-byte key).
    assert!(crypto::wrap_key(DEFAULT_RSA_PUBLIC_KEY, TEST_AES_KEY).is_ok());
    assert!(crypto::unwrap_key("not a key", &wrapped).is_err());
    // Reject a legacy raw-key (non-Base64-wrapped) ciphertext mismatch.
    assert!(crypto::unwrap_key(TEST_PRIVATE_KEY, TEST_WRAPPED_KEY).is_err());
}

/// A generated key is 32 alphanumerics, as the server's AES call expects.
#[test]
fn generated_login_keys_are_32_alphanumerics() {
    let key = crypto::random_key().unwrap();
    assert_eq!(key.len(), 32);
    assert!(key.chars().all(|character| character.is_ascii_alphanumeric()));
    assert_ne!(key, crypto::random_key().unwrap());
}

#[cfg(unix)]
#[test]
fn session_file_is_owner_only_and_never_shows_the_token() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("session");
    let store = Store::open(&dir).unwrap();
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "the plugin directory holds a bearer token");
    store
        .save_session(&Session {
            base_url: "http://127.0.0.1:1/prod-api".to_owned(),
            client_id: "client".to_owned(),
            tenant_id: "000000".to_owned(),
            access_token: "super-secret-token".to_owned(),
            user_id: "42".to_owned(),
            user_name: "liang".to_owned(),
            nick_name: "梁辰".to_owned(),
            updated_at: "2026-11-24T09:00:00+08:00".to_owned(),
        })
        .unwrap();
    let file = dir.join("session.json");
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the session file must not be group readable");
    // The token is on disk but must never reach a page, a log or a tool result.
    let status = store.status().unwrap();
    let rendered = status.to_string();
    assert!(!rendered.contains("super-secret-token"), "status leaked the token: {rendered}");
    assert_eq!(status["session"]["logged_in"], true);
    assert_eq!(status["session"]["nick_name"], "梁辰");
    assert!(status["session"].get("access_token").is_none());
    store.logout().unwrap();
    assert!(store.session().unwrap().is_none());
}

#[test]
fn guide_starts_empty_and_round_trips() {
    let dir = temp_dir("guide");
    let store = Store::open(&dir).unwrap();
    assert!(store.guide_path().is_file(), "the markdown page needs a real file");
    assert_eq!(store.guide().unwrap(), "");
    store.write_guide("# 写作规范\n\n- 按项目分段").unwrap();
    assert_eq!(store.guide().unwrap(), "# 写作规范\n\n- 按项目分段");
    // A reopened store keeps the body: one reader, one writer, no cache.
    let reopened = Store::open(&dir).unwrap();
    assert!(reopened.guide().unwrap().contains("按项目分段"));
}

#[test]
fn draft_builds_the_company_payload() {
    let dir = temp_dir("draft");
    let store = Store::open(&dir).unwrap();
    let mut value = draft();
    store.save_draft(&value).unwrap();
    assert_eq!(store.draft().unwrap().unwrap().projects.len(), 2);
    let session = Session {
        user_id: "42".to_owned(),
        user_name: "liang".to_owned(),
        nick_name: "梁辰".to_owned(),
        ..Session::default()
    };
    let payload = value.payload(&session);
    assert_eq!(payload["reportDate"], "2026-11-24");
    assert_eq!(payload["reportYear"], "2026");
    assert_eq!(payload["reportMonth"], "11");
    assert_eq!(payload["userName"], "梁辰");
    assert_eq!(payload["userId"], "42");
    assert_eq!(payload["overtimeHours"], 1.5);
    assert_eq!(payload["projects"][0]["projectId"], 1001);
    assert_eq!(payload["projects"][1]["projectName"], "数据中台");
    assert!(payload.get("id").is_none(), "a create must not carry a row id");
    value.action = "update".to_owned();
    value.report_id = Some("77".to_owned());
    assert_eq!(value.payload(&session)["id"], "77");
    store.clear_draft().unwrap();
    assert!(store.draft().unwrap().is_none());
}

/// A stand-in for the company server: the login answer is plain JSON because
/// the real one is only encrypted when it is asked to be.
async fn mock_server() -> (String, Arc<Mutex<Vec<(String, Value)>>>, Arc<Mutex<Vec<String>>>) {
    type Log = Arc<Mutex<Vec<(String, Value)>>>;
    type Bodies = Arc<Mutex<Vec<String>>>;
    let log: Log = Arc::default();
    let bodies: Bodies = Arc::default();
    let state = (log.clone(), bodies.clone());
    let app = Router::new()
        .route(
            "/prod-api/auth/login",
            post(|State((log, bodies)): State<(Log, Bodies)>, body: String| async move {
                bodies.lock().unwrap().push(body);
                log.lock().unwrap().push(("login".to_owned(), Value::Null));
                Json(json!({"code": 200, "data": {"access_token": "token-123"}}))
            }),
        )
        .route(
            "/prod-api/system/user/getInfo",
            get(|| async { Json(json!({"code": 200, "data": {"user": {"userId": 42, "userName": "liang", "nickName": "梁辰"}}})) }),
        )
        .route(
            "/prod-api/system/project/list",
            get(|| async { Json(json!({"code": 200, "msg": "查询成功", "total": 1, "rows": [{"projectId": 1001, "projectName": "雅砻江知识运营管理"}]})) }),
        )
        .route(
            "/prod-api/report/daily",
            post(|State((log, _)): State<(Log, Bodies)>, Json(body): Json<Value>| async move {
                log.lock().unwrap().push(("POST".to_owned(), body));
                Json(json!({"code": 200, "data": {"id": "9"}}))
            })
            .put(|State((log, _)): State<(Log, Bodies)>, Json(body): Json<Value>| async move {
                log.lock().unwrap().push(("PUT".to_owned(), body));
                Json(json!({"code": 200, "data": {"id": "9"}}))
            }),
        )
        .route(
            "/prod-api/report/daily/list",
            get(|| async { Json(json!({"code": 200, "msg": "查询成功", "total": 1, "rows": [{"id": "9", "reportDate": "2026-11-24"}]})) }),
        )
        .route(
            "/prod-api/report/daily/expired",
            get(|| async { Json(json!({"code": 401, "msg": "认证失败，无法访问系统资源"})) }),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/prod-api", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, log, bodies)
}

/// The login must not put the password on the wire in readable form.
#[tokio::test]
async fn client_encrypts_the_login_and_reads_the_account_back() {
    let (base, _, bodies) = mock_server().await;
    let session = client::login(&client::LoginRequest {
        base_url: base,
        client_id: "client".to_owned(),
        tenant_id: "000000".to_owned(),
        username: "liang".to_owned(),
        password: "s3cret-password".to_owned(),
        rsa_public_key: Some(TEST_PUBLIC_KEY.to_owned()),
    })
    .await
    .unwrap();
    assert_eq!(session.access_token, "token-123");
    assert_eq!(session.user_id, "42");
    assert_eq!(session.nick_name, "梁辰");
    let sent = bodies.lock().unwrap().join("");
    assert!(!sent.contains("s3cret-password"), "the password travelled in clear text: {sent}");
    assert!(crypto::decrypt(&sent, TEST_AES_KEY).is_err(), "the body is encrypted with a fresh key");
}

#[tokio::test]
async fn client_reads_projects_and_sends_the_report_payload() {
    let (base, log, _) = mock_server().await;
    let session = Session {
        base_url: base,
        client_id: "client".to_owned(),
        access_token: "token-123".to_owned(),
        user_id: "42".to_owned(),
        user_name: "liang".to_owned(),
        nick_name: "梁辰".to_owned(),
        ..Session::default()
    };
    let api = client::Api::new(session).unwrap();
    let projects = api.projects().await.unwrap();
    assert_eq!(projects[0]["project_id"], 1001);
    assert_eq!(projects[0]["project_name"], "雅砻江知识运营管理");
    let reports = api.reports(&[]).await.unwrap();
    assert_eq!(reports["total"], 1);
    assert_eq!(reports["rows"][0]["id"], "9");
    api.submit(&draft()).await.unwrap();
    let mut updating = draft();
    updating.action = "update".to_owned();
    updating.report_id = Some("9".to_owned());
    api.submit(&updating).await.unwrap();
    let calls = log.lock().unwrap().clone();
    let created = calls.iter().find(|(method, _)| method == "POST").expect("a create call").1.clone();
    assert_eq!(created["reportDate"], "2026-11-24");
    assert_eq!(created["projects"].as_array().unwrap().len(), 2);
    assert!(created.get("id").is_none());
    let updated = calls.iter().find(|(method, _)| method == "PUT").expect("an update call").1.clone();
    assert_eq!(updated["id"], "9");
}

/// An expired session has to be recognisable, so the page can ask again.
#[tokio::test]
async fn client_reports_an_expired_session() {
    let (base, _, _) = mock_server().await;
    let session = Session {
        base_url: base,
        client_id: "client".to_owned(),
        access_token: "stale".to_owned(),
        ..Session::default()
    };
    let api = client::Api::new(session).unwrap();
    let error = format!("{:#}", api.report("expired").await.unwrap_err());
    assert!(error.contains(client::AUTH_EXPIRED), "got {error}");
    assert!(!client::Api::new(Session::default()).is_ok(), "an empty session cannot call out");
}

/// Submitting is a two-step human decision: prepare, show, then allow once.
#[tokio::test]
async fn submit_demands_approval_and_a_prepared_draft() {
    let dir = temp_dir("approval");
    let workspace = temp_dir("workspace");
    let unapproved = package::execute(&dir, &workspace, "daily_report__submit", json!({})).await;
    assert!(
        format!("{:#}", unapproved.unwrap_err()).contains("APPROVAL_REQUIRED"),
        "the tool must refuse without a human"
    );
    let approved = package::scoped("task".to_owned(), true, package::execute(&dir, &workspace, "daily_report__submit", json!({})));
    let error = format!("{:#}", approved.await.unwrap_err());
    assert!(error.contains("prepare_draft"), "got {error}");
}

/// The draft tool stores what the page shows and refuses an empty report.
#[tokio::test]
async fn prepare_draft_stores_the_reviewed_report() {
    let dir = temp_dir("prepare");
    let workspace = temp_dir("workspace");
    let stored = package::execute(
        &dir,
        &workspace,
        "daily_report__prepare_draft",
        json!({
            "report_date": "2026-11-24",
            "projects": [{ "project_id": 1001, "project_name": "雅砻江知识运营管理", "work_content": "完成了日报插件" }],
            "problems": "无",
            "overtime_hours": 2,
        }),
    )
    .await
    .unwrap();
    assert_eq!(stored["stored"], true);
    assert_eq!(stored["project_count"], 1);
    let saved = Store::open(&dir).unwrap().draft().unwrap().unwrap();
    assert_eq!(saved.origin, "ai");
    assert_eq!(saved.overtime_hours, 2.0);
    assert_eq!(saved.projects[0].project_name, "雅砻江知识运营管理");
    let empty = package::execute(&dir, &workspace, "daily_report__prepare_draft", json!({ "report_date": "2026-11-24", "projects": [] })).await;
    assert!(empty.is_err(), "a report with no project row is not a report");
    let bad_day = package::execute(&dir, &workspace, "daily_report__prepare_draft", json!({ "report_date": "11/24/2026", "projects": [{ "project_name": "x", "work_content": "y" }] })).await;
    assert!(bad_day.is_err(), "the server expects YYYY-MM-DD");
}

/// Without a login the tool explains itself instead of failing opaquely.
#[tokio::test]
async fn list_explains_a_missing_login() {
    let dir = temp_dir("list");
    let workspace = temp_dir("workspace");
    let answer = package::execute(&dir, &workspace, "daily_report__list", json!({})).await.unwrap();
    assert_eq!(answer["logged_in"], false);
    assert!(answer["hint"].as_str().unwrap_or_default().contains("工作台"));
    let guide = package::execute(&dir, &workspace, "daily_report__read_guide", json!({})).await.unwrap();
    assert_eq!(guide["empty"], true);
    package::execute(&dir, &workspace, "daily_report__write_guide", json!({ "content": "# 规范" })).await.unwrap();
    let guide = package::execute(&dir, &workspace, "daily_report__read_guide", json!({})).await.unwrap();
    assert_eq!(guide["empty"], false);
    assert_eq!(guide["content"], "# 规范");
}

/// Today's aggregation reads the real state database and never invents rows.
#[tokio::test]
async fn today_activity_reads_the_state_database() {
    let workspace = temp_dir("activity-workspace");
    let conn = crate::server::agent_service::open_db(&workspace).unwrap();
    let now = crate::server::agent_service::now();
    conn.execute(
        "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES('t1','写日报','m','finished',?1,?1)",
        [now],
    )
    .unwrap();
    // The session table belongs to the UI layer; this test adds it first.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_session_meta(task_id TEXT PRIMARY KEY,title TEXT,pinned INTEGER NOT NULL DEFAULT 0,archived INTEGER NOT NULL DEFAULT 0,workspace TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_session_meta(task_id,title,workspace) VALUES('t1','日报插件',?1)",
        [workspace.display().to_string()],
    )
    .unwrap();
    drop(conn);
    let day = activity::today(&workspace, None).unwrap();
    assert_eq!(day["totals"]["sessions"], 1);
    let project = &day["projects"][0];
    assert_eq!(project["sessions"][0]["title"], "日报插件");
    // The model follows this id into session.read_session_history for details.
    assert_eq!(project["sessions"][0]["task_id"], "t1");
    assert!(project["workspace"].as_str().unwrap_or_default().contains("daily-report-activity-workspace"));
    // An explicit date with no work is empty, not yesterday.
    let quiet = activity::today(&workspace, Some("2020-01-01")).unwrap();
    assert_eq!(quiet["totals"]["sessions"], 0);
    assert!(activity::today(&workspace, Some("2020/01/01")).is_err());
}

/// The manifest sells exactly what the model receives.
#[test]
fn tool_definitions_match_the_manifest() {
    let definitions = package::definitions();
    let names = definitions
        .iter()
        .map(|definition| definition["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 6);
    for name in &names {
        assert!(name.starts_with("daily_report__"), "{name}");
    }
    for expected in [
        "daily_report__read_guide",
        "daily_report__write_guide",
        "daily_report__today_activity",
        "daily_report__list",
        "daily_report__prepare_draft",
        "daily_report__submit",
    ] {
        assert!(names.iter().any(|name| name == expected), "{expected} is missing");
    }
    for definition in &definitions {
        let schema = &definition["inputSchema"];
        assert_eq!(schema["type"], "object", "{} has no input schema", definition["name"]);
        assert_eq!(schema["additionalProperties"], false);
        assert!(definition["description"].as_str().is_some_and(|text| !text.trim().is_empty()));
    }
    // Only the submission leaves the host, and only it needs approval.
    assert!(super::package::requires_approval("daily_report__submit", &json!({})));
    assert!(!super::package::requires_approval("daily_report__prepare_draft", &json!({})));
}
