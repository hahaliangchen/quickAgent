async fn fixture() -> (String, tokio::task::JoinHandle<()>) {
    use axum::{
        Router,
        http::{HeaderMap, StatusCode},
        response::Html,
        routing::get,
    };
    let router = Router::new()
        .route("/", get(|| async { Html(r#"<!doctype html><title>Task primary</title>
            <label for=entry>Search</label><input id=entry>
            <button id=save onclick="document.querySelector('output').textContent=entry.value">Save</button>
            <button id=remove onclick="this.remove()">Remove</button>
            <button style="opacity:0">Invisible</button><button hidden>Hidden</button>
            <a href=/next>Next</a><a href=/next target=second rel=opener>New tab</a>
            <a href=/download id=download>Download report</a><output></output>
            <script>fetch('/failure?access_token=fixture-secret', {headers:{Authorization:'Bearer fixture-secret', 'X-Time':'visible'}});</script>"#) }))
        .route("/next", get(|| async { Html("<!doctype html><title>Second tab</title><button>New control</button>") }))
        .route("/download", get(|| async { ([("content-disposition", "attachment; filename=report.txt")], "download fixture") }))
        .route("/failure", get(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer fixture-secret");
            (StatusCode::INTERNAL_SERVER_ERROR, [("set-cookie", "cookie=fixture-secret; Path=/")], "x".repeat(5000))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, server)
}

fn ref_named<'a>(elements: &'a Value, name: &str) -> &'a Value {
    &elements["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("no {name}: {elements}"))["ref"]
}

#[tokio::test]
async fn e2_ax_refs_capture_network_tabs_and_downloads() {
    if browser_path().is_none() {
        eprintln!("Skipping Chrome fixture: no executable");
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "browser-e2-{}",
        crate::media::visual_artifacts::new_id()
    ));
    std::fs::create_dir(&root).unwrap();
    let workspace = crate::tools::Workspace::new(&root).unwrap();
    let context = crate::media::visual_artifacts::VisualContext::mcp(&root);
    let (base, server) = fixture().await;
    let run = async |tool: &str, args: Value| {
        execute_scoped(&workspace, &context, tool, &args)
            .await
            .unwrap()
    };
    let opened = run(
        "browser_open",
        json!({"url":format!("{base}/"),"attach":false}),
    )
    .await;
    let first = opened["page"]["page_id"].clone();
    let state = run("browser_read", json!({})).await;
    assert_eq!(ref_named(&state, "Search"), "e1");
    assert!(!state["elements"].to_string().contains("Invisible"));
    assert!(!state["elements"].to_string().contains("Hidden"));
    let first_save = ref_named(&state, "Save").clone();
    let bounded = run("browser_read", json!({"max_elements":1})).await;
    assert_eq!(bounded["elements"].as_array().unwrap().len(), 1);
    let state = run("browser_read", json!({})).await;
    let input = ref_named(&state, "Search");
    let button = ref_named(&state, "Save");
    assert_eq!(*button, first_save);
    let remove = ref_named(&state, "Remove");
    assert_eq!(
        run("browser_fill", json!({"ref":input,"value":"saved by ref"})).await["length"],
        12
    );
    assert_eq!(
        run("browser_hover", json!({"ref":button})).await["hovered"],
        "Save"
    );
    assert_eq!(
        run("browser_click", json!({"ref":button})).await["match"],
        "ref"
    );
    assert_eq!(
        run("browser_wait", json!({"text":"saved by ref"})).await["matched"],
        true
    );
    assert!(
        run("browser_screenshot", json!({"ref":button,"padding":2})).await["clip"]["width"]
            .is_number()
    );
    let reread = run("browser_read", json!({})).await;
    assert_eq!(ref_named(&reread, "Save"), button);
    run("browser_click", json!({"ref":remove})).await;
    assert_eq!(
        run("browser_click", json!({"ref":remove})).await["error_code"],
        "STALE_REF"
    );
    let network = run(
        "browser_network",
        json!({"min_status":400,"filter":"/failure","include_body":true}),
    )
    .await;
    let failure = &network["requests"][0];
    assert_eq!(failure["status"], 500, "{network}");
    assert_eq!(
        failure["request_headers"]["Authorization"], "[REDACTED]",
        "{failure}"
    );
    assert!(
        failure["response_headers"]
            .as_object()
            .unwrap()
            .iter()
            .any(|(key, value)| key.eq_ignore_ascii_case("set-cookie") && value == "[REDACTED]"),
        "{failure}"
    );
    assert!(!failure.to_string().contains("fixture-secret"));
    assert_eq!(failure["body"].as_str().unwrap().len(), 4096);
    assert_eq!(failure["body_truncated"], true);
    run(
        "browser_click",
        json!({"ref":ref_named(&state,"Download report")}),
    )
    .await;
    let mut downloads = Value::Null;
    for _ in 0..30 {
        downloads = run("browser_wait", json!({"selector":"#download"})).await["downloads"].clone();
        if !downloads.as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(downloads[0]["filename"], "report.txt", "{downloads}");
    let path = Path::new(downloads[0]["path"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "download fixture");
    run("browser_click", json!({"ref":ref_named(&state,"New tab")})).await;
    let mut tabs = Value::Null;
    for _ in 0..20 {
        tabs = run("browser_tabs", json!({})).await;
        if tabs["tabs"].as_array().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let second = &tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["active"] == false)
        .expect("new tab")["target_id"];
    assert_eq!(
        run("browser_switch_tab", json!({"target_id":second})).await["target_id"],
        *second
    );
    assert_eq!(run("browser_read", json!({})).await["title"], "Second tab");
    assert_eq!(
        run("browser_hover", json!({"ref":button})).await["error_code"],
        "STALE_REF"
    );
    run("browser_switch_tab", json!({"target_id":first})).await;
    let state = run("browser_read", json!({})).await;
    let next = ref_named(&state, "Next");
    run("browser_click", json!({"ref":next})).await;
    assert_eq!(
        run("browser_wait", json!({"text":"New control"})).await["matched"],
        true
    );
    assert_eq!(
        run("browser_screenshot", json!({"ref":next})).await["error_code"],
        "STALE_REF"
    );
    assert_eq!(run("browser_close", json!({})).await["closed"], true);
    server.abort();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn e2_ring_redaction_and_url_guards() {
    let mut capture = NetworkCapture::default();
    for n in 0..103 {
        capture.observe("target",&json!({"method":"Network.requestWillBeSent","params":{"requestId":n.to_string(),
            "type":"XHR","request":{"method":"GET","url":"https://example.org/path?q=secret","headers":{"aUtHoRiZaTiOn":"secret","Cookie":"secret","x-api-key":"secret","x-trace":"ok"}}}}));
    }
    assert_eq!(capture.entries.len(), 100);
    let first = &capture.entries[0].info;
    assert_eq!(first["request_id"], "3");
    assert_eq!(first["url"], "https://example.org/path");
    assert!(!first.to_string().contains("secret"));
    assert_eq!(first["request_headers"]["x-trace"], "ok");
    let preview = body_preview(&"€".repeat(2000), false);
    assert_eq!(preview["body"].as_str().unwrap().len(), 4095);
    assert_eq!(preview["body_truncated"], true);
    for url in [
        "http://127.0.0.1:3000/",
        "https://localhost:3001/",
        "http://example.org:3001/",
    ] {
        assert!(
            check_page_url(url, false)
                .unwrap_err()
                .to_string()
                .contains("CONTROL_PORT_BLOCKED")
        );
    }
    assert!(check_page_url("data:text/html,test", false).is_err());
    assert!(check_page_url("about:blank", true).is_ok());
    assert!(cdp_url("ws://127.0.0.1:3000/devtools/browser/test").is_err());
}

#[tokio::test]
async fn e2_persistent_profile_and_attached_close_keep_existing_tabs() {
    if browser_path().is_none() {
        eprintln!("Skipping Chrome fixture: no executable");
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "browser-e2-profile-{}",
        crate::media::visual_artifacts::new_id()
    ));
    let home = root.join("home");
    let work = root.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    assert!(profile_dir(&home, &home, "invalid").is_err());
    assert!(profile_dir(&home, &work, "../invalid").is_err());
    let profile = profile_dir(&home, &work, "fixture").unwrap();
    let (base, server) = fixture().await;
    let initial = launch(false, Some(profile.clone())).await.unwrap();
    let first_id = initial.page_id.clone();
    open(&initial.ws, &format!("{base}/next")).await.unwrap();
    eval(
        &initial.ws,
        "(localStorage.setItem('login','still here'), true)",
    )
    .await
    .unwrap();
    let key = session_key(&work, "owner");
    let endpoint = initial.targets.as_ref().unwrap().endpoint.clone();
    let page = sessions().lock().await.entry(key).or_default().clone();
    *page.lock().await = Some(initial);
    let mut attached = attach_browser(endpoint.clone(), false).await.unwrap();
    assert_ne!(attached.page_id, first_id);
    assert_eq!(
        tab_list(&mut attached).await.unwrap()["tabs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(select_tab(&mut attached, &first_id).await.is_err());
    open(&attached.ws, &format!("{base}/next")).await.unwrap();
    assert_eq!(
        eval(&attached.ws, "localStorage.getItem('login')")
            .await
            .unwrap(),
        "still here"
    );
    let page = sessions()
        .lock()
        .await
        .entry(session_key(&work, "attached"))
        .or_default()
        .clone();
    *page.lock().await = Some(attached);
    assert_eq!(close_session(&work, "attached").await["closed"], true);
    let left = call(endpoint.as_str(), "Target.getTargets", json!({}))
        .await
        .unwrap();
    let pages: Vec<_> = left["result"]["targetInfos"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|target| page_target(target))
        .collect();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["targetId"], first_id);
    assert_eq!(close_session(&work, "owner").await["closed"], true);
    let reopened = launch(false, Some(profile)).await.unwrap();
    open(&reopened.ws, &format!("{base}/next")).await.unwrap();
    assert_eq!(
        eval(&reopened.ws, "localStorage.getItem('login')")
            .await
            .unwrap(),
        "still here"
    );
    let page = sessions()
        .lock()
        .await
        .entry(session_key(&work, "owner"))
        .or_default()
        .clone();
    *page.lock().await = Some(reopened);
    assert_eq!(close_session(&work, "owner").await["closed"], true);
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn e2_chrome_blocks_control_ports_before_redirect_or_fetch() {
    if browser_path().is_none() {
        eprintln!("Skipping Chrome fixture: no executable");
        return;
    }
    use axum::{
        Router,
        response::{Html, Redirect},
        routing::get,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let observed = hits.clone();
    let protected = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = protected.local_addr().unwrap().port();
    crate::tools::process_sandbox::register_control_port(port);
    let protected_server = tokio::spawn(async move {
        axum::serve(
            protected,
            Router::new().fallback(move || async move {
                observed.fetch_add(1, Ordering::SeqCst);
                "blocked target"
            }),
        )
        .await
        .unwrap();
    });
    let safe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let permitted = format!("http://{}", safe.local_addr().unwrap());
    let blocked = format!("http://127.0.0.1:{port}/");
    let redirect = blocked.clone();
    let markup = format!(
        r#"<!doctype html><script>fetch('{blocked}').then(()=>document.title='unexpected').catch(()=>document.title='blocked')</script><a href='{blocked}' target=_blank>Blocked tab</a>"#
    );
    let safe_server = tokio::spawn(async move {
        axum::serve(
            safe,
            Router::new()
                .route("/", get(move || async move { Html(markup) }))
                .route(
                    "/redirect",
                    get(move || async move { Redirect::temporary(&redirect) }),
                ),
        )
        .await
        .unwrap();
    });
    let session = launch(false, None).await.unwrap();
    assert!(
        open(&session.ws, &blocked)
            .await
            .unwrap_err()
            .to_string()
            .contains("CONTROL_PORT_BLOCKED")
    );
    open(&session.ws, &format!("{permitted}/")).await.unwrap();
    let mut title = Value::Null;
    for _ in 0..30 {
        title = eval(&session.ws, "document.title").await.unwrap();
        if title == "blocked" || title == "unexpected" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(title, "blocked");
    assert!(
        click(&session.ws, "", "a", 1)
            .await
            .unwrap_err()
            .to_string()
            .contains("CONTROL_PORT_BLOCKED")
    );
    assert!(
        open(&session.ws, &format!("{permitted}/redirect"))
            .await
            .is_err()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let dir = session.profile.clone().unwrap();
    let _ = call(&session.ws, "Browser.close", json!({})).await;
    drop(session);
    protected_server.abort();
    safe_server.abort();
    let _ = std::fs::remove_dir_all(dir);
}
