use super::*;
use crate::server::{agent_service, skill_packages};
use axum::{extract::Path as RoutePath, http::Method};
use std::{path::PathBuf, sync::Mutex as SyncMutex, time::Duration};

#[test]
fn environment_routes_are_scoped_and_dynamic_takes_precedence() {
    let variables = HashMap::from([
        ("EXTERNAL_LLM_BASE_URL", "http://127.0.0.1:9004/internal/v1/"),
        ("EXTERNAL_LLM_API_KEY", "env-fixture-secret"),
        ("EXTERNAL_LLM_MODEL", "rag-default"),
        ("EXTERNAL_LLM_PLUGINS", " rag-kg, example-plugin , "),
    ]);
    let get = |key: &str| variables.get(key).map(|v| (*v).to_owned());
    let registry = external_models::ExternalProviderRegistry::from_environment(get).unwrap();
    let config: AiProxyConfig = serde_json::from_value(json!({
        "orchestrator_provider":"external-upstream", "orchestrator_model":"local-model",
        "providers":{"external-upstream":{"url":"https://user.example/v1","model_map":{"local-model":"local-model"}}},
        "visual_tool":{"provider":"external-upstream","model":"local-model"},
        "delegate_agent":{"provider":"external-upstream","model":"local-model"}
    })).unwrap();
    let main = catalog_with_external(&config, registry.catalog_only());
    assert_eq!(main["models"].as_array().unwrap().len(), 1);
    assert_eq!(main["models"][0]["id"], "local-model");
    assert_eq!(plugin_catalog_with_external(&config, "ppt-master", &registry), main);

    let scoped = restrict_with_external(&config, "rag-kg", &registry).unwrap();
    assert_eq!(scoped.default_provider.as_deref(), Some("external-upstream"));
    assert_eq!(scoped.orchestrator_model.as_deref(), Some("rag-default"));
    assert_eq!(scoped.providers.len(), 1);
    let route = &scoped.providers["external-upstream"];
    assert_eq!(route.api_key, "env-fixture-secret");
    assert_eq!(route.url, "http://127.0.0.1:9004/internal/v1");
    assert_eq!(route.api_type, "openai-completions");
    assert!(scoped.visual_tool.provider.is_none() && scoped.delegate_agent.provider.is_none());
    let models = plugin_catalog_with_external(&config, "rag-kg", &registry);
    assert_eq!(models["models"].as_array().unwrap().len(), 1);
    assert_eq!(models["models"][0]["id"], "rag-default");
    for private in [&route.url, &route.api_key] {
        assert!(!models.to_string().contains(private.as_str()));
        assert!(!settings_view(&config, String::new()).to_string().contains(private.as_str()));
    }
    assert!(registry.for_plugin("example-plugin").is_some());

    let dynamic: external_models::ExternalModelProvider = serde_json::from_value(json!({
        "url":"https://dynamic.example/v1","model":"new-model","protocol":"anthropic-messages"
    })).unwrap();
    registry.bind("rag-kg", dynamic.validate().unwrap());
    let models = plugin_catalog_with_external(&config, "rag-kg", &registry);
    assert_eq!(models["models"][0]["id"], "new-model");
    assert_eq!(models["models"][0]["provider"], "ext-rag-kg");
    registry.unbind("rag-kg");
    assert_eq!(registry.for_plugin("rag-kg").unwrap().model, "rag-default");

    let unscoped = external_models::ExternalProviderRegistry::from_environment(|key| match key {
        "EXTERNAL_LLM_PROVIDER_ID" => Some("catalog_only".into()),
        "EXTERNAL_LLM_PLUGINS" => None,
        "EXTERNAL_LLM_PROTOCOL" => Some("openai-responses".into()),
        _ => get(key),
    }).unwrap();
    let public = catalog_with_external(&config, unscoped.catalog_only());
    assert_eq!(public["defaults"], main["defaults"]);
    assert_eq!(public["models"][0]["provider"], "catalog_only");
    assert_eq!(public["models"][0]["api_type"], "openai-responses");
    assert_eq!(public["models"][0]["available_for_agent"], false);
    assert_eq!(plugin_catalog_with_external(&config, "rag-kg", &unscoped), main);
    assert!(!config.providers.contains_key("catalog_only"));
}

#[test]
fn invalid_external_providers_are_rejected_without_echoing_secrets() {
    for (field, invalid) in [
        ("url", "ftp://example.com"), ("url", "https://example.com/\n/v1"),
        ("url", "https://secret@example.com"), ("model", "  "),
        ("model", "bad\nmodel"), ("protocol", "unknown"),
        ("provider_id", ""), ("provider_id", "invalid.id"), ("api_key", "secret\nkey"),
    ] {
        let mut input = json!({"url":"https://example.com/v1","model":"model"});
        input[field] = json!(invalid);
        let provider: external_models::ExternalModelProvider = serde_json::from_value(input).unwrap();
        let error = provider.validate().err().expect(field).to_string();
        assert!(!error.contains("secret"));
    }
    let unknown = json!({"url":"http://example.com","model":"test","unknown":true});
    assert!(serde_json::from_value::<external_models::ExternalModelProvider>(unknown).is_err());
}

struct LocalServer {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl LocalServer {
    async fn start(app: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { base, task }
    }
}
impl Drop for LocalServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mock_upstream(
    model: &'static str,
    token: &'static str,
    tool: bool,
) -> (LocalServer, Arc<SyncMutex<Vec<Value>>>) {
    let requests = Arc::new(SyncMutex::new(Vec::<Value>::new()));
    let capture = requests.clone();
    let server = LocalServer::start(Router::new().route("/v1/chat/completions", post(
        move |headers: HeaderMap, Json(body): Json<Value>| {
            let capture = capture.clone();
            async move {
                assert_eq!(headers["authorization"], format!("Bearer {token}"));
                assert!(!headers["x-agent-affinity"].is_empty());
                assert_eq!(body["model"], model);
                assert_eq!(body["stream"], true);
                let index = {
                    let mut requests = capture.lock().unwrap();
                    requests.push(body);
                    requests.len()
                };
                let (delta, finish) = if index == 1 && tool {
                    let arguments = json!({"code":"const identity = host.context(); const receipt = ioc_fixture.ping({ model: identity.model, provider: identity.provider }); receipt;"}).to_string();
                    (json!({"tool_calls":[{"index":0,"id":"mock_run_js","type":"function",
                        "function":{"name":"run_js","arguments":arguments}}]}), "tool_calls")
                } else {
                    (json!({"content":"mock answer"}), "stop")
                };
                let output = json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                let end = json!({"choices":[{"index":0,"delta":{},"finish_reason":finish}]});
                ([(header::CONTENT_TYPE,"text/event-stream")], format!("data: {output}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
            }
        }
    ))).await;
    (server, requests)
}

async fn response_json(request: reqwest::RequestBuilder, expected: StatusCode) -> Value {
    let response = request.send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, expected, "{text}");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn remote_install_and_hot_swap_keep_credentials_in_memory_and_run_sse_tools() {
    const PLUGIN: &str = "ioc-remote-fixture";
    const FIRST_SECRET: &str = "first-model-exclusive-secret";
    const SECOND_SECRET: &str = "second-model-exclusive-secret";
    let root = std::env::temp_dir().join(format!("external-model-http-{}", agent_service::uuid_like()));
    std::fs::create_dir_all(&root).unwrap();
    let config_path: PathBuf = root.join("config.yml");
    let package_path = root.join("skill-packages.json");
    write_yaml_atomic(&config_path, &json!([
        {"id":"llm-pi-ai","config":{"providers":{"user":{"baseURL":"http://127.0.0.1:1/v1","api":"openai-completions","models":[{"id":"user-model"}]}}}},
        {"id":"agent-default-model","config":{"provider":"user","model":"user-model"}}
    ])).unwrap();

    let calls = Arc::new(SyncMutex::new(Vec::<Value>::new()));
    let receive = calls.clone();
    let remote = LocalServer::start(Router::new().route("/remote/api/plugin/{method}", post(
        move |RoutePath(method): RoutePath<String>, headers: HeaderMap, Json(body): Json<Value>| {
            let receive = receive.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fixture-plugin-token");
                let value = match method.as_str() {
                    "handshake" => json!({"version":1,"state":"awaiting_confirmation","connection_id":"c","subject":"user",
                        "plugin":{"id":PLUGIN,"library":"ioc_fixture","tools":[{"name":"ping","description":"Ping fixture","inputSchema":{"type":"object"}}],"pages":[]}}),
                    "confirm" => json!({"state":"ready"}),
                    "call" => {
                        assert_eq!(headers["x-plugin-connection"], "c");
                        receive.lock().unwrap().push(body["payload"]["args"].clone());
                        json!({"value":{"content":"remote tool ran"}})
                    }
                    _ => panic!("unexpected remote plugin request: {method}"),
                };
                Json(json!({"type":"server-response","rpcId":body["rpcId"],"result":{"ok":true,"value":value}}))
            }
        }
    ))).await;
    let (first, first_requests) = mock_upstream("rag-default", FIRST_SECRET, true).await;
    let (second, second_requests) = mock_upstream("rag-next", SECOND_SECRET, false).await;
    let first_url = format!("{}/v1", first.base);
    let second_url = format!("{}/v1", second.base);
    let first_provider = json!({"url":first_url,"api_key":FIRST_SECRET,"model":"rag-default"});
    let second_provider = json!({"url":second_url,"api_key":SECOND_SECRET,"model":"rag-next","provider_id":"replacement"});

    let (runtime, catalog) = skill_packages::test_runtime();
    catalog.packages.use_test_settings_path(package_path.clone());
    let config = Arc::new(RwLock::new(load_config(&config_path).unwrap()));
    let mut agent = agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
    agent.config = Some(config.clone());
    agent.config_path = Some(config_path.clone());
    agent.tool_catalog = Some(catalog.clone());
    let app = remote_package_routes()
        .route("/agent/settings/data", get(get_agent_settings).put(put_agent_settings))
        .route("/agent/settings", get(crate::server::agent_web::settings_page))
        .route("/agent/models", get(get_model_catalog))
        .with_state(AiProxyState {
            config, config_path: config_path.clone(), settings_write: Arc::default(),
            client: Client::new(), local_api_client: Client::new(), runtime: runtime.clone(),
        })
        .merge(Router::new().route("/agent/skill-packages/{id}/session", post(crate::server::plugin_sessions::request))
            .with_state(agent.clone()));
    let host = LocalServer::start(app).await;
    let client = Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap();
    let host_url = |path: &str| format!("{}{path}", host.base);
    let install = host_url("/agent/skill-packages/remote");
    let replace = host_url(&format!("/agent/skill-packages/remote/{PLUGIN}/model-provider"));
    let uninstall = host_url(&format!("/agent/skill-packages/{PLUGIN}"));
    let session_url = host_url(&format!("/agent/skill-packages/{PLUGIN}/session"));
    let settings = host_url("/agent/settings/data");
    let registration = json!({"url":format!("{}/remote", remote.base),"token":"fixture-plugin-token","model_provider":first_provider});

    response_json(client.put(&replace).json(&first_provider), StatusCode::BAD_REQUEST).await;
    let mut broken = registration.clone();
    broken["url"] = json!(format!("{}/not-found", remote.base));
    response_json(client.post(&install).json(&broken), StatusCode::BAD_REQUEST).await;
    assert!(!package_path.exists());
    assert!(external_models::registry().for_plugin(PLUGIN).is_none());
    let installed = response_json(client.post(&install).json(&registration), StatusCode::OK).await;
    let entry = installed["packages"].as_array().unwrap().iter().find(|p| p["id"] == PLUGIN).unwrap();
    let scope = entry["scope"].clone();
    let saved_package_settings = std::fs::read_to_string(&package_path).unwrap();
    let saved_packages: Value = serde_json::from_str(&saved_package_settings).unwrap();
    assert_eq!(saved_packages["remotes"][PLUGIN], json!({"url":registration["url"],"token":registration["token"]}));

    let assert_private = |text: &str| {
        for private in [FIRST_SECRET, SECOND_SECRET, &first_url, &second_url, "model_provider"] {
            assert!(!text.contains(private), "external provider configuration leaked");
        }
    };
    assert_private(&installed.to_string());
    assert_private(&saved_package_settings);
    let main = response_json(client.get(host_url("/agent/models")), StatusCode::OK).await;
    assert!(main["models"].as_array().unwrap().iter().all(|m| m["id"] != "rag-default"));
    let view = response_json(client.get(&settings), StatusCode::OK).await;
    assert_private(&view.to_string());
    // The HTML shell can be absent in a Rust-only checkout; its JSON API above must succeed.
    let html = client.get(host_url("/agent/settings")).send().await.unwrap();
    assert!(matches!(html.status(), StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE));
    assert_private(&html.text().await.unwrap());
    let saved = response_json(client.put(&settings).json(&json!({"revision":view["revision"],"providers":view["providers"],
        "orchestrator_provider":"user","orchestrator_model":"user-model"})), StatusCode::OK).await;
    assert_private(&saved.to_string());
    assert_private(&std::fs::read_to_string(&config_path).unwrap());

    let models = |expected| {
        let request = client.post(&session_url).json(&json!({"method":"models","package_scope":scope}));
        async move {
            let value = response_json(request, StatusCode::OK).await;
            assert_eq!(value["models"].as_array().unwrap().len(), 1);
            assert_eq!(value["models"][0]["id"], expected);
            value
        }
    };
    assert_private(&models("rag-default").await.to_string());
    let draft = response_json(client.post(&session_url).json(&json!({"method":"create","package_scope":scope})), StatusCode::OK).await;
    let task = draft["task_id"].as_str().unwrap();
    let prompt = |text| client.post(&session_url).json(&json!({"method":"prompt","task_id":task,"package_scope":scope,"prompt":text}));
    let wait_for_turn = || async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !agent.cancellations.lock().await.contains_key(task) { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        let status = response_json(client.post(&session_url).json(&json!({"method":"status","task_id":task,"package_scope":scope})), StatusCode::OK).await;
        assert_eq!(status["status"], "completed", "{status}");
    };
    response_json(prompt("Run the fixture tool"), StatusCode::ACCEPTED).await;
    wait_for_turn().await;
    let received = calls.lock().unwrap().clone();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["tool"], "ping");
    assert_eq!(received[0]["arguments"], json!({"model":"rag-default","provider":"ext-ioc-remote-fixture"}));
    let original = first_requests.lock().unwrap().clone();
    assert_eq!(original.len(), 2);
    assert_eq!(original[0]["messages"][0], original[1]["messages"][0]);

    let updated = response_json(client.request(Method::PUT, &replace).json(&second_provider), StatusCode::OK).await;
    assert_eq!(updated, json!({"plugin_id":PLUGIN,"provider":"replacement","model":"rag-next","protocol":"openai-completions","url":second_url}));
    let mut invalid = first_provider.clone();
    invalid["protocol"] = json!("unsupported");
    response_json(client.put(&replace).json(&invalid), StatusCode::BAD_REQUEST).await;
    models("rag-next").await;
    response_json(prompt("Answer after the hot swap"), StatusCode::ACCEPTED).await;
    wait_for_turn().await;
    assert_eq!(first_requests.lock().unwrap().len(), 2);
    let next = second_requests.lock().unwrap().clone();
    assert_eq!(next.len(), 1);
    let current_system = next[0]["messages"][0]["content"].as_str().unwrap();
    assert!(current_system.contains("Current runtime model: rag-next (provider: replacement)"));
    assert!(!current_system.contains("rag-default"));
    assert_eq!(std::fs::read_to_string(&package_path).unwrap(), saved_package_settings);
    let logs = std::fs::read_dir(root.join("logs")).unwrap().filter_map(|entry| {
        let path = entry.unwrap().path();
        path.file_name().unwrap().to_string_lossy().starts_with("model-requests-")
            .then(|| std::fs::read_to_string(path).unwrap())
    }).collect::<Vec<_>>().join("\n");
    assert!(logs.contains("replacement"));
    assert_private(&logs);
    assert!(!logs.contains(&first.base) && !logs.contains(&second.base));

    assert_eq!(response_json(client.delete(&replace), StatusCode::OK).await, updated);
    assert!(external_models::registry().for_plugin(PLUGIN).is_none());
    models("user-model").await;
    response_json(client.put(&replace).json(&first_provider), StatusCode::OK).await;
    response_json(client.delete(&uninstall), StatusCode::OK).await;
    assert!(external_models::registry().for_plugin(PLUGIN).is_none());
    assert!(!catalog.packages.is_remote(PLUGIN));
    assert_private(&std::fs::read_to_string(&package_path).unwrap());
    response_json(client.put(&replace).json(&first_provider), StatusCode::BAD_REQUEST).await;
    runtime.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
