use super::{
    TestWorkspace, completion, init_task_row, last_tool_output, run_js, spawn_mock_provider,
};
use crate::server::{
    agent_service::{self, PermissionMode},
    skill_packages::{mcp_packages, test_runtime},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const STDIO: &str = r#"
import json, os, sys
assert sys.argv[1] == 'literal ; $(not-a-shell)'
assert os.environ['MCP_EXPLICIT'] == 'fixture'
assert 'CARGO_MANIFEST_DIR' not in os.environ
sys.stderr.write('x' * 5000 + 'mcp-mock-stderr-tail')
sys.stderr.flush()
phase = 0
for line in sys.stdin:
    req = json.loads(line)
    assert req['jsonrpc'] == '2.0'
    method = req['method']
    if method == 'initialize':
        assert phase == 0 and req['params']['protocolVersion'] == '2025-03-26'
        phase = 1
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}}}
    elif method == 'notifications/initialized':
        assert phase == 1 and 'id' not in req
        phase = 2
        continue
    elif method == 'tools/list':
        assert phase == 2
        names = ['echo', 'unsafe', 'conflicting', 'large'] + ['lookup_' + str(i) for i in range(26)]
        result = {'tools': [dict(name=name, description='external-schema-marker-' + name,
            inputSchema={'type': 'object', 'properties': {'msg': {'type': 'string', 'minLength': 1}}},
            outputSchema={'type': 'object', 'properties': {'sample': {'type': 'string'}}},
            annotations={} if name == 'unsafe' else {'readOnlyHint': True, 'destructiveHint': name == 'conflicting'})
            for name in names]}
    elif method == 'tools/call':
        assert phase == 2
        name = req['params']['name']
        arg = req['params']['arguments']
        if arg.get('exit'):
            sys.exit(12)
        msg = 'x' * 70000 if name == 'large' else arg.get('msg', 'ok')
        print(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/message'}), flush=True)
        result = {'content': [{'type': 'text', 'text': msg}]}
    else:
        assert False, method
    print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': result}), flush=True)
"#;

fn config() -> Value {
    json!({"command":if cfg!(windows) { "python" } else { "python3" }, "args":["-u","-c",STDIO,"literal ; $(not-a-shell)"],"env":{"MCP_EXPLICIT":"fixture"}})
}

#[tokio::test]
async fn mcp_stdio_vm_sync_promises_lazy_contracts_permission_and_assets() {
    let work = TestWorkspace::new("mcp-stdio-vm");
    let task = "mcp-vm";
    init_task_row(work.path(), task);
    let (runtime, catalog) = test_runtime();
    let (mut state, captured, mock) = spawn_mock_provider(work.path(), |step, request| match step {
        0 => run_js("const a=mcp_demo.echo({msg:'mcp-sync-ok'}); a.content[0].text;"),
        1 => {
            assert!(last_tool_output(&request).contains("mcp-sync-ok"));
            run_js("const b=await mcp_demo.promises.echo({msg:'mcp-async-ok'}); b.content[0].text;")
        }
        2 => {
            assert!(last_tool_output(&request).contains("mcp-async-ok"));
            run_js("const found=host.find_methods({library:'mcp_demo'}); const types=host.contracts({methods:['mcp_demo.echo']}); [found.methods.length, types.declarations, types.schemas[0].inputSchema];")
        }
        3 => {
            let last = last_tool_output(&request);
            assert!(last.contains("30") && last.contains("external-schema-marker-echo") && last.contains("minLength"), "{last}");
            assert!(!last.contains("external-schema-marker-lookup_0"));
            run_js("mcp_demo.unsafe({});")
        }
        4 => {
            assert!(last_tool_output(&request).contains("permission_denied"));
            run_js("await mcp_demo.promises.conflicting({});")
        }
        5 => {
            assert!(last_tool_output(&request).contains("permission_denied"));
            run_js("const big=mcp_demo.large({}); big.content[0].text.length;")
        }
        6 => {
            assert!(last_tool_output(&request).contains("70000"));
            completion("done")
        }
        _ => panic!("unexpected VM step {step}"),
    }).await;
    state.permission_mode = PermissionMode::ReadOnly;
    state.tool_catalog = Some(catalog.clone());
    mcp_packages::mount_test_server(&runtime, &catalog.packages, "demo", config()).await;
    assert_eq!(catalog.packages.mcp_status()[0]["tool_count"], 30);
    crate::runtime::single_agent::run_task(
        state,
        task.into(),
        "fake".into(),
        "MCP regression".into(),
        12,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    mock.abort();
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 7);
    let system = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("mcp_demo (30 tools)"));
    assert!(!system.contains("external-schema-marker") && !system.contains("lookup_0"));
    let summary = system
        .lines()
        .find(|line| line.contains("MCP libraries"))
        .unwrap();
    assert!(
        summary.len() + 2 <= 512,
        "MCP increased pinned header by {} bytes",
        summary.len() + 2
    );
    for request in requests.iter() {
        assert_eq!(request["messages"][0]["content"], system);
        super::assert_single_run_js_tool(request);
    }
    let db = agent_service::open_db(work.path()).unwrap();
    let asset_id: String = db.query_row("SELECT asset_id FROM agent_context_assets WHERE task_id=?1 AND source_tool='mcp_demo__large'", [task], |row| row.get(0)).unwrap();
    let asset =
        crate::context::context_assets::snapshot(work.path(), task, &json!({"asset_id":asset_id}))
            .unwrap();
    assert!(asset.to_string().contains(&"x".repeat(70_000)));
    let stderr = catalog.packages.mcp_stderr("mcp-demo");
    assert_eq!(stderr.len(), 4096);
    assert!(stderr.ends_with("mcp-mock-stderr-tail"));
    let exec = catalog.packages.mcp_tool("mcp_demo__echo").unwrap();
    let work =
        std::sync::Arc::new(crate::tools::Workspace::new(work.path().to_path_buf()).unwrap());
    assert!(
        (exec.handler)(work, "mcp_demo__echo".into(), json!({"exit":true}))
            .await
            .is_err()
    );
    assert_eq!(catalog.packages.mcp_status()[0]["status"], "disconnected");
    mcp_packages::reconnect(&runtime, &catalog.packages, "mcp-demo")
        .await
        .unwrap();
    assert_eq!(catalog.packages.mcp_status()[0]["status"], "connected");
}

#[tokio::test]
async fn mcp_execution_uses_ask_and_auto_review_approval() {
    let work = TestWorkspace::new("mcp-approval");
    let (runtime, catalog) = test_runtime();
    mcp_packages::mount_test_server(&runtime, &catalog.packages, "demo", config()).await;
    for (mode, task) in [
        (PermissionMode::Ask, "mcp-ask"),
        (PermissionMode::AutoReview, "mcp-review"),
    ] {
        init_task_row(work.path(), task);
        let (mut state, _, mock) =
            spawn_mock_provider(work.path(), |_, _| completion("not an approval verdict")).await;
        state.tool_catalog = Some(catalog.clone());
        state.permission_mode = mode;
        assert!(crate::tools::tool_approvals::needs_approval(
            mode,
            "mcp_demo__unsafe",
            Some(&catalog),
            false
        ));
        assert!(!crate::tools::tool_approvals::needs_approval(
            mode,
            "mcp_demo__echo",
            Some(&catalog),
            false
        ));
        let cancel = CancellationToken::new();
        let s = super::super::Session {
            agent_id: None,
            task_node_id: None,
            plugin: None,
            state: &state,
            task,
            model: "fake",
            turn: 1,
            cancel: &cancel,
            ban_execution: false,
            scope: "main".into(),
            parent: None,
            input_budget: 100_000,
        };
        let invoke = super::super::business_tool(
            &s,
            "node",
            "mcp_demo__unsafe",
            json!({"msg":"would execute"}),
            "call",
        );
        let deny = async {
            let db = agent_service::open_db(work.path()).unwrap();
            loop {
                let pending = db.query_row("SELECT EXISTS(SELECT 1 FROM agent_task_events WHERE task_id=?1 AND kind='approval/request')",[task],|r|r.get::<_,bool>(0)).unwrap();
                if pending {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            db.execute(
                "INSERT INTO tool_approvals(task_id,call_id,decision) VALUES (?1,'call','deny')",
                [task],
            )
            .unwrap();
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(8), async { tokio::join!(invoke, deny) })
                .await
                .unwrap();
        assert!(result.unwrap_err().to_string().contains("declined"));
        mock.abort();
    }
}
