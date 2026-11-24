use super::{
    impact::{self, ImpactQuery},
    model::*,
    package::graph_view,
    store::Store,
    verify,
};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

#[tokio::test]
#[ignore = "Playwright starts this isolated loopback fixture"]
async fn blueprint_playwright_fixture() -> anyhow::Result<()> {
    let root = Workspace::new();
    let mut source = "// fixture\n".repeat(15);
    source.push_str("export function onSubmit() { return 'blueprint fixture' }\n");
    fs::write(root.0.join("app.ts"), source)?;
    anyhow::ensure!(
        std::env::var_os("GROW_CONFIG_DIR").is_some(),
        "isolated config is required"
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let workspace = std::sync::Arc::new(crate::tools::Workspace::new(root.0.clone())?);
    println!(
        "BLUEPRINT_E2E_READY {}",
        serde_json::json!({"url":format!("http://{}",listener.local_addr()?),"root":root.0})
    );
    crate::server::ai_proxy::run(
        listener,
        &crate::server::ai_proxy::config_dir().join("config.yml"),
        workspace,
    )
    .await
}

struct Workspace(std::path::PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "blueprint-test-{}",
            crate::server::agent_service::uuid_like()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> Store {
        Store::open(&self.0).unwrap()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn update(store: &Store, payload: Value) {
    store
        .upsert_entities(serde_json::from_value(payload).unwrap())
        .unwrap();
}
fn graph(store: &Store, id: &str, payload: Value) -> BlueprintGraph {
    store
        .upsert_graph(id, serde_json::from_value(payload).unwrap())
        .unwrap()
}
fn entity(id: &str, kind: &str) -> Value {
    json!({"id":id,"name":id,"kind":kind,"ports":[{"id":"p","name":"port","direction":"both","kind":"method"}]})
}
fn edge(id: &str, from: &str, to: &str) -> Value {
    json!({"id":id,"source_entity":from,"source_port":"p","target_entity":to,"target_port":"p","relation":"calls","severity":"critical","invariant":"keep guard"})
}
#[test]
fn blueprint_crud_preserves_layout_and_cascades_port_and_entity_removal() {
    let root = Workspace::new();
    let store = root.open();
    update(
        &store,
        json!({"entities":[entity("ui","trigger_ui"),entity("db","store")]}),
    );
    graph(
        &store,
        "save",
        json!({"nodes":[{"entity_id":"ui","x":45,"y":90},{"entity_id":"db"}]}),
    );
    store
        .save_layout(
            "save",
            serde_json::from_value(json!([{"entity_id":"ui","x":250,"y":185}])).unwrap(),
        )
        .unwrap();
    let g = graph(
        &store,
        "save",
        json!({"nodes":[{"entity_id":"ui"},{"entity_id":"db"}],"edges":[edge("a","ui","db")]}),
    );
    assert_eq!((g.nodes[0].x, g.nodes[0].y), (Some(250.), Some(185.)));
    assert!(g.nodes[1].x.unwrap() > g.nodes[0].x.unwrap());
    assert!(
        graph_view(&store, "save", false).unwrap()["graph"]
            .get("nodes")
            .is_none()
    );
    for file in ["manifest.json", "entities.json", "graphs/save.json"] {
        assert!(
            root.0.join(".quickagent/blueprint").join(file).is_file(),
            "{file}"
        );
    }
    let raw = fs::read_to_string(root.0.join(".quickagent/blueprint/manifest.json")).unwrap();
    assert!(raw.starts_with("{\n  \"graphs\""));
    drop(store);
    let store = root.open();
    assert_eq!(store.graph("save").unwrap().unwrap().edges.len(), 1);
    let mut ui = entity("ui", "trigger_ui");
    ui["ports"][0]["id"] = json!("new");
    update(&store, json!({"entities":[ui.clone()]}));
    assert_eq!(store.entities().unwrap().entities[1].ports.len(), 2);
    update(&store, json!({"entities":[ui],"replace_ports":true}));
    assert!(store.graph("save").unwrap().unwrap().edges.is_empty());
    update(&store, json!({"remove_entity_ids":["db"]}));
    assert_eq!(store.graph("save").unwrap().unwrap().nodes.len(), 1);
    store.delete_graph("save").unwrap();
    assert!(store.graph("save").unwrap().is_none());
    assert!(store.manifest().unwrap().unwrap().graphs.is_empty());
}
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
#[test]
fn blueprint_verify_distinguishes_matching_moved_changed_and_missing_anchors() {
    let root = Workspace::new();
    git(&root.0, &["init", "-q"]);
    fs::write(
        root.0.join("src.rs"),
        "fn stable() {}\nfn moved() {}\nfn removed() {}\n",
    )
    .unwrap();
    fs::write(root.0.join("deleted.rs"), "fn gone() {}\n").unwrap();
    git(&root.0, &["add", "."]);
    git(
        &root.0,
        &[
            "-c",
            "user.name=Blueprint",
            "-c",
            "user.email=blueprint@example.test",
            "commit",
            "-qm",
            "baseline",
        ],
    );
    let baseline = git(&root.0, &["rev-parse", "HEAD"]);
    fs::write(
        root.0.join("src.rs"),
        format!("fn stable() {{}}\n{}fn moved() {{}}\n", "\n".repeat(11)),
    )
    .unwrap();
    git(&root.0, &["add", "src.rs"]);
    git(
        &root.0,
        &[
            "-c",
            "user.name=Blueprint",
            "-c",
            "user.email=blueprint@example.test",
            "commit",
            "-qm",
            "move symbol",
        ],
    );
    fs::remove_file(root.0.join("deleted.rs")).unwrap();
    let store = root.open();
    let ports = [("ok",Some("stable"),1),("moved",Some("moved"),2),("range",None,1),("removed",Some("removed"),3)]
        .map(|(id,symbol,line)| json!({"id":id,"name":id,"direction":"both","kind":"method","symbol":symbol,"line_start":line}));
    update(
        &store,
        json!({"last_synced_commit":baseline,"entities":[{"id":"source","name":"Source","kind":"service","file_path":"src.rs","ports":ports},{"id":"gone","name":"Gone","kind":"service","file_path":"deleted.rs","ports":entity("gone","service")["ports"]}]}),
    );
    graph(&store, "save", json!({"nodes":[{"entity_id":"source"}]}));
    let manifest = store.manifest().unwrap().unwrap();
    let report = verify::verify(
        &store,
        Some(&manifest),
        &store.entities().unwrap().entities,
        &store.all_graphs(&manifest).unwrap(),
    );
    assert_eq!(
        (report.ok_ports, report.drifted_ports, report.missing_ports),
        (1, 2, 2)
    );
    assert_eq!(
        report.entities["source"].ports["moved"].suggested_line,
        Some(13)
    );
    assert_eq!(report.entities["gone"].status, PortStatus::Missing);
    assert_eq!(report.changed_files, ["deleted.rs", "src.rs"]);
    assert_eq!(report.affected_graphs, ["save"]);
    assert!(report.stale);
    assert_eq!(
        verify::read_source_snippet(&store, "src.rs", 13, 13, 0)
            .unwrap()
            .lines[0]
            .line,
        13
    );
}
#[test]
fn blueprint_impact_walks_both_directions_across_canvases() {
    let root = Workspace::new();
    let store = root.open();
    update(
        &store,
        json!({"entities":[entity("ui","trigger_ui"),entity("service","service"),entity("db","store"),entity("test","test_guard")]}),
    );
    graph(
        &store,
        "ui",
        json!({"invariants":["keep guard","canvas rule"],"nodes":[{"entity_id":"ui"},{"entity_id":"service"}],"edges":[edge("a","ui","service")]}),
    );
    graph(
        &store,
        "backend",
        json!({"nodes":[{"entity_id":"service"},{"entity_id":"db"},{"entity_id":"test"}],"edges":[edge("b","service","db"),edge("c","db","test")]}),
    );
    let query = || ImpactQuery {
        entity_id: Some("service".into()),
        file_path: None,
        port_id: Some("p".into()),
        depth: None,
    };
    let report = impact::impact(&store, query()).unwrap();
    assert_eq!(report.matches[0].graphs, ["backend", "ui"]);
    assert_eq!(report.upstream[0].entity_id, "ui");
    assert_eq!(report.downstream[0].entity_id, "db");
    assert_eq!(report.test_guards[0].id, "test");
    assert_eq!(report.invariants.len(), 2);
    assert_eq!(report.invariants[0].severity, Severity::Critical);
    assert!(
        impact::impact(
            &store,
            ImpactQuery {
                depth: Some(1),
                ..query()
            }
        )
        .unwrap()
        .test_guards
        .is_empty()
    );
}
#[test]
fn blueprint_checks_paths_and_does_not_initialize_on_read() {
    let root = Workspace::new();
    assert!(super::workspace_blueprint_hint(&root.0).is_none());
    {
        let store = root.open();
        assert!(
            store
                .upsert_graph("../invalid", GraphUpdate::default())
                .is_err()
        );
        assert!(verify::read_source_snippet(&store, "../../outside.txt", 1, 1, 0).is_err());
        assert!(store.manifest().unwrap().is_none());
        assert!(!root.0.join(".quickagent").exists());
        graph(&store, "api", json!({"entry_kind":"service_api"}));
    }
    let hint = super::workspace_blueprint_hint(&root.0).unwrap();
    assert!(hint.contains("1 graphs") && hint.lines().count() == 1);
}
#[cfg(unix)]
#[test]
fn blueprint_refuses_out_of_scope_symlinks() {
    let root = Workspace::new();
    let outside = Workspace::new();
    fs::write(outside.0.join("marker.txt"), "outside workspace").unwrap();
    std::os::unix::fs::symlink(&outside.0, root.0.join("link")).unwrap();
    std::os::unix::fs::symlink(&outside.0, root.0.join(".quickagent")).unwrap();
    let store = root.open();
    assert!(store.source("link/marker.txt").is_err());
    assert!(store.upsert_graph("test", GraphUpdate::default()).is_err());
    assert!(!outside.0.join("blueprint").exists());
}
#[test]
fn blueprint_projects_endpoint_reports_unbuilt_synced_and_stale() {
    let unbuilt = Workspace::new();
    let built = Workspace::new();
    let stale = Workspace::new();
    fs::write(built.0.join("app.ts"), "export function okFn() {}\n").unwrap();
    fs::write(stale.0.join("app.ts"), "export function missingLater() {}\n").unwrap();
    {
        let s = built.open();
        s.upsert_entities(
            serde_json::from_value(json!({"entities":[{
                "id":"ui.app","name":"App","kind":"trigger_ui","file_path":"app.ts",
                "ports":[{"id":"p1","name":"ok","direction":"out","kind":"event","symbol":"okFn","line_start":1,"line_end":1}]
            }]})).unwrap(),
        ).unwrap();
        graph(&s, "main", json!({"entry_kind":"ui_page","nodes":[{"entity_id":"ui.app"}]}));
    }
    {
        let s = stale.open();
        s.upsert_entities(
            serde_json::from_value(json!({"entities":[{
                "id":"ui.app","name":"App","kind":"trigger_ui","file_path":"app.ts",
                "ports":[{"id":"p1","name":"gone","direction":"out","kind":"event","symbol":"missingLater","line_start":1,"line_end":1}]
            }]})).unwrap(),
        ).unwrap();
        graph(&s, "main", json!({"entry_kind":"ui_page","nodes":[{"entity_id":"ui.app"}]}));
        fs::write(stale.0.join("app.ts"), "export function renamed() {}\n").unwrap();
    }
    let parsed = super::routes::scan_project_statuses(vec![
        unbuilt.0.clone(),
        built.0.clone(),
        stale.0.clone(),
        built.0.clone(),
    ]);
    let items = parsed["projects"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["status"], "unbuilt");
    assert_eq!(items[0]["exists"], false);
    assert!(!unbuilt.0.join(".quickagent").exists());
    assert_eq!(items[1]["status"], "synced");
    assert_eq!(items[1]["graph_count"], 1);
    assert!(items[1]["updated_at"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(items[2]["status"], "stale");
    assert_eq!(items[2]["missing_ports"], 1);
}
