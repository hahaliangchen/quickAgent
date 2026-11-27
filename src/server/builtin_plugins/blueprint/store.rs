use super::model::*;
use anyhow::{Context, Result, ensure};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{ErrorKind, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

const LIMIT: u64 = 8 * 1024 * 1024;
static WORKSPACE_LOCKS: LazyLock<Mutex<BTreeMap<PathBuf, Arc<RwLock<()>>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn lock_for_workspace(root: &Path) -> Arc<RwLock<()>> {
    let key = crate::tools::file_edit::strip_verbatim_prefix(root);
    let mut map = WORKSPACE_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key)
        .or_insert_with(|| Arc::new(RwLock::new(())))
        .clone()
}

pub(super) struct Store {
    pub root: PathBuf,
    root_dir: Dir,
    lock: Arc<RwLock<()>>,
}

fn subdir(dir: &Dir, name: &str, create: bool) -> Result<Option<Dir>> {
    match dir.open_dir_nofollow(name) {
        Ok(dir) => Ok(Some(dir)),
        Err(e) if e.kind() == ErrorKind::NotFound && create => {
            match dir.create_dir(name) {
                Ok(()) => (),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
            Ok(Some(dir.open_dir_nofollow(name)?))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn read<T: DeserializeOwned>(dir: &Dir, name: &str) -> Result<Option<T>> {
    let file = match dir.open_with(
        name,
        OpenOptions::new().read(true).follow(FollowSymlinks::No),
    ) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "blueprint entry is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "blueprint JSON exceeds size limit"
    );
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        format!("invalid blueprint file {name}")
    })?))
}
fn write(dir: &Dir, name: &str, value: &impl Serialize) -> Result<()> {
    // Serialize through Value so object keys are ordered regardless of struct field order.
    let mut bytes = serde_json::to_vec_pretty(&serde_json::to_value(value)?)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "blueprint JSON exceeds size limit"
    );
    let temp = format!(".{}.tmp", crate::server::agent_service::uuid_like());
    let result = (|| {
        let mut file = dir.open_with(&temp, OpenOptions::new().write(true).create_new(true))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        dir.rename(&temp, dir, name)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = dir.remove_file(temp);
    }
    result
}

// Canonicalize every existing prefix as well as the final path: a missing file
// below an out-of-workspace symlink is not a valid future source anchor either.
pub(super) fn source_path(root: &Path, raw: &str) -> Result<Option<PathBuf>> {
    let trimmed = raw.trim();
    ensure!(!trimmed.is_empty(), "source path must be workspace-relative");
    // Reject Windows drive prefixes or root prefixes on all platforms before normalizing separators.
    let raw_path = Path::new(trimmed);
    let posix = trimmed.replace('\\', "/");
    let posix_path = Path::new(&posix);
    let has_drive_prefix = trimmed.len() >= 2
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
        && trimmed.as_bytes()[1] == b':';
    ensure!(
        !has_drive_prefix
            && !posix.starts_with('/')
            && raw_path
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
            && posix_path
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
        "source path must be workspace-relative"
    );
    let mut path = root.to_owned();
    for component in posix_path.components() {
        path.push(component);
        match path.canonicalize() {
            Ok(resolved) => {
                ensure!(
                    resolved.starts_with(root),
                    "source path is outside workspace"
                );
                path = resolved;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                ensure!(
                    std::fs::symlink_metadata(&path).is_err(),
                    "source link cannot be resolved"
                );
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Some(path))
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        ensure!(!root.as_os_str().is_empty(), "workspace_root is required");
        let root = root.canonicalize().context("invalid workspace root")?;
        let root_dir = Dir::open_ambient_dir(&root, ambient_authority())?;
        let lock = lock_for_workspace(&root);
        Ok(Self {
            root,
            root_dir,
            lock,
        })
    }
    fn read_lock(&self) -> RwLockReadGuard<'_, ()> {
        self.lock.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write_lock(&self) -> RwLockWriteGuard<'_, ()> {
        self.lock.write().unwrap_or_else(|e| e.into_inner())
    }
    pub fn source(&self, raw: &str) -> Result<Option<String>> {
        let Some(path) = source_path(&self.root, raw)? else {
            return Ok(None);
        };
        let relative = path.strip_prefix(&self.root)?;
        ensure!(
            !relative.as_os_str().is_empty() && self.root_dir.metadata(relative)?.is_file(),
            "source is not a regular file"
        );
        let mut bytes = Vec::new();
        self.root_dir
            .open(relative)?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= LIMIT,
            "source file exceeds size limit"
        );
        Ok(Some(
            String::from_utf8(bytes).context("source is not UTF-8")?,
        ))
    }
    fn dir(&self, create: bool) -> Result<Option<Dir>> {
        let Some(parent) = subdir(&self.root_dir, ".quickagent", create)? else {
            return Ok(None);
        };
        subdir(&parent, "blueprint", create)
    }
    fn graphs(&self, create: bool) -> Result<Option<Dir>> {
        let Some(dir) = self.dir(create)? else {
            return Ok(None);
        };
        subdir(&dir, "graphs", create)
    }
    fn playbooks(&self, create: bool) -> Result<Option<Dir>> {
        let Some(dir) = self.dir(create)? else {
            return Ok(None);
        };
        subdir(&dir, "playbooks", create)
    }
    fn manifest_unlocked(&self) -> Result<Option<Manifest>> {
        let Some(dir) = self.dir(false)? else {
            return Ok(None);
        };
        let manifest: Option<Manifest> = read(&dir, "manifest.json")?;
        if let Some(m) = &manifest {
            ensure!(m.version == 1, "unsupported blueprint version");
        }
        Ok(manifest)
    }
    pub fn manifest(&self) -> Result<Option<Manifest>> {
        let _r = self.read_lock();
        self.manifest_unlocked()
    }
    fn entities_unlocked(&self) -> Result<EntityStore> {
        let Some(dir) = self.dir(false)? else {
            return Ok(EntityStore::default());
        };
        Ok(read(&dir, "entities.json")?.unwrap_or_default())
    }
    pub fn entities(&self) -> Result<EntityStore> {
        let _r = self.read_lock();
        self.entities_unlocked()
    }
    fn graph_unlocked(&self, id: &str) -> Result<Option<BlueprintGraph>> {
        graph_id(id)?;
        let Some(dir) = self.graphs(false)? else {
            return Ok(None);
        };
        let graph: Option<BlueprintGraph> = read(&dir, &format!("{id}.json"))?;
        if let Some(graph) = &graph {
            ensure!(graph.meta.id == id, "graph ID does not match filename");
        }
        Ok(graph)
    }
    pub fn graph(&self, id: &str) -> Result<Option<BlueprintGraph>> {
        let _r = self.read_lock();
        self.graph_unlocked(id)
    }
    fn all_graphs_unlocked(&self, manifest: &Manifest) -> Result<(Vec<BlueprintGraph>, Vec<String>)> {
        let mut loaded = Vec::with_capacity(manifest.graphs.len());
        let mut missing = Vec::new();
        for g in &manifest.graphs {
            match self.graph_unlocked(&g.id)? {
                Some(graph) => loaded.push(graph),
                None => missing.push(g.id.clone()),
            }
        }
        Ok((loaded, missing))
    }
    pub fn all_graphs(&self, manifest: &Manifest) -> Result<Vec<BlueprintGraph>> {
        let _r = self.read_lock();
        Ok(self.all_graphs_unlocked(manifest)?.0)
    }
    fn playbook_unlocked(&self, id: &str) -> Result<Option<Playbook>> {
        graph_id(id)?;
        let Some(dir) = self.playbooks(false)? else {
            return Ok(None);
        };
        let playbook: Option<Playbook> = read(&dir, &format!("{id}.json"))?;
        if let Some(playbook) = &playbook {
            ensure!(playbook.id == id, "playbook ID does not match filename");
        }
        Ok(playbook)
    }
    pub fn playbook(&self, id: &str) -> Result<Option<Playbook>> {
        let _r = self.read_lock();
        self.playbook_unlocked(id)
    }
    fn all_playbooks_unlocked(&self, manifest: &Manifest) -> Result<Vec<Playbook>> {
        manifest
            .playbooks
            .iter()
            .filter_map(|p| self.playbook_unlocked(&p.id).transpose())
            .collect()
    }
    pub fn all_playbooks(&self, manifest: &Manifest) -> Result<Vec<Playbook>> {
        let _r = self.read_lock();
        self.all_playbooks_unlocked(manifest)
    }
    fn save_manifest(
        &self,
        mut m: Manifest,
        kind: Option<ProjectKind>,
        commit: Option<String>,
    ) -> Result<()> {
        if let Some(kind) = kind {
            m.project_kind = kind;
        }
        if commit.is_some() {
            m.last_synced_commit = commit;
        }
        m.updated_at = chrono::Utc::now().to_rfc3339();
        m.graphs.sort_by(|a, b| a.id.cmp(&b.id));
        m.playbooks.sort_by(|a, b| a.id.cmp(&b.id));
        write(&self.dir(true)?.unwrap(), "manifest.json", &m)
    }
    pub fn upsert_entities(&self, update: EntityUpdate) -> Result<EntityStore> {
        let _w = self.write_lock();
        let manifest = self.manifest_unlocked()?.unwrap_or_default();
        let (graphs, _) = self.all_graphs_unlocked(&manifest)?;
        let playbooks = self.all_playbooks_unlocked(&manifest)?;
        let mut all = self
            .entities_unlocked()?
            .entities
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect::<BTreeMap<_, _>>();
        for id in &update.remove_entity_ids {
            item_id(id)?;
            all.remove(id);
        }
        for mut e in update.entities {
            item_id(&e.id)?;
            ensure!(!e.name.trim().is_empty(), "entity name is required");
            if let Some(path) = e.file_path.take() {
                let norm = normalize_rel_path(&path);
                if !norm.is_empty() {
                    source_path(&self.root, &norm)?;
                    e.file_path = Some(norm);
                }
            }
            let mut props = BTreeMap::new();
            if !update.replace_properties
                && let Some(old) = all.get(&e.id)
            {
                props.extend(old.properties.iter().cloned().map(|p| (p.id.clone(), p)));
            }
            let mut seen_props = BTreeSet::new();
            for mut prop in e.properties {
                item_id(&prop.id)?;
                ensure!(seen_props.insert(prop.id.clone()), "duplicate property ID");
                prop.name = prop.name.trim().to_owned();
                prop.value = prop.value.trim().to_owned();
                prop.summary = prop.summary.trim().to_owned();
                ensure!(
                    !prop.name.is_empty() && !prop.value.is_empty(),
                    "property name and value are required"
                );
                if prop.kind == PropertyKind::File {
                    let norm = normalize_rel_path(&prop.value);
                    source_path(&self.root, &norm)?;
                    prop.value = norm;
                } else if prop.kind == PropertyKind::Symbol
                    && let Some((file_part, sym_part)) = prop.value.split_once('#')
                {
                    let norm = normalize_rel_path(file_part);
                    source_path(&self.root, &norm)?;
                    prop.value = format!("{norm}#{}", sym_part.trim());
                }
                props.insert(prop.id.clone(), prop);
            }
            e.properties = props.into_values().collect();
            let mut ports = BTreeMap::new();
            if !update.replace_ports
                && let Some(old) = all.get(&e.id)
            {
                ports.extend(old.ports.iter().cloned().map(|p| (p.id.clone(), p)));
            }
            let mut seen = BTreeSet::new();
            for mut port in e.ports {
                item_id(&port.id)?;
                ensure!(seen.insert(port.id.clone()), "duplicate port ID");
                ensure!(
                    !port.name.trim().is_empty()
                        && port.line_start != Some(0)
                        && port
                            .line_end
                            .is_none_or(|end| port.line_start.is_some_and(|start| end >= start)),
                    "invalid port name or line range"
                );
                port.reads = port
                    .reads
                    .into_iter()
                    .map(|r| r.trim().to_owned())
                    .filter(|r| !r.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                ports.insert(port.id.clone(), port);
            }
            e.ports = ports.into_values().collect();
            e.traits.sort();
            e.traits.dedup();
            all.insert(e.id.clone(), e);
        }
        let dir = self.dir(true)?.unwrap();
        let graph_dir = self.graphs(true)?.unwrap();
        for mut graph in graphs {
            let old = (graph.nodes.len(), graph.edges.len());
            graph.nodes.retain(|n| all.contains_key(&n.entity_id));
            graph.edges.retain(|edge| edge_ports(&all, edge));
            if old != (graph.nodes.len(), graph.edges.len()) {
                write(&graph_dir, &format!("{}.json", graph.meta.id), &graph)?;
            }
        }
        if !update.remove_entity_ids.is_empty()
            && let Some(pb_dir) = self.playbooks(false)?
        {
            for mut pb in playbooks {
                let before = pb.related_entities.len();
                pb.related_entities.retain(|eid| all.contains_key(eid));
                if pb.related_entities.len() != before {
                    write(&pb_dir, &format!("{}.json", pb.id), &pb)?;
                }
            }
        }
        let entities = EntityStore {
            entities: all.into_values().collect(),
        };
        write(&dir, "entities.json", &entities)?;
        self.save_manifest(manifest, update.project_kind, update.last_synced_commit)?;
        Ok(entities)
    }
    pub fn upsert_graph(&self, id: &str, update: GraphUpdate) -> Result<BlueprintGraph> {
        let _w = self.write_lock();
        graph_id(id)?;
        ensure!(
            update.id.as_deref().is_none_or(|arg| arg == id),
            "graph ID does not match route"
        );
        let mut manifest = self.manifest_unlocked()?.unwrap_or_default();
        let mut graph = self.graph_unlocked(id)?.unwrap_or_else(|| BlueprintGraph {
            meta: GraphSummary {
                id: id.into(),
                title: id.into(),
                entry_kind: EntryKind::default(),
                entry_summary: String::new(),
            },
            invariants: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        });
        if let Some(title) = update.title {
            ensure!(!title.trim().is_empty(), "graph title is required");
            graph.meta.title = title;
        }
        if let Some(kind) = update.entry_kind {
            graph.meta.entry_kind = kind;
        }
        if let Some(summary) = update.entry_summary {
            graph.meta.entry_summary = summary;
        }
        if let Some(invariants) = update.invariants {
            graph.invariants = invariants;
        }
        let entities = self
            .entities_unlocked()?
            .entities
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect::<BTreeMap<_, _>>();
        if let Some(nodes) = update.nodes {
            let old = graph
                .nodes
                .iter()
                .map(|n| (n.entity_id.as_str(), (n.x, n.y)))
                .collect::<BTreeMap<_, _>>();
            graph.nodes = nodes
                .into_iter()
                .map(|mut n| {
                    if let Some((x, y)) = old.get(n.entity_id.as_str()) {
                        n.x = n.x.or(*x);
                        n.y = n.y.or(*y);
                    }
                    n
                })
                .collect();
        }
        let mut ids = BTreeSet::new();
        for n in &graph.nodes {
            ensure!(
                entities.contains_key(&n.entity_id) && ids.insert(n.entity_id.clone()),
                "unknown or duplicate node entity: {}",
                n.entity_id
            );
        }
        if let Some(edges) = update.edges {
            graph.edges = edges;
        } else {
            graph
                .edges
                .retain(|e| ids.contains(&e.source_entity) && ids.contains(&e.target_entity));
        }
        let mut edge_ids = BTreeSet::new();
        for edge in &graph.edges {
            item_id(&edge.id)?;
            ensure!(
                edge_ids.insert(&edge.id)
                    && ids.contains(&edge.source_entity)
                    && ids.contains(&edge.target_entity)
                    && edge_ports(&entities, edge),
                "invalid or duplicate edge: {}",
                edge.id
            );
        }
        place(&mut graph.nodes, &entities, &graph.edges);
        let dir = self.dir(true)?.unwrap();
        if read::<EntityStore>(&dir, "entities.json")?.is_none() {
            write(&dir, "entities.json", &EntityStore::default())?;
        }
        write(&self.graphs(true)?.unwrap(), &format!("{id}.json"), &graph)?;
        manifest.graphs.retain(|g| g.id != id);
        manifest.graphs.push(graph.meta.clone());
        self.save_manifest(manifest, update.project_kind, update.last_synced_commit)?;
        Ok(graph)
    }
    pub fn delete_graph(&self, id: &str) -> Result<()> {
        let _w = self.write_lock();
        graph_id(id)?;
        let Some(mut manifest) = self.manifest_unlocked()? else {
            return Ok(());
        };
        if let Some(dir) = self.graphs(false)? {
            match dir.remove_file(format!("{id}.json")) {
                Ok(()) => (),
                Err(e) if e.kind() == ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(pb_dir) = self.playbooks(false)? {
            for mut pb in self.all_playbooks_unlocked(&manifest)? {
                let before = pb.related_graphs.len();
                pb.related_graphs.retain(|gid| gid != id);
                if pb.related_graphs.len() != before {
                    write(&pb_dir, &format!("{}.json", pb.id), &pb)?;
                }
            }
        }
        manifest.graphs.retain(|g| g.id != id);
        self.save_manifest(manifest, None, None)
    }
    pub fn upsert_playbook(&self, id: &str, update: PlaybookUpdate) -> Result<Playbook> {
        let _w = self.write_lock();
        graph_id(id)?;
        ensure!(
            update.id.as_deref().is_none_or(|arg| arg == id),
            "playbook ID does not match route"
        );
        let mut manifest = self.manifest_unlocked()?.unwrap_or_default();
        let mut playbook = self.playbook_unlocked(id)?.unwrap_or_else(|| Playbook {
            id: id.into(),
            title: id.into(),
            trigger: String::new(),
            summary: String::new(),
            code: String::new(),
            anchor_files: Vec::new(),
            related_entities: Vec::new(),
            related_graphs: Vec::new(),
            verify_commands: Vec::new(),
            environment: Vec::new(),
            pitfalls: Vec::new(),
            source_tasks: Vec::new(),
            pinned: false,
            updated_at: String::new(),
        });
        if let Some(title) = update.title {
            ensure!(!title.trim().is_empty(), "playbook title is required");
            playbook.title = title.trim().to_owned();
        }
        if let Some(trigger) = update.trigger {
            playbook.trigger = trigger.trim().to_owned();
        }
        if let Some(summary) = update.summary {
            playbook.summary = summary.trim().to_owned();
        }
        if let Some(code) = update.code {
            playbook.code = code;
        }
        if let Some(files) = update.anchor_files {
            let mut clean = BTreeSet::new();
            for raw in files {
                let norm = normalize_rel_path(&raw);
                if norm.is_empty() {
                    continue;
                }
                source_path(&self.root, &norm)?;
                clean.insert(norm);
            }
            playbook.anchor_files = clean.into_iter().collect();
        }
        if let Some(entities) = update.related_entities {
            let mut clean = BTreeSet::new();
            for item in entities {
                let trimmed = item.trim();
                if trimmed.is_empty() {
                    continue;
                }
                item_id(trimmed)?;
                clean.insert(trimmed.to_owned());
            }
            playbook.related_entities = clean.into_iter().collect();
        }
        if let Some(graphs) = update.related_graphs {
            let mut clean = BTreeSet::new();
            for item in graphs {
                let trimmed = item.trim();
                if trimmed.is_empty() {
                    continue;
                }
                graph_id(trimmed)?;
                clean.insert(trimmed.to_owned());
            }
            playbook.related_graphs = clean.into_iter().collect();
        }
        if let Some(commands) = update.verify_commands {
            playbook.verify_commands = commands
                .into_iter()
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty())
                .collect();
        }
        if let Some(env_facts) = update.environment {
            playbook.environment = env_facts
                .into_iter()
                .map(|e| e.trim().to_owned())
                .filter(|e| !e.is_empty())
                .collect();
        }
        if let Some(pitfalls) = update.pitfalls {
            playbook.pitfalls = pitfalls
                .into_iter()
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect();
        }
        if let Some(tasks) = update.source_tasks {
            let mut seen = BTreeSet::new();
            playbook.source_tasks = tasks
                .into_iter()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty() && seen.insert(t.clone()))
                .collect();
        }
        if let Some(pinned) = update.pinned {
            playbook.pinned = pinned;
        }
        playbook.updated_at = chrono::Utc::now().to_rfc3339();
        let dir = self.dir(true)?.unwrap();
        if read::<EntityStore>(&dir, "entities.json")?.is_none() {
            write(&dir, "entities.json", &EntityStore::default())?;
        }
        write(&self.playbooks(true)?.unwrap(), &format!("{id}.json"), &playbook)?;
        manifest.playbooks.retain(|p| p.id != id);
        manifest.playbooks.push(playbook.summary_entry());
        self.save_manifest(manifest, update.project_kind, update.last_synced_commit)?;
        Ok(playbook)
    }
    pub fn delete_playbook(&self, id: &str) -> Result<()> {
        let _w = self.write_lock();
        graph_id(id)?;
        let Some(mut manifest) = self.manifest_unlocked()? else {
            return Ok(());
        };
        if let Some(dir) = self.playbooks(false)? {
            match dir.remove_file(format!("{id}.json")) {
                Ok(()) => (),
                Err(e) if e.kind() == ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        manifest.playbooks.retain(|p| p.id != id);
        self.save_manifest(manifest, None, None)
    }
    pub fn save_layout(&self, id: &str, nodes: Vec<GraphNode>) -> Result<()> {
        let _w = self.write_lock();
        let mut graph = self.graph_unlocked(id)?.context("graph not found")?;
        let positions = nodes
            .into_iter()
            .map(|n| (n.entity_id, (n.x, n.y)))
            .collect::<BTreeMap<_, _>>();
        for n in &mut graph.nodes {
            if let Some(&(Some(x), Some(y))) = positions.get(&n.entity_id) {
                n.x = Some(x);
                n.y = Some(y);
            }
        }
        write(
            &self.graphs(false)?.context("no graphs directory")?,
            &format!("{id}.json"),
            &graph,
        )
    }
    pub fn inbox(&self) -> Result<Vec<ObserverSubmission>> {
        let _r = self.read_lock();
        let Some(dir) = self.dir(false)? else {
            return Ok(Vec::new());
        };
        let mut store: ObserverInboxStore = read(&dir, "inbox.json")?.unwrap_or_default();
        store
            .submissions
            .sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        Ok(store.submissions)
    }
    pub fn submit_observer_note(
        &self,
        input: ObserverSubmissionCreate,
    ) -> Result<ObserverSubmission> {
        let _w = self.write_lock();
        let title = input.title.trim().to_owned();
        ensure!(!title.is_empty(), "submission title is required");
        let source_task_id = input.source_task_id.trim().to_owned();
        ensure!(!source_task_id.is_empty(), "source_task_id is required");
        let id = match input.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => {
                graph_id(id)?;
                id.to_owned()
            }
            None => format!(
                "obs-{}",
                &crate::server::agent_service::uuid_like()[..8]
            ),
        };
        let clean_list = |items: Vec<String>, posix_paths: bool| -> Vec<String> {
            let mut out = Vec::new();
            for item in items {
                let trimmed = if posix_paths {
                    normalize_rel_path(&item)
                } else {
                    item.trim().to_owned()
                };
                if !trimmed.is_empty() && !out.contains(&trimmed) {
                    out.push(trimmed);
                }
            }
            out
        };
        let now = chrono::Utc::now().to_rfc3339();
        let dir = self.dir(true)?.unwrap();
        let mut store: ObserverInboxStore = read(&dir, "inbox.json")?.unwrap_or_default();
        if let Some(existing) = store.submissions.iter_mut().find(|item| {
            item.id == id
                || (input.source_seq.is_some()
                    && item.source_task_id == source_task_id
                    && item.source_seq == input.source_seq)
        }) {
            existing.title = title;
            existing.trigger = input.trigger.trim().to_owned();
            existing.summary = input.summary.trim().to_owned();
            existing.anchor_files = clean_list(input.anchor_files, true);
            existing.environment = clean_list(input.environment, false);
            existing.pitfalls = clean_list(input.pitfalls, false);
            existing.verify_commands = clean_list(input.verify_commands, false);
            if !input.source_task_title.trim().is_empty() {
                existing.source_task_title = input.source_task_title.trim().to_owned();
            }
            if input.observer_task_id.is_some() {
                existing.observer_task_id = input.observer_task_id;
            }
            existing.updated_at = now;
            let updated = existing.clone();
            write(&dir, "inbox.json", &store)?;
            return Ok(updated);
        }
        let submission = ObserverSubmission {
            id,
            source_task_id,
            source_task_title: input.source_task_title.trim().to_owned(),
            observer_task_id: input.observer_task_id,
            source_seq: input.source_seq,
            title,
            trigger: input.trigger.trim().to_owned(),
            summary: input.summary.trim().to_owned(),
            anchor_files: clean_list(input.anchor_files, true),
            environment: clean_list(input.environment, false),
            pitfalls: clean_list(input.pitfalls, false),
            verify_commands: clean_list(input.verify_commands, false),
            status: ObserverSubmissionStatus::Pending,
            merged_playbook_id: None,
            created_at: now.clone(),
            updated_at: now,
        };
        store.submissions.push(submission.clone());
        store
            .submissions
            .sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        store.submissions.truncate(80);
        write(&dir, "inbox.json", &store)?;
        Ok(submission)
    }
    pub fn update_observer_note_status(
        &self,
        id: &str,
        patch: ObserverSubmissionPatch,
    ) -> Result<ObserverSubmission> {
        let _w = self.write_lock();
        graph_id(id)?;
        let dir = self.dir(false)?.context("blueprint inbox not found")?;
        let mut store: ObserverInboxStore = read(&dir, "inbox.json")?.unwrap_or_default();
        let entry = store
            .submissions
            .iter_mut()
            .find(|item| item.id == id)
            .context("observer submission not found")?;
        entry.status = patch.status;
        if let Some(pb) = patch.merged_playbook_id {
            entry.merged_playbook_id = Some(pb);
        }
        entry.updated_at = chrono::Utc::now().to_rfc3339();
        let updated = entry.clone();
        write(&dir, "inbox.json", &store)?;
        Ok(updated)
    }
    pub fn delete_observer_note(&self, id: &str) -> Result<()> {
        let _w = self.write_lock();
        graph_id(id)?;
        let Some(dir) = self.dir(false)? else {
            return Ok(());
        };
        let mut store: ObserverInboxStore = read(&dir, "inbox.json")?.unwrap_or_default();
        let before = store.submissions.len();
        store.submissions.retain(|item| item.id != id);
        if store.submissions.len() != before {
            write(&dir, "inbox.json", &store)?;
        }
        Ok(())
    }
}
fn edge_ports(all: &BTreeMap<String, Entity>, edge: &BlueprintEdge) -> bool {
    all.get(&edge.source_entity).is_some_and(|e| {
        e.ports
            .iter()
            .any(|p| p.id == edge.source_port && p.direction != Direction::In)
    }) && all.get(&edge.target_entity).is_some_and(|e| {
        e.ports
            .iter()
            .any(|p| p.id == edge.target_port && p.direction != Direction::Out)
    })
}
fn height(entity: &Entity) -> f64 {
    108. + entity.properties.len() as f64 * 26. + entity.ports.len() as f64 * 31.
}
fn place(nodes: &mut [GraphNode], entities: &BTreeMap<String, Entity>, edges: &[BlueprintEdge]) {
    let mut columns = nodes
        .iter()
        .map(|n| {
            (
                n.entity_id.clone(),
                match entities[&n.entity_id].kind {
                    EntityKind::TriggerUi | EntityKind::TriggerApi | EntityKind::TriggerJob => 0,
                    EntityKind::Service => 1,
                    _ => 2,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    // Layer acyclic service chains; cap relaxation for cycles.
    let mut indegree = columns
        .keys()
        .map(|k| (k.clone(), 0))
        .collect::<BTreeMap<_, _>>();
    let mut next = BTreeMap::<&str, Vec<&str>>::new();
    for edge in edges {
        *indegree.get_mut(&edge.target_entity).unwrap() += 1;
        next.entry(&edge.source_entity)
            .or_default()
            .push(&edge.target_entity);
    }
    let mut queue = indegree
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(k, _)| k.clone())
        .collect::<std::collections::VecDeque<_>>();
    while let Some(from) = queue.pop_front() {
        for &to in next.get(from.as_str()).into_iter().flatten() {
            if columns[to] != 0 {
                let col = columns[&from] + 1;
                *columns.get_mut(to).unwrap() = columns[to].max(col);
            }
            let degree = indegree.get_mut(to).unwrap();
            *degree -= 1;
            if *degree == 0 {
                queue.push_back(to.to_owned());
            }
        }
    }
    let mut occupied = nodes
        .iter()
        .filter_map(|n| Some((n.x?, n.y?, height(&entities[&n.entity_id]))))
        .collect::<Vec<_>>();
    for n in nodes.iter_mut().filter(|n| n.x.is_none() || n.y.is_none()) {
        let mut x = n.x.unwrap_or(40. + columns[&n.entity_id] as f64 * 470.);
        let mut y = n.y.unwrap_or(64.);
        let h = height(&entities[&n.entity_id]);
        while let Some(&(ox, oy, oh)) = occupied
            .iter()
            .find(|&&(ox, oy, oh)| (ox - x).abs() < 320. && y < oy + oh + 24. && oy < y + h + 24.)
        {
            if n.y.is_some() {
                x = ox + 470.;
            } else {
                y = oy + oh + 28.;
            }
        }
        n.x = Some(x);
        n.y = Some(y);
        occupied.push((x, y, h));
    }
}
