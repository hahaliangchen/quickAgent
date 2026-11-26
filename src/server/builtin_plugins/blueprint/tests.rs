use super::{
    impact::{self, ImpactQuery},
    model::*,
    package::{graph_view, playbook_view},
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
        &store.all_playbooks(&manifest).unwrap(),
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

#[test]
fn blueprint_playbook_crud_and_anchor_drift_verification() {
    let root = Workspace::new();
    git(&root.0, &["init", "-q"]);
    fs::write(root.0.join("src.rs"), "pub fn run() {}\n").unwrap();
    fs::write(root.0.join("gone.rs"), "pub fn legacy() {}\n").unwrap();
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
    {
        let store = root.open();

        // Upsert playbook and verify manifest/file/view
        let created = store
            .upsert_playbook(
                "release-path",
                serde_json::from_value(json!({
                    "title": "桌面端打包最短路径",
                    "trigger": "修改后端并重启桌面端前",
                    "summary": "省去试错，直接构建带 daily-report 的 Release 并同步二进制",
                    "code": "// 1. cargo build --release --features internal-daily-report\n// 2. sync desktop binaries",
                    "anchor_files": ["src.rs", "gone.rs"],
                    "related_graphs": ["main"],
                    "verify_commands": ["cargo test -- blueprint"],
                    "environment": ["201 验证端口 18133 必须使用 venv/bin/python (3.12)"],
                    "pitfalls": ["不要漏掉 internal-daily-report feature"],
                    "source_tasks": ["task-123"],
                    "last_synced_commit": baseline
                }))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(created.id, "release-path");
        assert_eq!(created.anchor_files, ["gone.rs", "src.rs"]);
        assert!(
            root.0
                .join(".quickagent/blueprint/playbooks/release-path.json")
                .is_file()
        );

        // Clean baseline -> ok
        let view = playbook_view(&store, "release-path").unwrap();
        assert_eq!(view["status"]["status"], "ok");

        // Modify one anchor -> drifted
        fs::write(root.0.join("src.rs"), "pub fn run() { /* changed */ }\n").unwrap();
        let manifest = store.manifest().unwrap().unwrap();
        let report = verify::verify(
            &store,
            Some(&manifest),
            &[],
            &[],
            &store.all_playbooks(&manifest).unwrap(),
        );
        assert_eq!(report.drifted_playbooks, 1);
        assert_eq!(report.missing_playbooks, 0);
        assert_eq!(report.affected_playbooks, ["release-path"]);
        assert_eq!(report.playbooks["release-path"].status, PortStatus::Drifted);
        assert_eq!(report.playbooks["release-path"].changed_files, ["src.rs"]);
        assert!(report.stale);

        // Delete another anchor -> missing
        fs::remove_file(root.0.join("gone.rs")).unwrap();
        let view_missing = playbook_view(&store, "release-path").unwrap();
        assert_eq!(view_missing["status"]["status"], "missing");
        assert_eq!(view_missing["status"]["missing_files"], json!(["gone.rs"]));

        // Reject invalid path traversal in anchor_files
        assert!(
            store
                .upsert_playbook(
                    "bad",
                    serde_json::from_value(json!({"anchor_files": ["../outside.rs"]})).unwrap(),
                )
                .is_err()
        );
    }

    // Hint & tail reminders & project scanner reopen Store internally, so run after dropping store guard.
    let hint = super::workspace_blueprint_hint(&root.0).unwrap();
    assert!(hint.contains("0 graphs, 1 playbooks") && hint.lines().count() == 1);
    let tail = super::workspace_blueprint_tail(&root.0).unwrap();
    assert!(tail.contains("Blueprint Environment & Pitfall Reminders"), "{tail}");
    assert!(tail.contains("201 验证端口 18133 必须使用 venv/bin/python (3.12)"), "{tail}");
    assert!(tail.contains("不要漏掉 internal-daily-report feature"), "{tail}");
    assert!(tail.contains("cargo test -- blueprint"), "{tail}");

    let scan = super::routes::scan_project_statuses(vec![root.0.clone()]);
    let item = &scan["projects"][0];
    assert_eq!(item["playbook_count"], 1);
    assert_eq!(item["missing_playbooks"], 1);
    assert_eq!(item["status"], "stale");

    // Delete playbook cleans up file & manifest
    {
        let store = root.open();
        store.delete_playbook("release-path").unwrap();
        assert!(store.playbook("release-path").unwrap().is_none());
        assert!(store.manifest().unwrap().unwrap().playbooks.is_empty());
    }
}

#[test]
fn blueprint_inspect_session_aggregates_sqlite_dossier() {
    let root = Workspace::new();
    let conn = crate::server::agent_service::open_db(&root.0).unwrap();
    conn.execute(
        "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name)
         VALUES ('task-panoramic','Deploy 201 isolation without touching prod 7002','gpt-6.2-sol','completed',1000,61000,'subapi')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_session_meta(task_id,title,workspace) VALUES ('task-panoramic','201 隔离部署','/repo/rag-go')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_task_plans(task_id,turn,steps_json,updated_at) VALUES ('task-panoramic',1,?1,2000)",
        [json!([{"id":"s1","title":"Deploy 201","status":"done"}]).to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_task_pinned_facts(task_id,turn,facts_json,updated_at) VALUES ('task-panoramic',1,?1,2000)",
        [json!(["18133 uses Python 3.12 venv"]).to_string()],
    )
    .unwrap();
    let events = [
        (1, "step/start", json!({"step":1})),
        (2, "program/start", json!({"step":1,"source":"io.edit(...)"})),
        (3, "tool/call", json!({"step":1,"callId":"c1","name":"apply_edits","arguments":json!({"files":[{"path":"test_kg.py"}]}).to_string()})),
        (4, "tool/result", json!({"step":1,"message":{"toolCallId":"c1","isError":true},"meta":{"result":{"error":"old_text not found"}}})),
        (5, "step/start", json!({"step":2})),
        (6, "tool/call", json!({"step":2,"callId":"c2","name":"apply_edits","arguments":json!({"files":[{"path":"test_kg.py"}]}).to_string()})),
        (7, "tool/result", json!({"step":2,"message":{"toolCallId":"c2","isError":true},"meta":{"result":{"error":"old_text not found"}}})),
        (8, "tool/call", json!({"step":2,"callId":"c3","name":"edit_file","arguments":json!({"path":"test_kg.py"}).to_string()})),
        (9, "tool/result", json!({"step":2,"message":{"toolCallId":"c3","isError":false},"meta":{"result":{"path":"test_kg.py","changed":true}}})),
        (10, "tool/call", json!({"step":3,"callId":"c4","name":"ssh__edit_file","arguments":json!({"server_id":"srv-201","path":"/opt/rag/.env"}).to_string()})),
        (11, "tool/result", json!({"step":3,"message":{"toolCallId":"c4","isError":false},"meta":{"result":{"changed":true}}})),
        (12, "tool/call", json!({"step":4,"callId":"c5","name":"ssh__exec","arguments":json!({"server_id":"srv-201","cwd":"/opt/rag","command":"python3 main.py"}).to_string()})),
        (13, "tool/result", json!({"step":4,"message":{"toolCallId":"c5","isError":false},"meta":{"result":{"exit_code":1,"stderr":"ModuleNotFoundError: No module named 'rapidocr'"}}})),
        (14, "tool/call", json!({"step":5,"callId":"c6","name":"ssh__exec","arguments":json!({"server_id":"srv-201","cwd":"/opt/rag","command":"/opt/anydoc/venv/bin/python main.py"}).to_string()})),
        (15, "tool/result", json!({"step":5,"message":{"toolCallId":"c6","isError":false},"meta":{"result":{"exit_code":0,"stdout":"started on 127.0.0.1:18133"}}})),
        (16, "context/usage", json!({"step":5,"cost_amount":0.25})),
        (17, "assistant/message", json!({"step":5,"message":{"content":"All 201 checks passed."}})),
    ];
    for (seq, kind, data) in events {
        conn.execute(
            "INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES ('task-panoramic',?1,?2,?3,?4)",
            rusqlite::params![seq, seq * 1000, kind, data.to_string()],
        )
        .unwrap();
    }
    drop(conn);

    let dossier = super::session_dossier::inspect_session(
        &root.0,
        &json!({"task_id": "task-panoramic"}),
    )
    .unwrap();
    assert_eq!(dossier["task"]["status"], "completed");
    assert_eq!(dossier["task"]["duration_sec"], 60);
    assert_eq!(dossier["task"]["estimated_cost_usd"], 0.25);
    assert_eq!(dossier["plan"]["pinned_facts"], json!(["18133 uses Python 3.12 venv"]));
    assert_eq!(dossier["changed_files"]["local"], json!(["test_kg.py"]));
    assert_eq!(dossier["changed_files"]["remote"][0]["path"], "/opt/rag/.env");
    assert_eq!(dossier["failure_clusters"][0]["tool"], "apply_edits");
    assert_eq!(dossier["failure_clusters"][0]["count"], 2);
    assert_eq!(dossier["environment_signals"]["env_files_touched"], json!(["/opt/rag/.env"]));
    assert!(
        dossier["environment_signals"]["cues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap_or("").contains("venv/bin/python"))
    );
    assert_eq!(dossier["winning_commands"][0]["scope"], "remote");
    assert_eq!(dossier["final_outcome"], "All 201 checks passed.");
    assert_eq!(dossier["task"]["latest_seq"], 17);
    let tail = dossier["recent_tail"].as_array().unwrap();
    assert!(!tail.is_empty());
    assert_eq!(tail.last().unwrap()["seq"], 17);
    assert_eq!(tail.last().unwrap()["kind"], "assistant/message");

    // Verify `max_recent` bounds the returned tail slice to the newest events.
    let bounded = super::session_dossier::inspect_session(
        &root.0,
        &json!({"task_id": "task-panoramic", "max_recent": 2}),
    )
    .unwrap();
    let bounded_tail = bounded["recent_tail"].as_array().unwrap();
    assert_eq!(bounded_tail.len(), 2);
    assert_eq!(bounded_tail[0]["seq"], 15);
    assert_eq!(bounded_tail[1]["seq"], 17);
}

#[test]
fn blueprint_inspect_session_tracks_user_instructions_timeline_and_in_flight_tails() {
    let root = Workspace::new();
    let conn = crate::server::agent_service::open_db(&root.0).unwrap();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let task_id = format!("task-live-{}", crate::server::agent_service::uuid_like());
    conn.execute(
        "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name)
         VALUES (?1,'第1轮：不要改代码，也不要重启服务','gpt-6.2-sol','running',?2,?3,'subapi')",
        rusqlite::params![task_id, now_ms - 600_000, now_ms],
    )
    .unwrap();

    let long_turn1 = format!("第1轮红线不要改代码 {}", "甲".repeat(1200));
    let long_turn3 = format!("第3轮授权修复 anydoc_image_vlm.go 并仅重启 9005 {}", "乙".repeat(1200));
    let events = [
        (1, now_ms - 500_000, "turn/start", json!({"turn": 1})),
        (2, now_ms - 499_000, "user/message", json!({"turn": 1, "message": {"role": "user", "content": long_turn1}})),
        (3, now_ms - 450_000, "turn/end", json!({"turn": 1, "reason": {"kind": "completed"}})),
        (4, now_ms - 400_000, "turn/start", json!({"turn": 2})),
        (5, now_ms - 399_000, "user/message", json!({"turn": 2, "message": {"role": "user", "content": [{"type": "text", "text": "第2轮：先排查日志原因"}]}})),
        (6, now_ms - 350_000, "turn/end", json!({"turn": 2, "reason": {"kind": "completed"}})),
        (7, now_ms - 300_000, "turn/start", json!({"turn": 3})),
        (8, now_ms - 299_000, "user/message", json!({"turn": 3, "message": {"role": "user", "content": long_turn3}})),
        (9, now_ms - 250_000, "user/interjection", json!({"text": "补充授权：如果 9005 报错 422 立即停下来查代码"})),
        // Completed call should NOT appear in in_flight_tools.
        (10, now_ms - 240_000, "tool/call", json!({"step": 25, "callId": "call_done", "name": "read_file", "arguments": json!({"path": "anydoc_image_vlm.go"}).to_string()})),
        (11, now_ms - 239_000, "tool/result", json!({"step": 25, "message": {"toolCallId": "call_done", "isError": false}, "meta": {"result": {"ok": true}}})),
        // In-flight SSH call (reads live tail from ssh::live).
        (12, now_ms - 200_000, "tool/call", json!({"step": 26, "callId": "call_26", "name": "ssh__exec", "arguments": json!({"server_id": "srv-201", "command": "bash verify.sh"}).to_string()})),
        // In-flight local shell call with streaming shell/output lines.
        (13, now_ms - 90_000, "tool/call", json!({"step": 27, "callId": "call_27", "name": "exec_command", "arguments": json!({"cmd": "cargo test"}).to_string()})),
        (14, now_ms - 30_000, "shell/output", json!({"callId": "call_27", "text": "running 2 tests\nFAIL: assertion failed\n"})),
    ];
    for (seq, ts, kind, data) in events {
        conn.execute(
            "INSERT INTO agent_task_events(task_id,seq,timestamp,kind,data) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![task_id, seq, ts, kind, data.to_string()],
        )
        .unwrap();
    }
    drop(conn);

    // Seed SSH live terminal ring buffer with >30 lines and ANSI color codes.
    let mut ssh_stream = String::new();
    for i in 1..=35 {
        ssh_stream.push_str(&format!("\x1b[32mline-{i}\x1b[0m\r\n"));
    }
    ssh_stream.push_str("\x1b[31mHTTP 422: selected document does not exist\x1b[0m\r\n");
    crate::server::builtin_plugins::ssh::live::emit_test_chunk(
        &task_id,
        "srv-201",
        ssh_stream.as_bytes(),
    );

    let dossier = super::session_dossier::inspect_session(
        &root.0,
        &json!({"task_id": task_id}),
    )
    .unwrap();

    // Verify user_instructions timeline & latest_user_instruction.
    let instructions = dossier["user_instructions"].as_array().unwrap();
    assert_eq!(instructions.len(), 4);
    assert_eq!(instructions[0]["turn"], 1);
    assert_eq!(instructions[0]["kind"], "user/message");
    // Turn 1 is an older turn (turns 2 and 3 are the latest 2 turns), clipped to <= 800 chars + ellipsis.
    assert!(instructions[0]["text"].as_str().unwrap().chars().count() <= 801);
    assert_eq!(instructions[1]["turn"], 2);
    assert_eq!(instructions[2]["turn"], 3);
    // Turn 3 is within the latest 2 turns, retains > 1000 chars without aggressive truncation.
    assert!(instructions[2]["text"].as_str().unwrap().chars().count() > 1000);
    assert_eq!(instructions[3]["turn"], 3);
    assert_eq!(instructions[3]["kind"], "user/interjection");
    assert_eq!(dossier["latest_user_instruction"], instructions[3]);
    assert!(
        dossier["latest_user_instruction"]["text"]
            .as_str()
            .unwrap()
            .contains("422")
    );

    // Verify in_flight_tools captures both SSH live tail (capped at 30 lines, ANSI stripped) and local shell/output.
    let in_flight = dossier["in_flight_tools"].as_array().unwrap();
    assert_eq!(in_flight.len(), 2);

    let ssh_call = &in_flight[0];
    assert_eq!(ssh_call["call_id"], "call_26");
    assert_eq!(ssh_call["step"], 26);
    assert_eq!(ssh_call["tool"], "ssh__exec");
    assert!(ssh_call["target"].as_str().unwrap().contains("srv-201"));
    assert!(ssh_call["elapsed_sec"].as_i64().unwrap() >= 190);
    let ssh_tail = ssh_call["tail_lines"].as_array().unwrap();
    assert_eq!(ssh_tail.len(), 30);
    assert_eq!(
        ssh_tail.last().unwrap().as_str().unwrap(),
        "HTTP 422: selected document does not exist"
    );

    let local_call = &in_flight[1];
    assert_eq!(local_call["call_id"], "call_27");
    assert_eq!(local_call["tool"], "exec_command");
    assert!(local_call["elapsed_sec"].as_i64().unwrap() >= 85);
    assert!(local_call["idle_sec"].as_i64().unwrap() >= 25);
    assert_eq!(
        local_call["tail_lines"],
        json!(["running 2 tests", "FAIL: assertion failed"])
    );
}
