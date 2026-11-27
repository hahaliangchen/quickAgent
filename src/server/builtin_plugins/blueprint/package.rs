use super::{impact, model::*, runtime_graph, store::Store, verify};
use crate::server::{
    builtin_plugins::ToolHandler,
    plugin_protocol::{NativeContributions, NativeTool, ToolPermission},
    skill_packages::Package,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn definitions() -> Vec<Value> {
    serde_json::from_str(include_str!("tool.json")).expect("compiled blueprint tool definitions")
}
pub(super) fn graph_view(store: &Store, id: &str, canvas: bool) -> Result<Value> {
    let graph = store.graph(id)?.context("graph not found")?;
    let ids = graph
        .nodes
        .iter()
        .map(|n| n.entity_id.as_str())
        .collect::<BTreeSet<_>>();
    let entities = store
        .entities()?
        .entities
        .into_iter()
        .filter(|e| ids.contains(e.id.as_str()))
        .collect::<Vec<_>>();
    let manifest = store.manifest()?;
    let verification = verify::verify(
        store,
        manifest.as_ref(),
        &entities,
        std::slice::from_ref(&graph),
        &[],
    );
    let chains = runtime_graph::compute_chains(std::slice::from_ref(&graph), &entities);
    let mut value = serde_json::to_value(graph)?;
    if !canvas {
        value.as_object_mut().unwrap().remove("nodes");
    }
    Ok(json!({"graph":value,"entities":entities,"statuses":verification.entities,"chains":chains}))
}

pub(super) fn playbook_view(store: &Store, id: &str) -> Result<Value> {
    let playbook = store.playbook(id)?.context("playbook not found")?;
    let manifest = store.manifest()?;
    let verification = verify::verify(
        store,
        manifest.as_ref(),
        &[],
        &[],
        std::slice::from_ref(&playbook),
    );
    let status = verification
        .playbooks
        .get(&playbook.id)
        .cloned()
        .unwrap_or_default();
    Ok(json!({"playbook": playbook, "status": status}))
}

fn execute(workspace: &Path, name: &str, args: Value) -> Result<Value> {
    let writable = name.starts_with("blueprint__upsert_") || name == "blueprint__resolve_inbox";
    ensure!(
        !writable || args.get("workspace_root").is_none(),
        "blueprint writes are limited to the current workspace"
    );
    let selected = args["workspace_root"]
        .as_str()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .unwrap_or(workspace);
    let root = if selected.is_absolute() {
        selected.to_owned()
    } else {
        workspace.join(selected)
    };
    let store = Store::open(&root)?;
    Ok(match name {
        "blueprint__overview" => {
            let manifest = store.manifest()?;
            let pending_inbox = store
                .inbox()
                .unwrap_or_default()
                .into_iter()
                .filter(|item| item.status == ObserverSubmissionStatus::Pending)
                .count();
            json!({"exists":manifest.is_some(),"manifest":manifest,"pending_inbox":pending_inbox})
        }
        "blueprint__open" => {
            runtime_graph::build_runtime_graph(&store, args["graph_id"].as_str())?
        }
        "blueprint__read_graph" => graph_view(
            &store,
            args["graph_id"].as_str().context("graph_id is required")?,
            false,
        )?,
        "blueprint__read_playbook" => playbook_view(
            &store,
            args["playbook_id"]
                .as_str()
                .context("playbook_id is required")?,
        )?,
        "blueprint__inspect_session" => {
            ensure!(
                workspace.join(".codex-workspace-mcp/codex_state.db").is_file(),
                "blueprint__inspect_session requires host state database context"
            );
            super::session_dossier::inspect_session(workspace, &args)?
        }
        "blueprint__list_inbox" => {
            let status_filter = args["status"].as_str().map(str::trim).filter(|s| !s.is_empty());
            let items: Vec<ObserverSubmission> = store
                .inbox()?
                .into_iter()
                .filter(|item| match status_filter {
                    Some("pending") => item.status == ObserverSubmissionStatus::Pending,
                    Some("merged") => item.status == ObserverSubmissionStatus::Merged,
                    Some("dismissed") => item.status == ObserverSubmissionStatus::Dismissed,
                    _ => true,
                })
                .collect();
            json!({"submissions": items, "count": items.len()})
        }
        "blueprint__resolve_inbox" => {
            let id = args["id"].as_str().context("id is required")?;
            let patch: ObserverSubmissionPatch = serde_json::from_value(args.clone())?;
            let updated = store.update_observer_note_status(id, patch)?;
            serde_json::to_value(updated)?
        }
        "blueprint__impact" => {
            serde_json::to_value(impact::impact(&store, serde_json::from_value(args)?)?)?
        }
        "blueprint__verify_sync" => {
            let manifest = store.manifest()?;
            let graphs = manifest
                .as_ref()
                .map(|m| store.all_graphs(m))
                .transpose()?
                .unwrap_or_default();
            let playbooks = manifest
                .as_ref()
                .map(|m| store.all_playbooks(m))
                .transpose()?
                .unwrap_or_default();
            let entities = if manifest.is_some() {
                store.entities()?.entities
            } else {
                Vec::new()
            };
            serde_json::to_value(verify::verify(
                &store,
                manifest.as_ref(),
                &entities,
                &graphs,
                &playbooks,
            ))?
        }
        "blueprint__upsert_entities" => {
            let all = store.upsert_entities(serde_json::from_value(args)?)?;
            json!({"entity_count":all.entities.len()})
        }
        "blueprint__upsert_graph" => {
            let update: GraphUpdate = serde_json::from_value(args)?;
            let id = update.id.clone().context("id is required")?;
            let graph = store.upsert_graph(&id, update)?;
            json!({"id":id,"node_count":graph.nodes.len(),"edge_count":graph.edges.len()})
        }
        "blueprint__upsert_playbook" => {
            let update: PlaybookUpdate = serde_json::from_value(args)?;
            let id = update.id.clone().context("id is required")?;
            let playbook = store.upsert_playbook(&id, update)?;
            json!({
                "id": playbook.id,
                "title": playbook.title,
                "anchor_count": playbook.anchor_files.len(),
                "related_graphs": playbook.related_graphs.len()
            })
        }
        _ => anyhow::bail!("unknown blueprint tool"),
    })
}

pub(in crate::server::builtin_plugins) fn package() -> Result<Package> {
    let tools = definitions()
        .into_iter()
        .map(|definition| {
            let name = definition["name"]
                .as_str()
                .expect("blueprint tool name")
                .to_owned();
            let permission =
                if name.starts_with("blueprint__upsert_") || name == "blueprint__resolve_inbox" {
                    ToolPermission::WorkspaceWrite
                } else {
                    ToolPermission::ReadOnly
                };
            let target = name.clone();
            let handler: ToolHandler = Arc::new(move |workspace, _, args| {
                let target = target.clone();
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || execute(workspace.root(), &target, args))
                        .await?
                })
            });
            (
                name,
                NativeTool {
                    definition,
                    handler,
                    permission,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let root = std::env::var_os("GROW_RESOURCES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    crate::server::skill_packages::load_builtin(
        &root.join("src/server/builtin_plugins/blueprint"),
        Arc::new(NativeContributions {
            tools,
            frontend_entries: vec!["blueprint.workbench".into()],
            on_unmount: None,
        }),
    )
}
