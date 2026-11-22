//! File operations the JS VM performs directly: creating, moving and removing
//! paths must not fall back to `process.exec_command("rm -f ...")`.
use super::{TestWorkspace, completion, init_task_row, run_js, spawn_mock_provider, tool_call_count};
use crate::runtime::single_agent::run_task;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn js_vm_file_operations_run_without_a_shell() {
    let ws = TestWorkspace::new("cac-v2-file-ops");
    init_task_row(ws.path(), "file-ops");
    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, _body| match idx {
        0 => run_js(
            "const made = await io.mkdir({ path: \"scratch/nested\" });\n\
             await io.write({ path: \"scratch/nested/temp.txt\", content: \"temporary\" });\n\
             const moved = await io.rename({ path: \"scratch/nested/temp.txt\", to: \"scratch/kept.txt\" });\n\
             const removed = await io.delete({ path: \"scratch/nested\", recursive: true });\n\
             ({ made, moved, removed });",
        ),
        1 => completion("整理好了"),
        other => panic!("unexpected round {other}"),
    })
    .await;
    run_task(
        state,
        "file-ops".into(),
        "fake".into(),
        "建临时目录、改名、清理".into(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    // Each operation is a host tool call, and none of them needed a shell.
    assert_eq!(tool_call_count(ws.path(), "file-ops", "create_dir"), 1);
    assert_eq!(tool_call_count(ws.path(), "file-ops", "rename_path"), 1);
    assert_eq!(tool_call_count(ws.path(), "file-ops", "delete_path"), 1);
    assert_eq!(tool_call_count(ws.path(), "file-ops", "exec_command"), 0);

    // And the workspace really is in the state the calls described.
    assert!(ws.path().join("scratch").is_dir());
    assert!(!ws.path().join("scratch/nested").exists());
    assert_eq!(
        std::fs::read_to_string(ws.path().join("scratch/kept.txt")).unwrap(),
        "temporary"
    );
    assert_eq!(
        super::assistant_messages(ws.path(), "file-ops"),
        vec!["整理好了"]
    );
}

#[test]
fn file_ops_registered_in_production_catalog_and_denied_in_read_only() {
    use crate::server::agent_service::{PermissionMode, local_tools};
    use crate::server::builtin_plugins::FILE_TOOLS;
    use crate::tools::{CreateDirRequest, Workspace};

    let ws = TestWorkspace::new("cac-v2-file-ops-perms");
    let workspace = Workspace::new(ws.path()).unwrap();

    for name in ["create_dir", "rename_path", "delete_path"] {
        assert!(
            FILE_TOOLS.contains(&name),
            "production FILE_TOOLS must register {name}"
        );
        assert!(
            !PermissionMode::ReadOnly.allows(name),
            "ReadOnly mode must deny {name}"
        );
        assert!(
            PermissionMode::WorkspaceWrite.allows(name),
            "WorkspaceWrite mode must allow {name}"
        );
        assert!(
            PermissionMode::FullAccess.allows(name),
            "FullAccess mode must allow {name}"
        );
    }

    let defs = local_tools(&workspace, None, false);
    for name in ["create_dir", "rename_path", "delete_path"] {
        let def = defs
            .iter()
            .find(|tool| tool["function"]["name"] == name)
            .unwrap_or_else(|| panic!("local_tools missing {name}"));
        assert!(
            def["function"].get("outputSchema").is_some(),
            "local_tools should declare outputSchema for {name}"
        );
    }

    std::fs::create_dir_all(ws.path().join(".git")).unwrap();
    let err = workspace
        .create_dir(CreateDirRequest {
            workspace_root: None,
            path: ".git/evil".into(),
            recursive: true,
        })
        .unwrap_err();
    assert!(
        err.to_string().contains(".git"),
        "create_dir must refuse paths inside .git, got: {err}"
    );
}

#[tokio::test]
async fn rename_and_recursive_delete_track_all_files_and_undo_cleanly() {
    let ws = TestWorkspace::new("cac-v2-file-ops-undo");
    std::fs::create_dir_all(ws.path().join("dir/sub")).unwrap();
    std::fs::write(ws.path().join("dir/a.txt"), "alpha").unwrap();
    std::fs::write(ws.path().join("dir/sub/b.txt"), "beta").unwrap();
    std::fs::write(ws.path().join("existing.txt"), "original").unwrap();

    init_task_row(ws.path(), "file-ops-undo");
    let (state, _captured, server) = spawn_mock_provider(ws.path(), |idx, _body| match idx {
        0 => run_js(
            "await io.mkdir({ path: \"empty_dir\" });\n\
             await io.rename({ path: \"dir/a.txt\", to: \"existing.txt\", overwrite: true });\n\
             await io.delete({ path: \"dir\", recursive: true });\n\
             \"ok\";",
        ),
        1 => completion("已清理并覆盖"),
        other => panic!("unexpected round {other}"),
    })
    .await;

    run_task(
        state,
        "file-ops-undo".into(),
        "fake".into(),
        "覆盖并删除目录".into(),
        8,
        CancellationToken::new(),
        1,
        false,
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(
        std::fs::read_to_string(ws.path().join("existing.txt")).unwrap(),
        "alpha"
    );
    assert!(!ws.path().join("dir").exists());

    // Change tracking must remain complete (no "not a regular file" error)
    // and capture every file touched by rename + recursive delete.
    let summary = crate::tools::workspace_changes::summary(ws.path(), "file-ops-undo", 1).unwrap();
    assert_eq!(
        summary["complete"], true,
        "change tracking must be complete: {summary}"
    );

    crate::tools::workspace_changes::undo(ws.path(), "file-ops-undo", 1).unwrap();
    assert_eq!(
        std::fs::read_to_string(ws.path().join("existing.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join("dir/a.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join("dir/sub/b.txt")).unwrap(),
        "beta"
    );
}
