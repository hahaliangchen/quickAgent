async fn get_model_catalog(State(state): State<AiProxyState>) -> Response {
    let catalog = crate::model::model_catalog::ModelCatalog {
        config: state.config,
        path: Some(state.config_path),
    };
    match catalog.list().await {
        Ok(value) => Json(value).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not load model catalog"})),
        )
            .into_response(),
    }
}

fn trusted_settings_request(headers: &HeaderMap) -> bool {
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
        return false;
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    reqwest::Url::parse(origin).ok().is_some_and(|url| {
        url.scheme() == "http"
            && url
                .host_str()
                .is_some_and(|name| matches!(name, "127.0.0.1" | "localhost"))
            && url.host_str().is_some_and(|name| {
                host.eq_ignore_ascii_case(&format!(
                    "{}:{}",
                    name,
                    url.port_or_known_default().unwrap_or(80)
                ))
            })
    })
}

// The desktop server is a privileged local API; a web page from another
// origin must not be able to use its permissive web-mode CORS configuration.
async fn desktop_origin_guard(
    State(expected): State<String>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let host = request.headers().get(header::HOST).and_then(|v| v.to_str().ok());
    if !trusted_settings_request(request.headers()) || Some(expected.as_str()) != host {
        return (StatusCode::FORBIDDEN, "untrusted desktop request").into_response();
    }
    next.run(request).await
}

fn shell_response(result: anyhow::Result<Value>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, Json(json!({"error":format!("{error:#}")}))).into_response(),
    }
}

async fn get_shell_environment(State(state): State<AiProxyState>, headers: HeaderMap) -> Response {
    if !trusted_settings_request(&headers) { return StatusCode::FORBIDDEN.into_response(); }
    // Only the code-session UI or an explicit Settings redetect requests this.
    let result = tokio::task::spawn_blocking(move || {
        let settings = crate::tools::shell_environment::read_settings(&state.config_path)?;
        Ok(crate::tools::shell_environment::detect_description(settings.active_path(), false))
    }).await;
    shell_response(result.unwrap_or_else(|error| Err(error.into())))
}

#[derive(Deserialize)]
struct DetectShellInput { path: Option<String> }

async fn detect_shell_environment(State(state): State<AiProxyState>, headers: HeaderMap, Json(input): Json<DetectShellInput>) -> Response {
    if !trusted_settings_request(&headers) { return StatusCode::FORBIDDEN.into_response(); }
    let _guard = state.settings_write.lock().await;
    let config_path = state.config_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        if let Some(path) = input.path {
            let selected = if cfg!(windows) { crate::tools::shell_environment::validate_pwsh_path(&path)? } else { crate::tools::shell_environment::validate_bash_path(&path)? };
            let generation_path = generation_settings_path(&config_path);
            let revision = disk_revision(&config_path)?;
            let mut generation = read_yaml(&generation_path, json!({}))?;
            generation.as_object_mut().context("generation settings must be an object")?
                .insert(if cfg!(windows) { "pwsh_path" } else { "bash_path" }.into(), json!(selected));
            anyhow::ensure!(disk_revision(&config_path)? == revision, "settings changed while saving the shell path; retry");
            write_yaml_atomic(&generation_path, &generation)?;
        }
        let config = load_config(&config_path)?;
        let status = crate::tools::shell_environment::detect_description(if cfg!(windows) { config.pwsh_path.as_deref() } else { config.bash_path.as_deref() }, true);
        Ok::<_, anyhow::Error>((config, status))
    }).await;
    match result {
        Ok(Ok((config, status))) => { *state.config.write().await = config; Json(status).into_response() }
        Ok(Err(error)) => shell_response(Err(error)),
        Err(error) => shell_response(Err(error.into())),
    }
}

async fn get_shell_install(headers: HeaderMap) -> Response {
    if !trusted_settings_request(&headers) { return StatusCode::FORBIDDEN.into_response(); }
    Json(crate::tools::shell_environment::installation_status()).into_response()
}

async fn start_shell_install(State(state): State<AiProxyState>, headers: HeaderMap) -> Response {
    if !trusted_settings_request(&headers) { return StatusCode::FORBIDDEN.into_response(); }
    shell_response(crate::tools::shell_environment::start_installation(&state.config_path))
}

async fn cancel_shell_install(headers: HeaderMap) -> Response {
    if !trusted_settings_request(&headers) { return StatusCode::FORBIDDEN.into_response(); }
    Json(crate::tools::shell_environment::cancel_installation()).into_response()
}

async fn open_shell_store(headers: HeaderMap) -> Response {
    if !trusted_settings_request(&headers) { return StatusCode::FORBIDDEN.into_response(); }
    shell_response(crate::tools::shell_environment::open_store().await)
}

async fn get_agent_settings(State(state): State<AiProxyState>, headers: HeaderMap) -> Response {
    if !trusted_settings_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"settings are only available to the local page"})),
        )
            .into_response();
    }
    let _guard = state.settings_write.lock().await;
    let path = state.config_path.clone();
    let loaded = tokio::task::spawn_blocking(move || -> anyhow::Result<(AiProxyConfig, String)> {
        Ok((load_config(&path)?, disk_revision(&path)?))
    })
    .await;
    match loaded {
        Ok(Ok((config, revision))) => {
            let view = settings_view(&config, revision);
            *state.config.write().await = config;
            Json(view).into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not load agent settings"})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ProviderEdit {
    #[serde(default)]
    prefix_system_messages: Option<bool>,
    id: String,
    url: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default = "default_api_type")]
    api_type: String,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    model_capabilities: Option<HashMap<String, ModelCapabilities>>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    clear_api_key: bool,
}

#[derive(Deserialize)]
struct SettingsEdit {
    revision: String,
    /// Omitted preserves the selection; an empty string disables web search.
    #[serde(default)]
    web_search_provider: Option<String>,
    #[serde(default)]
    bash_path: Option<String>,
    #[serde(default)]
    pwsh_path: Option<String>,
    providers: Vec<ProviderEdit>,
    orchestrator_provider: Option<String>,
    orchestrator_model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    fast_mode: Option<bool>,
    #[serde(default)]
    visual_tool: Option<crate::model::model_settings::ModelSettings>,
    #[serde(default)]
    image_generation: Option<crate::model::model_settings::ModelSettings>,
    #[serde(default)]
    delegate_agent: Option<crate::model::model_settings::ModelSettings>,
    #[serde(default)]
    plugin_models: Option<HashMap<String, Vec<crate::model::model_catalog::ModelRef>>>,
    #[serde(default)]
    plugin_generation: Option<HashMap<String, crate::model::model_settings::PluginGeneration>>,
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn verified_image_support(capability: &ModelCapabilities, url: &str, model: &str) -> bool {
    capability.image_probe.as_ref().is_some_and(|probe| {
        probe["status"] == "verified"
            && probe["capability"] == "supported"
            && probe["request"]["contains_image_content"] == true
            && probe["route_fingerprint"] == crate::media::visual_probe::route_fingerprint(url, model)
    })
}

fn image_probe_route(
    config: &AiProxyConfig,
    provider: &str,
    model: &str,
) -> anyhow::Result<crate::media::visual_artifacts::VisualRoute> {
    let route = config
        .providers
        .get(provider)
        .ok_or_else(|| anyhow::anyhow!("provider is not configured; save settings first"))?;
    anyhow::ensure!(
        route.api_type == "openai-completions",
        "image probe currently requires an OpenAI Chat Completions route"
    );
    let model = route
        .model_map
        .get(model)
        .ok_or_else(|| anyhow::anyhow!("model is not configured; save settings first"))?;
    Ok(crate::media::visual_artifacts::VisualRoute {
        provider: provider.into(),
        model: model.clone(),
        url: route.url.clone(),
        api_key: route.api_key.clone(),
    })
}

fn persist_image_probe(
    path: &Path,
    revision: &str,
    provider: &str,
    model: &str,
    report: &Value,
) -> anyhow::Result<(AiProxyConfig, String)> {
    anyhow::ensure!(
        disk_revision(path)? == revision,
        "settings changed during image verification; run it again on the saved route"
    );
    let mut config = load_config(path)?;
    let route = image_probe_route(&config, provider, model)?;
    anyhow::ensure!(
        report["route_fingerprint"]
            == crate::media::visual_probe::route_fingerprint(&route.url, &route.model),
        "image probe route changed"
    );
    let capability = config
        .model_capabilities
        .entry(provider.into())
        .or_default()
        .entry(model.into())
        .or_default();
    capability.image_input = serde_json::from_value(report["capability"].clone())?;
    capability.image_probe = Some(report.clone());
    let generation_path = generation_settings_path(path);
    let mut generation = read_yaml(&generation_path, json!({}))?;
    generation["model_capabilities"] = serde_json::to_value(&config.model_capabilities)?;
    write_yaml_atomic(&generation_path, &generation)?;
    Ok((config, disk_revision(path)?))
}

#[derive(Deserialize)]
struct ImageProbeRequest {
    revision: String,
    provider: String,
    model: String,
}

async fn probe_agent_image_input(
    State(state): State<AiProxyState>,
    headers: HeaderMap,
    Json(input): Json<ImageProbeRequest>,
) -> Response {
    if !trusted_settings_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"settings are only available to the local page"})),
        )
            .into_response();
    }
    let result = async {
        let config = load_config(&state.config_path)?;
        anyhow::ensure!(
            disk_revision(&state.config_path)? == input.revision,
            "settings changed; reload before image verification"
        );
        let route = image_probe_route(&config, &input.provider, &input.model)?;
        let report = crate::media::visual_probe::probe(&state.client, &route).await?;
        let _guard = state.settings_write.lock().await;
        let (config, revision) = persist_image_probe(
            &state.config_path,
            &input.revision,
            &input.provider,
            &input.model,
            &report,
        )?;
        let view = settings_view(&config, revision);
        *state.config.write().await = config;
        Ok::<_, anyhow::Error>(json!({"probe":report,"settings":view}))
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}

pub(crate) async fn probe_default_image_input(path: &Path) -> anyhow::Result<Value> {
    let revision = disk_revision(path)?;
    let config = load_config(path)?;
    let provider = config
        .orchestrator_provider
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("main provider is not configured"))?;
    let model = config
        .orchestrator_model
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("main model is not configured"))?;
    let route = image_probe_route(&config, provider, model)?;
    let report = crate::media::visual_probe::probe(&Client::new(), &route).await?;
    // Store evidence without touching profile routing, credentials, or fallback settings.
    persist_image_probe(path, &revision, provider, model, &report)?;
    Ok(report)
}

fn save_settings(
    config_path: &Path,
    request: SettingsEdit,
) -> anyhow::Result<Result<(AiProxyConfig, String), String>> {
    if disk_revision(config_path)? != request.revision {
        return Ok(Err(
            "settings changed since this page loaded; reload before saving".into(),
        ));
    }
    let mut config = load_config(config_path)?;
    if let Some(path) = request.bash_path.as_deref() {
        config.bash_path = crate::tools::shell_environment::validate_bash_path(path)?;
    }
    if let Some(path) = request.pwsh_path.as_deref() {
        // Saving another settings tab must not start a PowerShell probe when
        // the user has not changed the executable selection.
        if config.pwsh_path.as_deref() != Some(Path::new(path.trim())) || path.trim().is_empty() {
            config.pwsh_path = crate::tools::shell_environment::validate_pwsh_path(path)?;
        }
    }
    let mut providers = HashMap::new();
    let mut model_capabilities = HashMap::new();
    let mut key_actions: HashMap<String, Option<String>> = HashMap::new();
    for draft in request.providers {
        let id = draft.id.trim().to_owned();
        anyhow::ensure!(
            !id.is_empty()
                && id.len() <= 64
                && id.as_bytes()[0].is_ascii_lowercase()
                && id.as_bytes()[id.len() - 1].is_ascii_alphanumeric()
                && !id.contains("--")
                && id
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
            "provider ID must be lowercase words separated by single hyphens (up to 64 characters)"
        );
        anyhow::ensure!(!providers.contains_key(&id), "duplicate provider ID: {id}");
        let url = reqwest::Url::parse(draft.url.trim())?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "provider {id} requires an HTTP(S) URL"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "provider {id} URL must not include credentials, query, or fragment"
        );
        anyhow::ensure!(
            matches!(
                draft.api_type.as_str(),
                "openai-completions" | "openai-responses" | "anthropic-messages"
            ),
            "provider {id} has unsupported protocol"
        );
        anyhow::ensure!(
            !draft.models.is_empty() && draft.models.iter().all(|model| !model.trim().is_empty()),
            "provider {id} needs at least one model"
        );
        anyhow::ensure!(
            draft
                .models
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == draft.models.len(),
            "provider {id} has duplicate model IDs"
        );
        let configured = draft.model_capabilities.unwrap_or_else(|| {
            config
                .model_capabilities
                .get(&id)
                .cloned()
                .unwrap_or_default()
        });
        let mut capabilities = HashMap::new();
        for model in &draft.models {
            let mut capability = configured
                .get(model)
                .cloned()
                .unwrap_or_else(|| built_in_capabilities(model));
            // A settings payload cannot manufacture successful probe evidence.
            capability.image_probe = config
                .model_capabilities
                .get(&id)
                .and_then(|models| models.get(model))
                .and_then(|previous| previous.image_probe.clone());
            if capability.image_probe.as_ref().is_some_and(|probe| {
                draft.api_type != "openai-completions"
                    || probe["route_fingerprint"]
                        != crate::media::visual_probe::route_fingerprint(url.as_str(), model)
            }) {
                capability.image_input = crate::media::visual_artifacts::ImageCapability::Unknown;
            }
            anyhow::ensure!(
                capability.image_input != crate::media::visual_artifacts::ImageCapability::Supported
                    || (draft.api_type == "openai-completions"
                        && verified_image_support(&capability, url.as_str(), model)),
                "provider {id} model {model}: verify image input on the saved route before marking it supported"
            );
            anyhow::ensure!(
                capability
                    .reasoning_efforts
                    .iter()
                    .all(|effort| crate::model::model_settings::canonical_effort(effort)
                        .is_some_and(|known| known == effort.as_str())),
                "provider {id} model {model} has an unsupported reasoning effort"
            );
            anyhow::ensure!(
                capability
                    .reasoning_efforts
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == capability.reasoning_efforts.len(),
                "provider {id} model {model} has duplicate reasoning efforts"
            );
            anyhow::ensure!(
                capability
                    .default_effort
                    .as_ref()
                    .is_none_or(|effort| capability.reasoning_efforts.contains(effort)),
                "provider {id} model {model} default effort is not in its supported levels"
            );
            capabilities.insert(model.clone(), capability);
        }
        model_capabilities.insert(id.clone(), capabilities);
        let previous = config.providers.get(&id);
        let requested_key = draft.api_key.filter(|v| !v.is_empty());
        if draft.clear_api_key {
            key_actions.insert(id.clone(), None);
        } else if let Some(value) = &requested_key {
            key_actions.insert(id.clone(), Some(value.clone()));
        }
        let key = if draft.clear_api_key {
            String::new()
        } else if let Some(value) = requested_key {
            anyhow::ensure!(value.len() <= 8192, "provider {id} has an invalid API key");
            value
        } else {
            previous.map(|p| p.api_key.clone()).unwrap_or_default()
        };
        let mut provider = previous.cloned().unwrap_or(ProviderConfig {
            url: String::new(),
            api_key: String::new(),
            api_key_ref: None,
            model_map: HashMap::new(),
            api_type: default_api_type(),
            max_output_tokens: HashMap::new(),
            model_metadata: HashMap::new(),
            extra: HashMap::new(),
        });
        provider.url = url.as_str().trim_end_matches('/').to_owned();
        provider.api_key = key;
        provider.model_map = draft
            .models
            .into_iter()
            .map(|model| (model.clone(), model))
            .collect();
        provider.api_type = draft.api_type;
        if let Some(prefix) = draft.prefix_system_messages {
            provider
                .extra
                .insert("prefixSystemMessages".into(), json!(prefix));
        }
        match nonempty(draft.display_name) {
            Some(name) => {
                provider.extra.insert("displayName".into(), json!(name));
            }
            None => {
                provider.extra.remove("displayName");
            }
        }
        providers.insert(id, provider);
    }
    config.providers = providers;
    if let Some(provider) = request.web_search_provider {
        config.web_search_provider = nonempty(Some(provider));
    }
    if config.web_search_provider.is_some() {
        // Search credentials are needed when using web, not when saving an
        // unrelated local model. Still reject an unsupported search endpoint.
        web_provider_route(&config)?;
    }
    config.model_capabilities = model_capabilities;
    config.orchestrator_provider = nonempty(request.orchestrator_provider);
    config.orchestrator_model = nonempty(request.orchestrator_model);
    for (name, edit, target) in [
        ("visual_tool", request.visual_tool, &mut config.visual_tool),
        (
            "image_generation",
            request.image_generation,
            &mut config.image_generation,
        ),
        ("delegate_agent", request.delegate_agent, &mut config.delegate_agent),
    ] {
        if let Some(mut edit) = edit {
            edit.normalize()?;
            *target = edit;
        }
        if target.provider.is_some() || target.model.is_some() {
            let provider = target
                .provider
                .as_ref()
                .or(config.orchestrator_provider.as_ref())
                .context("choose a provider for the dedicated task")?;
            let model = target
                .model
                .as_ref()
                .or(config.orchestrator_model.as_ref())
                .context("choose a model for the dedicated task")?;
            let route = config
                .providers
                .get(provider)
                .with_context(|| format!("configured {name} provider is unavailable"))?;
            // A dedicated route runs on the same wire as the conversation, so it
            // may be any protocol the agent speaks. Image generation is the one
            // caller that needs an OpenAI image endpoint rather than the agent loop.
            let compatible = if name == "image_generation" {
                matches!(route.api_type.as_str(), "openai-completions" | "openai-responses")
            } else {
                agent_api(&route.api_type)
            };
            anyhow::ensure!(
                compatible,
                "configured {name} provider is incompatible"
            );
            anyhow::ensure!(
                route.model_map.contains_key(model),
                "dedicated task model {model} is not in provider {provider}'s catalog"
            );
            if let Some(cap) = config
                .model_capabilities
                .get(provider)
                .and_then(|m| m.get(model))
            {
                anyhow::ensure!(
                    cap.reasoning_efforts.is_empty()
                        || target
                            .reasoning_effort
                            .as_ref()
                            .is_none_or(|e| cap.reasoning_efforts.contains(e)),
                    "dedicated task model does not support this reasoning effort"
                );
            }
        }
    }
    config.default_provider = config.orchestrator_provider.clone();
    if let Some(policies) = request.plugin_models {
        config.plugin_models = policies;
    }
    if let Some(mut policies) = request.plugin_generation {
        for (id, policy) in &mut policies {
            anyhow::ensure!(
                !id.trim().is_empty(),
                "plugin generation policy requires a plugin ID"
            );
            policy.normalize()?;
        }
        config.plugin_generation = policies;
    }
    for (plugin, models) in &config.plugin_models {
        anyhow::ensure!(
            !plugin.is_empty(),
            "plugin model policy requires a plugin ID"
        );
        let mut unique = std::collections::HashSet::new();
        for model in models {
            anyhow::ensure!(
                unique.insert(model.clone()),
                "duplicate plugin model reference"
            );
            let route = config
                .providers
                .get(&model.provider)
                .filter(|p| p.model_map.contains_key(&model.model))
                .with_context(|| {
                    format!(
                        "插件 {plugin} 引用的模型 {}/{} 未在可用模型目录中登记",
                        model.provider, model.model
                    )
                })?;
            anyhow::ensure!(
                agent_api(&route.api_type),
                "插件 {plugin} 引用的模型 {}/{} 使用了不支持的协议 {}",
                model.provider, model.model, route.api_type
            );
        }
    }
    if let Some(effort) = request.reasoning_effort {
        let effort = effort.trim();
        anyhow::ensure!(
            effort.is_empty()
                || crate::model::model_settings::canonical_effort(effort)
                    .is_some_and(|known| known == effort),
            "unsupported reasoning effort"
        );
        config.reasoning_effort = (!effort.is_empty()).then(|| effort.to_owned());
    }
    if let Some(fast_mode) = request.fast_mode {
        config.fast_mode = fast_mode;
    }
    let selected_capability = config
        .orchestrator_provider
        .as_ref()
        .and_then(|provider| config.model_capabilities.get(provider))
        .and_then(|models| {
            config
                .orchestrator_model
                .as_ref()
                .and_then(|model| models.get(model))
        });
    if let Some(capability) = selected_capability {
        anyhow::ensure!(
            config
                .reasoning_effort
                .as_ref()
                .is_none_or(|effort| capability.reasoning_efforts.contains(effort)),
            "selected model does not support this reasoning effort"
        );
        anyhow::ensure!(
            !config.fast_mode || capability.fast_mode,
            "selected model does not support fast mode"
        );
    } else {
        config.reasoning_effort = None;
        config.fast_mode = false;
    }
    for name in [&config.orchestrator_provider].into_iter().flatten() {
        anyhow::ensure!(
            config.providers.contains_key(name),
            "selected provider {name} does not exist"
        );
    }
    anyhow::ensure!(
        config.orchestrator_provider.is_some() == config.orchestrator_model.is_some(),
        "choose both a main-agent provider and model"
    );
    for (provider, model) in [(&config.orchestrator_provider, &config.orchestrator_model)] {
        if let (Some(provider), Some(model)) = (provider, model) {
            anyhow::ensure!(
                config.providers[provider].model_map.contains_key(model),
                "model {model} is not in provider {provider}'s catalog"
            );
        }
    }
    // The agent speaks three protocols (see `agent_api`), so the saved default
    // route may be any of them. Requiring Chat Completions here meant a pick on a
    // Responses or Messages provider was refused by the save, which left the next
    // chat on the model that had been stored before it.
    for name in [&config.orchestrator_provider].into_iter().flatten() {
        anyhow::ensure!(
            agent_api(&config.providers[name].api_type),
            "agent route {name} cannot drive the agent: pick a Chat Completions, Responses or Anthropic Messages provider"
        );
    }
    let mut patch = read_yaml(config_path, json!([]))?;
    let original_patch = patch.clone();
    let mut credentials = read_yaml(
        &credentials_path(config_path),
        json!({"version":1,"refs":{},"records":{}}),
    )?;
    anyhow::ensure!(
        patch.is_array(),
        "DSH profile patch must be a YAML sequence"
    );
    anyhow::ensure!(
        credentials.get("version").and_then(Value::as_u64) == Some(1),
        "unsupported credentials version"
    );
    let refs = credentials
        .get_mut("refs")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow::anyhow!("credentials refs must be a mapping"))?;
    let old_providers = plugin_config(&patch, "llm-pi-ai")
        .and_then(|v| v.get("providers"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut dsh_providers = old_providers.clone();
    for id in old_providers.keys() {
        if config.providers.contains_key(id)
            || old_providers[id].get("models").is_none()
            || old_providers[id].get("baseURL").is_none()
        {
            continue;
        }
        dsh_providers.remove(id);
    }
    for (id, provider) in &config.providers {
        let mut entry = old_providers.get(id).cloned().unwrap_or_else(|| json!({}));
        let item = entry
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("provider {id} is not a mapping"))?;
        item.insert("baseURL".into(), json!(provider.url));
        item.insert("api".into(), json!(provider.api_type));
        item.remove("prefixSystemMessages");
        if let Some(name) = provider.extra.get("displayName") {
            item.insert("displayName".into(), name.clone());
        } else {
            item.remove("displayName");
        }
        let mut models = Vec::new();
        for model in provider.model_map.values() {
            let previous = old_providers
                .get(id)
                .and_then(|v| v.get("models"))
                .and_then(Value::as_array)
                .and_then(|a| {
                    a.iter()
                        .find(|v| v.get("id").and_then(Value::as_str) == Some(model))
                });
            models.push(previous.cloned().unwrap_or_else(|| json!({"id":model})));
        }
        models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        models.dedup_by(|a, b| a["id"] == b["id"]);
        item.insert("models".into(), json!(models));
        let reference = provider
            .api_key_ref
            .clone()
            .unwrap_or_else(|| format!("{}_API_KEY", id.to_ascii_uppercase().replace('-', "_")));
        match key_actions.get(id) {
            Some(None) => {
                anyhow::ensure!(
                    std::env::var_os(&reference).is_none(),
                    "{reference} is supplied by the environment and cannot be cleared here"
                );
                item.remove("apiKeyEnv");
                refs.remove(&reference);
            }
            Some(Some(key)) => {
                anyhow::ensure!(
                    std::env::var_os(&reference).is_none(),
                    "{reference} is supplied by the environment and cannot be replaced here"
                );
                item.insert("apiKeyEnv".into(), json!(reference));
                refs.insert(reference, json!(key));
            }
            None => {}
        }
        dsh_providers.insert(id.clone(), entry);
    }
    let mut llm = plugin_config(&patch, "llm-pi-ai")
        .cloned()
        .unwrap_or_else(|| json!({}));
    llm.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai config is not a mapping"))?
        .insert("providers".into(), Value::Object(dsh_providers));
    set_plugin_config(&mut patch, "llm-pi-ai", llm)?;
    if let (Some(provider), Some(model)) =
        (&config.orchestrator_provider, &config.orchestrator_model)
    {
        let mut default_model = plugin_config(&patch, "agent-default-model")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let item = default_model
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("agent-default-model config is not a mapping"))?;
        item.insert("provider".into(), json!(provider));
        item.insert("model".into(), json!(model));
        set_plugin_config(&mut patch, "agent-default-model", default_model)?;
    } else if let Some(entries) = patch.as_array_mut() {
        entries
            .retain(|entry| entry.get("id").and_then(Value::as_str) != Some("agent-default-model"));
    }
    let secret_path = credentials_path(config_path);
    let generation_path = generation_settings_path(config_path);
    let original_generation = read_yaml(&generation_path, json!({}))?;
    let mut generation_settings = original_generation.clone();
    let current_generation = json!({
        "web_search_provider": config.web_search_provider,
        "bash_path": config.bash_path,
        "pwsh_path": config.pwsh_path,
        "provider_compatibility":config.providers.iter().map(|(id,p)|(id.clone(),json!({"prefix_system_messages":p.extra.get("prefixSystemMessages").and_then(Value::as_bool).unwrap_or(false)}))).collect::<HashMap<_,_>>(),
        "reasoning_effort": config.reasoning_effort,
        "fast_mode": config.fast_mode,
        "model_capabilities": config.model_capabilities,
                "visual_tool":config.visual_tool,"image_generation":config.image_generation,"delegate_agent":config.delegate_agent,
        "plugin_models":config.plugin_models,"plugin_generation":config.plugin_generation,
    });
    generation_settings
        .as_object_mut()
        .context("generation settings must be an object")?
        .extend(current_generation.as_object().unwrap().clone());
    if disk_revision(config_path)? != request.revision {
        return Ok(Err(
            "settings changed while saving; reload before retrying".into()
        ));
    }
    if let Some(parent) = secret_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !key_actions.is_empty() {
        write_yaml_atomic(&secret_path, &credentials)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if secret_path.exists() {
            std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    if patch != original_patch {
        write_yaml_atomic(config_path, &patch)?;
    }
    if generation_settings != original_generation {
        write_yaml_atomic(&generation_path, &generation_settings)?;
    }
    let revision = disk_revision(config_path)?;
    Ok(Ok((config, revision)))
}

async fn put_agent_settings(
    State(state): State<AiProxyState>,
    headers: HeaderMap,
    Json(request): Json<SettingsEdit>,
) -> Response {
    if !trusted_settings_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"settings are only available to the local page"})),
        )
            .into_response();
    }
    let _guard = state.settings_write.lock().await;
    let path = state.config_path.clone();
    let result = tokio::task::spawn_blocking(move || save_settings(&path, request)).await;
    match result {
        Ok(Ok(Ok((config, revision)))) => {
            let view = settings_view(&config, revision);
            *state.config.write().await = config;
            Json(view).into_response()
        }
        Ok(Ok(Err(message))) => {
            (StatusCode::CONFLICT, Json(json!({"error":message}))).into_response()
        }
        Ok(Err(error)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"could not save agent settings"})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct DiscoverModelsRequest {
    id: String,
    url: String,
    #[serde(default)]
    api_key: Option<String>,
}

async fn discover_agent_models(
    State(state): State<AiProxyState>,
    headers: HeaderMap,
    Json(request): Json<DiscoverModelsRequest>,
) -> Response {
    if !trusted_settings_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"settings are only available to the local page"})),
        )
            .into_response();
    }
    let url = match reqwest::Url::parse(request.url.trim()) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => url,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"enter a valid HTTP(S) provider URL"})),
            )
                .into_response();
        }
    };
    let saved_key = {
        let config = state.config.read().await;
        config
            .providers
            .get(&request.id)
            .filter(|provider| {
                provider.url.trim_end_matches('/') == url.as_str().trim_end_matches('/')
            })
            .map(|provider| provider.api_key.clone())
            .unwrap_or_default()
    };
    let key = request
        .api_key
        .filter(|key| !key.is_empty())
        .unwrap_or(saved_key);
    let endpoint = format!("{}/models", url.as_str().trim_end_matches('/'));
    let mut call = state.client.get(endpoint);
    if !key.is_empty() {
        call = call.bearer_auth(key);
    }
    let response = match tokio::time::timeout(std::time::Duration::from_secs(20), call.send()).await
    {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":"could not reach provider model catalog"})),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"error":"provider model catalog timed out"})),
            )
                .into_response();
        }
    };
    if !response.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":format!("provider model catalog returned {}", response.status())})),
        )
            .into_response();
    }
    let value: Value = match response.json().await {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":"provider returned an invalid model catalog"})),
            )
                .into_response();
        }
    };
    let mut models: Vec<String> = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .or_else(|| item.as_str())
        })
        .map(str::to_owned)
        .take(500)
        .collect();
    models.sort();
    models.dedup();
    Json(json!({"models":models})).into_response()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

