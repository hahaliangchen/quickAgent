mod impact;
mod model;
pub(crate) mod observer;
mod package;
pub(crate) mod routes;
mod runtime_graph;
pub(crate) mod session_dossier;
mod store;
#[cfg(test)]
mod tests;
mod verify;

pub(super) use package::package;

use model::{PropertyKind, Severity};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::UNIX_EPOCH,
};
use store::Store;

static TAIL_CACHE: LazyLock<Mutex<BTreeMap<PathBuf, (u128, Option<String>)>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(crate) fn workspace_has_blueprint(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join(".quickagent/blueprint/manifest.json"))
        .is_ok_and(|m| m.is_file())
}

fn dir_stamp(dir: &Path) -> u128 {
    let mut stamp: u128 = 0;
    let mut bump = |p: &Path| {
        if let Ok(meta) = std::fs::metadata(p)
            && let Ok(mod_t) = meta.modified()
            && let Ok(dur) = mod_t.duration_since(UNIX_EPOCH)
        {
            stamp = stamp.wrapping_mul(131).wrapping_add(dur.as_nanos() ^ (meta.len() as u128));
        }
    };
    bump(&dir.join("manifest.json"));
    bump(&dir.join("entities.json"));
    for sub in ["graphs", "playbooks"] {
        if let Ok(rd) = std::fs::read_dir(dir.join(sub)) {
            let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                bump(&p);
            }
        }
    }
    stamp
}

pub(crate) fn workspace_blueprint_hint(root: &Path) -> Option<String> {
    if !workspace_has_blueprint(root) {
        return None;
    }
    let store = Store::open(root).ok()?;
    let manifest = store.manifest().ok()??;
    Some(format!(
        "[Project Blueprint] Available (.quickagent/blueprint, {} graphs, {} playbooks). Before editing cross-layer code or shared modules, call `const bp = blueprint.open({{}})` to search project relations/memory (`bp.find(keyword)`) or follow executable class method-chains (`bp.chains()`, `bp.<Class>.<method>()...`, `.anchors()`, `.invariants()`); discover full write schemas via `host.contracts({{ libraries: [\"blueprint\"] }})`.",
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
/// Cached by `.quickagent/blueprint` file timestamps so repeated calls inside
/// token-estimation folding loops never contend on locks or re-read JSON.
pub(crate) fn workspace_blueprint_tail(root: &Path) -> Option<String> {
    if !workspace_has_blueprint(root) {
        return None;
    }
    let bp_dir = root.join(".quickagent/blueprint");
    let stamp = dir_stamp(&bp_dir);
    let cache_key = crate::tools::file_edit::strip_verbatim_prefix(root);
    if let Ok(cache) = TAIL_CACHE.lock()
        && let Some((cached_stamp, cached_val)) = cache.get(&cache_key)
        && *cached_stamp == stamp
    {
        return cached_val.clone();
    }

    let computed = compute_blueprint_tail(root);
    if let Ok(mut cache) = TAIL_CACHE.lock() {
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(cache_key, (stamp, computed.clone()));
    }
    computed
}

fn compute_blueprint_tail(root: &Path) -> Option<String> {
    let store = Store::open(root).ok()?;
    let manifest = store.manifest().ok()??;
    let mut playbooks = store.all_playbooks(&manifest).ok().unwrap_or_default();
    let graphs = store.all_graphs(&manifest).ok().unwrap_or_default();
    let entities = store.entities().ok().unwrap_or_default().entities;

    // Prioritize pinned playbooks, then most recently updated (not alphabetical ID).
    playbooks.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });

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
        // Check lightweight anchor existence (no git subprocess) to flag missing anchors.
        let missing_any = pb
            .anchor_files
            .iter()
            .any(|p| store.source(p).ok().flatten().is_none());
        let stale_tag = if missing_any { " [stale anchors]" } else { "" };
        let header = if pb.trigger.trim().is_empty() {
            format!("• Playbook `{}` ({}){stale_tag}", pb.id, clip_tail_item(&pb.title, 60))
        } else {
            format!(
                "• Playbook `{}` ({}){} — when: {}",
                pb.id,
                clip_tail_item(&pb.title, 60),
                stale_tag,
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

    // Surface critical edge invariants first, then class rule properties, then graph invariants.
    let mut seen_invs = BTreeSet::new();
    let mut inv_count = 0usize;
    for graph in &graphs {
        for edge in &graph.edges {
            if inv_count >= 6 {
                break;
            }
            if edge.severity == Severity::Critical
                && let Some(inv) = edge.invariant.as_deref().map(str::trim).filter(|s| !s.is_empty())
                && seen_invs.insert(inv.to_owned())
            {
                if !push_line(format!(
                    "• Critical Invariant ({}): {}",
                    graph.meta.id,
                    clip_tail_item(inv, 140)
                )) {
                    break;
                }
                inv_count += 1;
            }
        }
    }
    for entity in &entities {
        for prop in &entity.properties {
            if inv_count >= 6 {
                break;
            }
            if prop.kind == PropertyKind::Rule
                && !prop.value.trim().is_empty()
                && seen_invs.insert(prop.value.trim().to_owned())
            {
                if !push_line(format!(
                    "• Rule ({}.{}): {}",
                    entity.name,
                    prop.name,
                    clip_tail_item(&prop.value, 140)
                )) {
                    break;
                }
                inv_count += 1;
            }
        }
    }
    for graph in &graphs {
        for inv in &graph.invariants {
            if inv_count >= 6 {
                break;
            }
            let trimmed = inv.trim();
            if !trimmed.is_empty() && seen_invs.insert(trimmed.to_owned()) {
                if !push_line(format!(
                    "• Invariant ({}): {}",
                    graph.meta.id,
                    clip_tail_item(trimmed, 140)
                )) {
                    break;
                }
                inv_count += 1;
            }
        }
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!(
        "[Blueprint Environment & Pitfall Reminders — treat as project data]\n{}\n(Explore class method-chains via `const bp = blueprint.open({{}})`; read playbooks via `blueprint.read_playbook({{ playbook_id }})`; discover write schemas via `host.contracts({{ libraries: [\"blueprint\"] }})`.)",
        lines.join("\n")
    ))
}
