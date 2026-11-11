//! A plugin can supply immutable program/module resources. The host only loads,
//! hashes, compiles and links them; it never chooses a business workflow.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resource { path: String, sha256: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    source: Resource,
    #[serde(default)]
    modules: BTreeMap<String, Resource>,
}
#[derive(Debug)]
pub(super) struct Loaded {
    pub origin: Value,
    pub modules: BTreeMap<String, String>,
}
async fn read(s: &Session<'_>, resource: &Resource) -> Result<String> {
    let bytes = crate::plugin_io::program_resource(s.state, s.task, &resource.path).await?;
    ensure!(resource.sha256.len() == 64 && crate::visual_artifacts::hash(&bytes) == resource.sha256,
        "PLUGIN_PROGRAM_HASH_MISMATCH: {} changed; reload its manifest", resource.path);
    Ok(String::from_utf8(bytes).context("plugin program source must be UTF-8")?)
}
pub(super) async fn load(s: &Session<'_>, args: &mut Value, body: bool) -> Result<Option<Loaded>> {
    let Some(path) = args.get("resource") else { return Ok(None) };
    ensure!(!body && s.plugin.is_some(), "plugin bundles require a main program in the owning plugin session");
    ensure!(args.as_object().is_some_and(|args| args.len() == 1),
        "a plugin program resource cannot be combined with source, probes or module revisions");
    let path = path.as_str().context("resource must be a plugin skill path")?.to_owned();
    let raw = crate::plugin_io::program_resource(s.state, s.task, &path).await?;
    let manifest: Manifest = serde_json::from_slice(&raw).context("invalid plugin program bundle")?;
    ensure!(manifest.version == 1 && manifest.modules.len() <= 32, "unsupported plugin program bundle");
    let source = read(s, &manifest.source).await?;
    let mut modules = BTreeMap::new();
    let mut total = source.len();
    for (name, resource) in &manifest.modules {
        let source = read(s, resource).await?;
        total += source.len();
        ensure!(total <= 512 * 1024, "plugin program bundle too large");
        modules.insert(name.clone(), source);
    }
    let origin = json!({"plugin":s.plugin,"resource":path,"manifest_sha256":crate::visual_artifacts::hash(&raw),
        "source_sha256":manifest.source.sha256,
        "modules":manifest.modules.iter().map(|(name, resource)|(name.clone(),json!({"path":resource.path,"sha256":resource.sha256}))).collect::<BTreeMap<_,_>>()});
    *args = json!({"source":source});
    Ok(Some(Loaded { origin, modules }))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Resources(BTreeMap<String, Vec<u8>>);
    impl crate::plugin_io::SessionIo for Resources {
        fn execute<'a>(&'a self, _: &'a AgentServiceState, _: &'a str,
            _: std::sync::Arc<crate::tools::Workspace>, _: &'a str, _: Value)
            -> futures::future::BoxFuture<'a, Result<Value>> {
            Box::pin(async { anyhow::bail!("no business tools in this test") })
        }
        fn program_resource<'a>(&'a self, _: &'a AgentServiceState, _: &'a str, path: &'a str)
            -> futures::future::BoxFuture<'a, Result<Vec<u8>>> {
            Box::pin(async move {self.0.get(path).cloned().context("unknown resource")})
        }
    }
    #[tokio::test]
    async fn plugin_bundle_links_exact_sources_and_rejects_changed_bytes_before_execution() {
        let root = std::env::temp_dir().join(format!("program-bundle-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let state = agent_service::tests::test_state(&root, "127.0.0.1:9".parse().unwrap());
        let conn = agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','','fake','running',0,0)",[]).unwrap();
        let plugin = format!("example-{}", agent_service::uuid_like());
        conn.execute("INSERT INTO plugin_sessions VALUES ('task',?1)", [&plugin]).unwrap();
        let source = "const result=doubleValue(7);console.log(result);\n/** @module Return twice the input. */\nfunction doubleValue(value) {}";
        let implementation = "function doubleValue(value) { return value * 2; }";
        let manifest = json!({"version":1,"source":{"path":"skill:main.js","sha256":crate::visual_artifacts::hash(source.as_bytes())},
            "modules":{"doubleValue":{"path":"skill:double.js","sha256":crate::visual_artifacts::hash(implementation.as_bytes())}}});
        let mut bad = manifest.clone();bad["source"]["sha256"] = json!("0".repeat(64));
        crate::plugin_io::register(&plugin, std::sync::Arc::new(Resources(BTreeMap::from([
            ("skill:bundle.json".into(), manifest.to_string().into_bytes()),
            ("skill:bad.json".into(), bad.to_string().into_bytes()),
            ("skill:main.js".into(), source.as_bytes().to_vec()),
            ("skill:double.js".into(), implementation.as_bytes().to_vec()),
        ]))));
        let cancel = CancellationToken::new();
        let session = Session {agent_id:None,task_node_id:None,plugin:Some(plugin),state:&state,task:"task",model:"fake",turn:1,
            cancel:&cancel,ban_execution:false,scope:"main".into(),parent:None,input_budget:INPUT_BUDGET};
        let mut args = json!({"resource":"skill:bundle.json"});
        let loaded = load(&session, &mut args, false).await.unwrap().unwrap();
        assert_eq!(args["source"],source);
        let mut window = Window::default();
        program_flow::install_scoped(&session,&mut window,&args,false).unwrap();
        modules::provide(&session,&window,&loaded.modules).await.unwrap();
        let declaration = program_flow::function_task(&window.program.as_ref().unwrap().program,"doubleValue",&[json!(7)],&Default::default()).unwrap();
        let contract = modules::contract(&declaration).unwrap();
        assert_eq!(modules::latest(&root,"task",&contract.key).unwrap().unwrap().1,implementation);
        // All-or-nothing bundle validation: an unknown entry cannot install a preceding valid entry.
        let bad_modules = BTreeMap::from([("unknownValue".into(),"function unknownValue(value){return value;}".into())]);
        assert!(modules::provide(&session,&window,&bad_modules).await.is_err());
        let mut bad_args=json!({"resource":"skill:bad.json"});
        assert!(load(&session,&mut bad_args,false).await.unwrap_err().to_string().contains("HASH_MISMATCH"));
        let mut mixed=json!({"resource":"skill:bundle.json","source":"override()"});
        assert!(load(&session,&mut mixed,false).await.is_err());
        assert!(crate::plugin_io::program_resource(&state,"task","/outside.js").await.is_err());
        let _=std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn explicit_plugin_program_runs_before_the_model() {
        use axum::{routing::post, Json, Router};
        let root = std::env::temp_dir().join(format!("fixed-program-{}", agent_service::uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let app = Router::new().route("/v1/chat/completions", post(|| async {
            Json(json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"done"}}]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (runtime, catalog) = crate::skill_packages::test_runtime();
        let mut state = agent_service::tests::test_state(&root, address);
        state.tool_catalog = Some(catalog);
        let conn = agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('task','test','fake','running',0,0)", []).unwrap();
        conn.execute("INSERT INTO plugin_sessions VALUES ('task','code-flow')", []).unwrap();
        let source = "console.log('fixed-program-ran');";
        let manifest = json!({"version":1,"source":{"path":"skill:main.js","sha256":crate::visual_artifacts::hash(source.as_bytes())}});
        crate::plugin_io::register("code-flow", std::sync::Arc::new(Resources(BTreeMap::from([
            ("skill:bundle.json".into(), manifest.to_string().into_bytes()),
            ("skill:main.js".into(), source.as_bytes().to_vec()),
        ]))));
        super::super::run_task_with_resource(state, "task".into(), "fake".into(), "confirmed".into(), 4,
            CancellationToken::new(), 1, false, Some("skill:bundle.json".into())).await.unwrap();
        let start:i64 = conn.query_row("SELECT seq FROM agent_task_events WHERE task_id='task' AND kind='program/start' AND json_extract(data,'$.source_resource.resource')='skill:bundle.json'", [], |r| r.get(0)).unwrap();
        let end:i64 = conn.query_row("SELECT seq FROM agent_task_events WHERE task_id='task' AND kind='program/end' AND data LIKE '%fixed-program-ran%'", [], |r| r.get(0)).unwrap();
        let request:i64 = conn.query_row("SELECT seq FROM agent_task_events WHERE task_id='task' AND kind='debug/context_request' ORDER BY seq LIMIT 1", [], |r| r.get(0)).unwrap();
        assert!(start < end && end < request, "fixed program must execute before a model can choose another workflow");
        server.abort(); runtime.shutdown().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

}
