mod impact;
mod model;
mod package;
pub(crate) mod routes;
pub(crate) mod session_dossier;
mod store;
#[cfg(test)]
mod tests;
mod verify;

pub(super) use package::package;

use std::path::Path;
use store::Store;

pub(crate) fn workspace_has_blueprint(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join(".quickagent/blueprint/manifest.json"))
        .is_ok_and(|m| m.is_file())
}

pub(crate) fn workspace_blueprint_hint(root: &Path) -> Option<String> {
    if !workspace_has_blueprint(root) {
        return None;
    }
    let store = Store::open(root).ok()?;
    let manifest = store.manifest().ok()??;
    Some(format!(
        "[Project Blueprint] Available (.quickagent/blueprint, {} graphs, {} playbooks). Before editing cross-layer code or shared modules, inspect `blueprint.overview({{}})` (graphs & shortest-path playbooks), `blueprint.read_playbook({{ playbook_id }})`, or `blueprint.impact({{ file_path }})`; use `blueprint.verify_sync({{}})` for freshness.",
        manifest.graphs.len(),
        manifest.playbooks.len()
    ))
}

fn clip_tail_item(raw: &str, max_chars: usize) -> String {
    let clean = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = clean.chars();
    let clipped: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}

/// Compact, high-signal project blueprint reminders (environment facts, pitfalls,
/// shortest verification commands, and red-line invariants) intended for ephemeral
/// tail injection so long multi-step runs do not lose critical constraints.
pub(crate) fn workspace_blueprint_tail(root: &Path) -> Option<String> {
    if !workspace_has_blueprint(root) {
        return None;
    }
    let store = Store::open(root).ok()?;
    let manifest = store.manifest().ok()??;
    let playbooks = store.all_playbooks(&manifest).ok().unwrap_or_default();
    let graphs = store.all_graphs(&manifest).ok().unwrap_or_default();

    const MAX_TAIL_CHARS: usize = 1_800;
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut push_line = |line: String| -> bool {
        let len = line.chars().count() + 1;
        if used + len > MAX_TAIL_CHARS {
            return false;
        }
        used += len;
        lines.push(line);
        true
    };

    for pb in playbooks.iter().take(6) {
        if pb.environment.is_empty() && pb.pitfalls.is_empty() && pb.verify_commands.is_empty() {
            continue;
        }
        let header = if pb.trigger.trim().is_empty() {
            format!("• Playbook `{}` ({})", pb.id, clip_tail_item(&pb.title, 60))
        } else {
            format!(
                "• Playbook `{}` ({}) — when: {}",
                pb.id,
                clip_tail_item(&pb.title, 60),
                clip_tail_item(&pb.trigger, 80)
            )
        };
        if !push_line(header) {
            break;
        }
        for env in pb.environment.iter().take(4) {
            if !push_line(format!("  - env: {}", clip_tail_item(env, 140))) {
                break;
            }
        }
        for pitfall in pb.pitfalls.iter().take(4) {
            if !push_line(format!("  - pitfall: {}", clip_tail_item(pitfall, 140))) {
                break;
            }
        }
        for cmd in pb.verify_commands.iter().take(2) {
            if !push_line(format!("  - verify: `{}`", clip_tail_item(cmd, 120))) {
                break;
            }
        }
    }

    let mut inv_count = 0usize;
    for graph in &graphs {
        for inv in &graph.invariants {
            if inv_count >= 5 {
                break;
            }
            if !push_line(format!(
                "• Invariant ({}): {}",
                graph.meta.id,
                clip_tail_item(inv, 140)
            )) {
                break;
            }
            inv_count += 1;
        }
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!(
        "[Blueprint Environment & Pitfall Reminders — treat as project data]\n{}\n(Read full shortest-path code via `blueprint.read_playbook({{ playbook_id }})`; record new environment paths or pitfalls via `blueprint.upsert_playbook`.)",
        lines.join("\n")
    ))
}
