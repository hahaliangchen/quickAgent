//! Projects Blueprint entities (classes), properties (anchors), ports (methods),
//! and edges (method calls) into:
//! 1. Live JS class & method-chain payloads consumed by `blueprint.open({})` in `run_js`;
//! 2. Precomputed entry-to-leaf method call chains for UI display and AI exploration.

use super::{model::*, store::Store};
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_CHAINS: usize = 64;
const MAX_CHAIN_DEPTH: usize = 12;

/// Convert a human/module display name into a clean JS identifier (e.g. `SshWorkbench`, `add_server`).
pub(super) fn js_ident(raw: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut capitalize_next = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            if capitalize_next && ch.is_ascii_lowercase() {
                out.push(ch.to_ascii_uppercase());
            } else {
                out.push(ch);
            }
            capitalize_next = false;
        } else if !out.is_empty() {
            capitalize_next = true;
        }
    }
    if out.is_empty() {
        out = fallback
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
            .collect();
    }
    if out.is_empty() {
        out.push_str("Item");
    }
    if out.as_bytes()[0].is_ascii_digit() {
        out.insert(0, '_');
    }
    out
}

fn is_reserved_graph_key(key: &str) -> bool {
    matches!(
        key,
        "__blueprint_graph"
            | "__bp_classes_id"
            | "exists"
            | "graph_id"
            | "class_map"
            | "class_names"
            | "chain_list"
            | "file_list"
            | "classes"
            | "chains"
            | "files"
            | "class"
            | "find"
            | "toString"
            | "valueOf"
            | "hasOwnProperty"
    )
}

fn entity_anchor_files(entity: &Entity) -> Vec<String> {
    let mut out = BTreeSet::new();
    if let Some(fp) = entity.file_path.as_deref() {
        let norm = normalize_rel_path(fp);
        if !norm.is_empty() {
            out.insert(norm);
        }
    }
    for prop in &entity.properties {
        match prop.kind {
            PropertyKind::File => {
                let norm = normalize_rel_path(&prop.value);
                if !norm.is_empty() {
                    out.insert(norm);
                }
            }
            PropertyKind::Symbol => {
                if let Some((f, _)) = prop.value.split_once('#') {
                    let norm = normalize_rel_path(f);
                    if !norm.is_empty() {
                        out.insert(norm);
                    }
                }
            }
            _ => {}
        }
    }
    out.into_iter().collect()
}

fn entity_rules(entity: &Entity) -> Vec<String> {
    entity
        .properties
        .iter()
        .filter(|p| p.kind == PropertyKind::Rule && !p.value.trim().is_empty())
        .map(|p| format!("{}.{}: {}", entity.name, p.name, p.value.trim()))
        .collect()
}

fn assign_class_names(entities: &[Entity]) -> BTreeMap<String, String> {
    let mut used = BTreeSet::new();
    let mut by_id = BTreeMap::new();
    for entity in entities {
        let base = js_ident(&entity.name, &entity.id);
        let mut candidate = if is_reserved_graph_key(&base) {
            format!("{base}Class")
        } else {
            base
        };
        let mut suffix = 2usize;
        while !used.insert(candidate.clone()) {
            candidate = format!("{}_{suffix}", js_ident(&entity.name, &entity.id));
            suffix += 1;
        }
        by_id.insert(entity.id.clone(), candidate);
    }
    by_id
}

fn method_js_name(port: &Port) -> String {
    if let Some(sym) = port.symbol.as_deref().filter(|s| !s.trim().is_empty()) {
        let ident = js_ident(sym, &port.id);
        if !ident.is_empty() {
            return ident;
        }
    }
    let from_id = js_ident(&port.id, "call");
    if from_id != "Item" {
        return from_id;
    }
    js_ident(&port.name, &port.id)
}

/// Compute entry-to-leaf method call chains across one or more graphs.
pub(super) fn compute_chains(graphs: &[BlueprintGraph], entities: &[Entity]) -> Vec<Value> {
    let entity_map: BTreeMap<&str, &Entity> =
        entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let class_names = assign_class_names(entities);
    let mut chains = Vec::new();

    for graph in graphs {
        if graph.edges.is_empty() {
            continue;
        }
        let mut incoming = BTreeSet::<(&str, &str)>::new();
        let mut outgoing = BTreeMap::<(&str, &str), Vec<&BlueprintEdge>>::new();
        for edge in &graph.edges {
            incoming.insert((edge.target_entity.as_str(), edge.target_port.as_str()));
            outgoing
                .entry((edge.source_entity.as_str(), edge.source_port.as_str()))
                .or_default()
                .push(edge);
        }

        // Entry points: methods with outgoing edges and no incoming edges (prioritizing trigger_* classes).
        let mut entries: Vec<(&str, &str)> = outgoing
            .keys()
            .copied()
            .filter(|k| !incoming.contains(k))
            .collect();
        if entries.is_empty() {
            // Cycle fallback: start from trigger classes or first outgoing key.
            entries = outgoing.keys().copied().take(4).collect();
        }
        entries.sort_by_key(|(eid, pid)| {
            let is_trigger = entity_map.get(eid).is_some_and(|e| {
                matches!(
                    e.kind,
                    EntityKind::TriggerUi | EntityKind::TriggerApi | EntityKind::TriggerJob
                )
            });
            (!is_trigger, *eid, *pid)
        });

        for (start_e, start_p) in entries {
            if chains.len() >= MAX_CHAINS {
                break;
            }
            let mut stack: Vec<(
                Vec<(String, String)>,
                Vec<String>,
                BTreeSet<String>,
                BTreeSet<String>,
            )> = Vec::new();
            let start_cls = class_names
                .get(start_e)
                .cloned()
                .unwrap_or_else(|| js_ident(start_e, "Class"));
            let start_m = entity_map
                .get(start_e)
                .and_then(|e| e.ports.iter().find(|p| p.id == start_p))
                .map(method_js_name)
                .unwrap_or_else(|| js_ident(start_p, "call"));
            let mut init_files = BTreeSet::new();
            let mut init_invs = BTreeSet::new();
            if let Some(ent) = entity_map.get(start_e) {
                init_files.extend(entity_anchor_files(ent));
                init_invs.extend(entity_rules(ent));
            }
            for inv in &graph.invariants {
                if !inv.trim().is_empty() {
                    init_invs.insert(inv.trim().to_owned());
                }
            }
            stack.push((
                vec![(start_e.to_owned(), start_p.to_owned())],
                vec![format!("{start_cls}.{start_m}")],
                init_files,
                init_invs,
            ));

            while let Some((path, labels, files, invs)) = stack.pop() {
                if chains.len() >= MAX_CHAINS {
                    break;
                }
                let (cur_e, cur_p) = path.last().unwrap();
                let next_edges = outgoing.get(&(cur_e.as_str(), cur_p.as_str()));
                let mut extended = false;
                if path.len() < MAX_CHAIN_DEPTH
                    && let Some(edges) = next_edges
                {
                    for edge in edges {
                        let next_key = (edge.target_entity.clone(), edge.target_port.clone());
                        if path.contains(&next_key) {
                            continue;
                        }
                        extended = true;
                        let mut next_path = path.clone();
                        next_path.push(next_key);
                        let next_cls = class_names
                            .get(&edge.target_entity)
                            .cloned()
                            .unwrap_or_else(|| js_ident(&edge.target_entity, "Class"));
                        let next_m = entity_map
                            .get(edge.target_entity.as_str())
                            .and_then(|e| e.ports.iter().find(|p| p.id == edge.target_port))
                            .map(method_js_name)
                            .unwrap_or_else(|| js_ident(&edge.target_port, "call"));
                        let mut next_labels = labels.clone();
                        next_labels.push(format!("{next_cls}.{next_m}"));
                        let mut next_files = files.clone();
                        let mut next_invs = invs.clone();
                        if let Some(ent) = entity_map.get(edge.target_entity.as_str()) {
                            next_files.extend(entity_anchor_files(ent));
                            next_invs.extend(entity_rules(ent));
                        }
                        if let Some(inv) = edge.invariant.as_deref().filter(|s| !s.trim().is_empty()) {
                            next_invs.insert(inv.trim().to_owned());
                        }
                        stack.push((next_path, next_labels, next_files, next_invs));
                    }
                }
                if !extended && labels.len() >= 2 {
                    // Build executable method-chain expression: `bp.ClassA.m1().m2()...`
                    let mut expr = format!("bp.{}", labels[0]);
                    expr.push_str("()");
                    for step in &labels[1..] {
                        if let Some((_, method)) = step.split_once('.') {
                            expr.push('.');
                            expr.push_str(method);
                            expr.push_str("()");
                        }
                    }
                    chains.push(json!({
                        "graph_id": graph.meta.id,
                        "graph_title": graph.meta.title,
                        "entry": labels[0],
                        "steps": labels,
                        "expression": expr,
                        "files": files.into_iter().collect::<Vec<_>>(),
                        "invariants": invs.into_iter().collect::<Vec<_>>(),
                    }));
                }
            }
        }
    }
    chains
}

/// Build the executable Blueprint class-graph snapshot returned by `blueprint.open({})`.
pub(super) fn build_runtime_graph(store: &Store, graph_id: Option<&str>) -> Result<Value> {
    let Some(manifest) = store.manifest()? else {
        return Ok(json!({
            "__blueprint_graph": true,
            "exists": false,
            "graph_id": graph_id,
            "class_map": {},
            "class_names": [],
            "chain_list": [],
            "file_list": []
        }));
    };
    let all_entities = store.entities()?.entities;
    let graphs = match graph_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(gid) => vec![store.graph(gid)?.context("graph not found")?],
        None => store.all_graphs(&manifest)?,
    };

    let entities: Vec<Entity> = if graph_id.is_some() {
        let scoped_ids: BTreeSet<&str> = graphs
            .iter()
            .flat_map(|g| g.nodes.iter().map(|n| n.entity_id.as_str()))
            .collect();
        all_entities
            .into_iter()
            .filter(|e| scoped_ids.contains(e.id.as_str()))
            .collect()
    } else {
        all_entities
    };

    let class_names = assign_class_names(&entities);
    let entity_map: BTreeMap<&str, &Entity> =
        entities.iter().map(|e| (e.id.as_str(), e)).collect();

    // Index outgoing method calls by (entity_id, port_id).
    let mut outgoing = BTreeMap::<(&str, &str), Vec<Value>>::new();
    for graph in &graphs {
        for edge in &graph.edges {
            let target_class = class_names
                .get(&edge.target_entity)
                .cloned()
                .unwrap_or_else(|| js_ident(&edge.target_entity, "Class"));
            let target_method_js = entity_map
                .get(edge.target_entity.as_str())
                .and_then(|e| e.ports.iter().find(|p| p.id == edge.target_port))
                .map(method_js_name)
                .unwrap_or_else(|| js_ident(&edge.target_port, "call"));
            outgoing
                .entry((edge.source_entity.as_str(), edge.source_port.as_str()))
                .or_default()
                .push(json!({
                    "edge_id": edge.id,
                    "graph_id": graph.meta.id,
                    "target_class": target_class,
                    "target_class_id": edge.target_entity,
                    "target_method": target_method_js,
                    "target_method_id": edge.target_port,
                    "relation": edge.relation,
                    "label": edge.label,
                    "invariant": edge.invariant,
                    "severity": edge.severity,
                }));
        }
    }

    let mut classes_obj = Map::new();
    let mut root_obj = Map::new();
    let mut all_files = BTreeSet::new();
    let mut class_name_list = Vec::new();

    for entity in &entities {
        let cls_name = class_names[&entity.id].clone();
        class_name_list.push(cls_name.clone());

        // Properties map (key -> value) + detailed property metadata.
        let mut props_values = Map::new();
        let mut props_meta = Vec::new();
        let mut has_primary_file = false;
        for prop in &entity.properties {
            if prop.kind == PropertyKind::File
                && entity
                    .file_path
                    .as_deref()
                    .is_some_and(|fp| normalize_rel_path(fp) == normalize_rel_path(&prop.value))
            {
                has_primary_file = true;
            }
            props_values.insert(prop.name.clone(), json!(prop.value));
            props_meta.push(json!({
                "id": prop.id,
                "name": prop.name,
                "kind": prop.kind,
                "value": prop.value,
                "summary": prop.summary,
            }));
        }
        if let Some(fp) = entity.file_path.as_deref() {
            let norm = normalize_rel_path(fp);
            if !norm.is_empty() && !has_primary_file {
                props_values
                    .entry("file".to_owned())
                    .or_insert_with(|| json!(norm));
                props_meta.push(json!({
                    "id": "file",
                    "name": "file",
                    "kind": PropertyKind::File,
                    "value": norm,
                    "summary": "",
                }));
            }
        }
        let anchor_files = entity_anchor_files(entity);
        all_files.extend(anchor_files.iter().cloned());
        let rules = entity_rules(entity);

        let mut methods_obj = Map::new();
        for port in &entity.ports {
            let js_name = method_js_name(port);
            let next = outgoing
                .get(&(entity.id.as_str(), port.id.as_str()))
                .cloned()
                .unwrap_or_default();
            methods_obj.insert(
                js_name.clone(),
                json!({
                    "id": port.id,
                    "js_name": js_name,
                    "name": port.name,
                    "symbol": port.symbol,
                    "reads": port.reads,
                    "summary": port.summary,
                    "next": next,
                }),
            );
        }

        let class_node = json!({
            "__blueprint_node": true,
            "id": entity.id,
            "name": entity.name,
            "class_name": cls_name,
            "kind": entity.kind,
            "description": entity.summary,
            "file_path": entity.file_path,
            "properties": Value::Object(props_values),
            "property_list": props_meta,
            "anchor_files": anchor_files,
            "rules": rules,
            "method_map": Value::Object(methods_obj),
            "chain_trace": [],
            "chain_anchors": anchor_files,
            "chain_invariants": rules,
            "chain_branches": [],
        });

        classes_obj.insert(cls_name.clone(), class_node.clone());
        root_obj.insert(cls_name, class_node);
    }

    let chains = compute_chains(&graphs, &entities);
    root_obj.insert("__blueprint_graph".into(), json!(true));
    root_obj.insert("exists".into(), json!(true));
    root_obj.insert("graph_id".into(), json!(graph_id));
    root_obj.insert("class_map".into(), Value::Object(classes_obj));
    root_obj.insert("class_names".into(), json!(class_name_list));
    root_obj.insert("chain_list".into(), json!(chains));
    root_obj.insert("file_list".into(), json!(all_files.into_iter().collect::<Vec<_>>()));

    Ok(Value::Object(root_obj))
}
