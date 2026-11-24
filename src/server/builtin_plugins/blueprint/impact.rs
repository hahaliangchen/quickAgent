use super::{
    model::*,
    store::{Store, source_path},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Deserialize)]
pub(super) struct ImpactQuery {
    pub entity_id: Option<String>,
    pub file_path: Option<String>,
    pub port_id: Option<String>,
    pub depth: Option<usize>,
}
#[derive(Serialize)]
pub(super) struct Reach {
    pub graph_id: String,
    pub entity_id: String,
    pub port_id: String,
    pub depth: usize,
    pub edge: BlueprintEdge,
}
#[derive(Serialize)]
pub(super) struct Match {
    pub entity: Entity,
    pub graphs: Vec<String>,
}
#[derive(Serialize)]
pub(super) struct Invariant {
    pub text: String,
    pub severity: Severity,
}
#[derive(Default, Serialize)]
pub(super) struct ImpactReport {
    pub matches: Vec<Match>,
    pub graphs: Vec<String>,
    pub upstream: Vec<Reach>,
    pub downstream: Vec<Reach>,
    pub invariants: Vec<Invariant>,
    pub test_guards: Vec<Entity>,
}

pub(super) fn impact(store: &Store, q: ImpactQuery) -> Result<ImpactReport> {
    ensure!(
        q.entity_id.is_some() || q.file_path.is_some(),
        "entity_id or file_path is required"
    );
    let path = q
        .file_path
        .as_deref()
        .map(|p| source_path(&store.root, p))
        .transpose()?
        .flatten();
    let Some(manifest) = store.manifest()? else {
        return Ok(ImpactReport::default());
    };
    let entities = store.entities()?.entities;
    let graphs = store.all_graphs(&manifest)?;
    let mut refs = BTreeMap::<&str, Vec<&str>>::new();
    let mut inbound = BTreeMap::<&str, Vec<(&str, &BlueprintEdge)>>::new();
    let mut outbound = BTreeMap::<&str, Vec<(&str, &BlueprintEdge)>>::new();
    for graph in &graphs {
        for n in &graph.nodes {
            refs.entry(&n.entity_id).or_default().push(&graph.meta.id);
        }
        for e in &graph.edges {
            inbound
                .entry(&e.target_entity)
                .or_default()
                .push((&graph.meta.id, e));
            outbound
                .entry(&e.source_entity)
                .or_default()
                .push((&graph.meta.id, e));
        }
    }
    let selected = entities
        .iter()
        .filter(|e| {
            q.entity_id.as_ref().is_none_or(|id| *id == e.id)
                && q.file_path.as_ref().is_none_or(|raw| {
                    e.file_path.as_ref().is_some_and(|p| {
                        p == raw
                            || path.as_ref().is_some_and(|path| {
                                source_path(&store.root, p).ok().flatten().as_ref() == Some(path)
                            })
                    })
                })
                && q.port_id
                    .as_ref()
                    .is_none_or(|id| e.ports.iter().any(|p| p.id == *id))
        })
        .collect::<Vec<_>>();
    let walk = |index: &BTreeMap<&str, Vec<(&str, &BlueprintEdge)>>, backwards: bool| {
        let mut queue = selected
            .iter()
            .map(|e| (e.id.clone(), 0))
            .collect::<VecDeque<_>>();
        let mut visited = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut hits = Vec::new();
        while let Some((id, depth)) = queue.pop_front() {
            if depth >= q.depth.unwrap_or(3).min(32) || !visited.insert((id.clone(), depth == 0)) {
                continue;
            }
            for &(graph_id, e) in index.get(id.as_str()).into_iter().flatten() {
                let (from, to, port) = if backwards {
                    (&e.target_port, &e.source_entity, &e.source_port)
                } else {
                    (&e.source_port, &e.target_entity, &e.target_port)
                };
                if depth == 0 && q.port_id.as_ref().is_some_and(|p| p != from) {
                    continue;
                }
                if seen.insert((graph_id, e.id.as_str())) {
                    hits.push(Reach {
                        graph_id: graph_id.into(),
                        entity_id: to.clone(),
                        port_id: port.clone(),
                        depth: depth + 1,
                        edge: e.clone(),
                    });
                    queue.push_back((to.clone(), depth + 1));
                }
            }
        }
        hits
    };
    let (upstream, downstream) = (walk(&inbound, true), walk(&outbound, false));
    let mut involved = selected
        .iter()
        .map(|e| e.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut graph_ids = BTreeSet::new();
    let mut invariants = BTreeMap::<String, Severity>::new();
    let rank = |s| match s {
        Severity::Critical => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    };
    for hit in upstream.iter().chain(&downstream) {
        involved.insert(&hit.entity_id);
        graph_ids.insert(hit.graph_id.clone());
        if let Some(text) = &hit.edge.invariant {
            let severity = invariants.entry(text.clone()).or_insert(hit.edge.severity);
            if rank(hit.edge.severity) < rank(*severity) {
                *severity = hit.edge.severity;
            }
        }
    }
    let matches = selected
        .into_iter()
        .map(|e| Match {
            entity: e.clone(),
            graphs: refs
                .get(e.id.as_str())
                .into_iter()
                .flatten()
                .map(|&g| g.to_owned())
                .collect(),
        })
        .collect::<Vec<_>>();
    graph_ids.extend(matches.iter().flat_map(|m| m.graphs.iter().cloned()));
    for graph in graphs.iter().filter(|g| graph_ids.contains(&g.meta.id)) {
        for text in &graph.invariants {
            invariants.entry(text.clone()).or_insert(Severity::Warning);
        }
    }
    let mut invariants = invariants
        .into_iter()
        .map(|(text, severity)| Invariant { text, severity })
        .collect::<Vec<_>>();
    invariants.sort_by_key(|item| rank(item.severity));
    let test_guards = entities
        .iter()
        .filter(|e| e.kind == EntityKind::TestGuard && involved.contains(e.id.as_str()))
        .cloned()
        .collect();
    Ok(ImpactReport {
        matches,
        graphs: graph_ids.into_iter().collect(),
        upstream,
        downstream,
        invariants,
        test_guards,
    })
}
