use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

macro_rules! kind {
    ($name:ident, $first:ident $(, $other:ident)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub(super) enum $name { #[default] $first, $($other),* }
    }
}
kind!(ProjectKind, Fullstack, Frontend, Backend, Cli, Library);
kind!(
    EntryKind,
    UiPage,
    ServiceApi,
    BackgroundJob,
    CliCommand,
    Custom
);
kind!(
    EntityKind, Service, TriggerUi, TriggerApi, TriggerJob, Store, External, TestGuard
);
kind!(Direction, Both, In, Out);
kind!(
    PortKind, Method, Event, Route, Tool, Query, Mutation, State, Guard
);
kind!(
    Relation, Calls, Triggers, Http, Reads, Writes, Emits, MustSync, Guards
);
kind!(Severity, Info, Warning, Critical);
kind!(PortStatus, Ok, Drifted, Missing);

pub(super) fn graph_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid graph ID"
    );
    Ok(())
}
pub(super) fn item_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "invalid entity, port or edge ID"
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub version: u32,
    pub project_kind: ProjectKind,
    pub last_synced_commit: Option<String>,
    pub updated_at: String,
    pub graphs: Vec<GraphSummary>,
    #[serde(default)]
    pub playbooks: Vec<PlaybookSummary>,
}
impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: 1,
            project_kind: ProjectKind::default(),
            last_synced_commit: None,
            updated_at: String::new(),
            graphs: Vec::new(),
            playbooks: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PlaybookSummary {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub anchor_count: usize,
    #[serde(default)]
    pub updated_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Playbook {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub anchor_files: Vec<String>,
    #[serde(default)]
    pub related_entities: Vec<String>,
    #[serde(default)]
    pub related_graphs: Vec<String>,
    #[serde(default)]
    pub verify_commands: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub pitfalls: Vec<String>,
    #[serde(default)]
    pub source_tasks: Vec<String>,
    #[serde(default)]
    pub updated_at: String,
}
impl Playbook {
    pub(super) fn summary_entry(&self) -> PlaybookSummary {
        PlaybookSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            trigger: self.trigger.clone(),
            summary: self.summary.clone(),
            anchor_count: self.anchor_files.len(),
            updated_at: self.updated_at.clone(),
        }
    }
}
#[derive(Debug, Default, Deserialize)]
pub(super) struct PlaybookUpdate {
    pub id: Option<String>,
    pub title: Option<String>,
    pub trigger: Option<String>,
    pub summary: Option<String>,
    pub code: Option<String>,
    pub anchor_files: Option<Vec<String>>,
    pub related_entities: Option<Vec<String>>,
    pub related_graphs: Option<Vec<String>>,
    pub verify_commands: Option<Vec<String>>,
    pub environment: Option<Vec<String>>,
    pub pitfalls: Option<Vec<String>>,
    pub source_tasks: Option<Vec<String>>,
    pub project_kind: Option<ProjectKind>,
    pub last_synced_commit: Option<String>,
}
#[derive(Debug, Default, Clone, Serialize)]
pub(super) struct PlaybookStatus {
    pub status: PortStatus,
    pub missing_files: Vec<String>,
    pub changed_files: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GraphSummary {
    pub id: String,
    pub title: String,
    pub entry_kind: EntryKind,
    #[serde(default)]
    pub entry_summary: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct EntityStore {
    pub entities: Vec<Entity>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Entity {
    pub id: String,
    pub name: String,
    pub kind: EntityKind,
    #[serde(default)]
    pub traits: Vec<String>,
    pub file_path: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub ports: Vec<Port>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Port {
    pub id: String,
    pub name: String,
    pub direction: Direction,
    pub kind: PortKind,
    pub symbol: Option<String>,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
    #[serde(default)]
    pub summary: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BlueprintGraph {
    #[serde(flatten)]
    pub meta: GraphSummary,
    #[serde(default)]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<BlueprintEdge>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GraphNode {
    pub entity_id: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BlueprintEdge {
    pub id: String,
    pub source_entity: String,
    pub source_port: String,
    pub target_entity: String,
    pub target_port: String,
    pub relation: Relation,
    #[serde(default)]
    pub label: String,
    pub invariant: Option<String>,
    #[serde(default)]
    pub severity: Severity,
}
#[derive(Debug, Default, Deserialize)]
pub(super) struct EntityUpdate {
    #[serde(default)]
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub remove_entity_ids: Vec<String>,
    #[serde(default)]
    pub replace_ports: bool,
    pub project_kind: Option<ProjectKind>,
    pub last_synced_commit: Option<String>,
}
#[derive(Debug, Default, Deserialize)]
pub(super) struct GraphUpdate {
    pub id: Option<String>,
    pub title: Option<String>,
    pub entry_kind: Option<EntryKind>,
    pub entry_summary: Option<String>,
    pub invariants: Option<Vec<String>>,
    pub nodes: Option<Vec<GraphNode>>,
    pub edges: Option<Vec<BlueprintEdge>>,
    pub project_kind: Option<ProjectKind>,
    pub last_synced_commit: Option<String>,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct Anchor {
    pub status: PortStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_line: Option<usize>,
}
#[derive(Debug, Default, Serialize)]
pub(super) struct EntityStatus {
    pub status: PortStatus,
    pub ports: BTreeMap<String, Anchor>,
}
#[derive(Debug, Default, Serialize)]
pub(super) struct VerifyReport {
    pub exists: bool,
    pub head_commit: Option<String>,
    pub last_synced_commit: Option<String>,
    pub changed_files: Vec<String>,
    pub affected_entities: Vec<String>,
    pub affected_graphs: Vec<String>,
    pub affected_playbooks: Vec<String>,
    pub ok_ports: usize,
    pub drifted_ports: usize,
    pub missing_ports: usize,
    pub ok_playbooks: usize,
    pub drifted_playbooks: usize,
    pub missing_playbooks: usize,
    pub stale: bool,
    pub entities: BTreeMap<String, EntityStatus>,
    pub playbooks: BTreeMap<String, PlaybookStatus>,
}

kind!(ObserverSubmissionStatus, Pending, Merged, Dismissed);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ObserverSubmission {
    pub id: String,
    pub source_task_id: String,
    #[serde(default)]
    pub source_task_title: String,
    #[serde(default)]
    pub observer_task_id: Option<String>,
    #[serde(default)]
    pub source_seq: Option<i64>,
    pub title: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub anchor_files: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub pitfalls: Vec<String>,
    #[serde(default)]
    pub verify_commands: Vec<String>,
    #[serde(default)]
    pub status: ObserverSubmissionStatus,
    #[serde(default)]
    pub merged_playbook_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct ObserverInboxStore {
    #[serde(default)]
    pub submissions: Vec<ObserverSubmission>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ObserverSubmissionCreate {
    pub id: Option<String>,
    pub source_task_id: String,
    #[serde(default)]
    pub source_task_title: String,
    #[serde(default)]
    pub observer_task_id: Option<String>,
    #[serde(default)]
    pub source_seq: Option<i64>,
    pub title: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub anchor_files: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub pitfalls: Vec<String>,
    #[serde(default)]
    pub verify_commands: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ObserverSubmissionPatch {
    pub status: ObserverSubmissionStatus,
    #[serde(default)]
    pub merged_playbook_id: Option<String>,
}
