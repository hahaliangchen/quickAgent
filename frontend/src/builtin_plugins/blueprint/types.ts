export const entityKinds = ['trigger_ui', 'trigger_api', 'trigger_job', 'service', 'store', 'external', 'test_guard'] as const
export const entryKinds = ['ui_page', 'service_api', 'background_job', 'cli_command', 'custom'] as const
export const projectKinds = ['fullstack', 'frontend', 'backend', 'cli', 'library'] as const
export const portKinds = ['event', 'method', 'route', 'tool', 'query', 'mutation', 'state', 'guard'] as const
export const directions = ['in', 'out', 'both'] as const
export const relations = ['triggers', 'http', 'calls', 'reads', 'writes', 'emits', 'must_sync', 'guards'] as const
export const severities = ['info', 'warning', 'critical'] as const
export type EntityKind = typeof entityKinds[number]
export type EntryKind = typeof entryKinds[number]
export type ProjectKind = typeof projectKinds[number]
export type Port = { id: string; name: string; direction: typeof directions[number]; kind: typeof portKinds[number]; symbol: string | null; line_start: number | null; line_end: number | null; summary: string }
export type Entity = { id: string; name: string; kind: EntityKind; traits: string[]; file_path: string | null; summary: string; ports: Port[] }
export type GraphSummary = { id: string; title: string; entry_kind: EntryKind; entry_summary: string }
export type GraphNode = { entity_id: string; x?: number; y?: number }
export type GraphEdge = { id: string; source_entity: string; source_port: string; target_entity: string; target_port: string; relation: typeof relations[number]; label: string; invariant: string | null; severity: typeof severities[number] }
export type Graph = GraphSummary & { nodes: GraphNode[]; edges: GraphEdge[]; invariants: string[] }
export type PlaybookSummary = {
  id: string
  title: string
  trigger: string
  summary: string
  anchor_count: number
  updated_at: string
}
export type Playbook = {
  id: string
  title: string
  trigger: string
  summary: string
  code: string
  anchor_files: string[]
  related_entities: string[]
  related_graphs: string[]
  verify_commands: string[]
  environment?: string[]
  pitfalls: string[]
  source_tasks: string[]
  updated_at: string
}
export type PlaybookUpdate = Partial<Omit<Playbook, 'updated_at'>> & {
  project_kind?: ProjectKind
  last_synced_commit?: string
}
export type PlaybookStatus = {
  status: 'ok' | 'drifted' | 'missing'
  missing_files: string[]
  changed_files: string[]
}
export type PlaybookView = {
  playbook: Playbook
  status: PlaybookStatus
}
export type Manifest = { version: number; project_kind: ProjectKind; last_synced_commit: string | null; updated_at: string; graphs: GraphSummary[]; playbooks?: PlaybookSummary[] }
export type Anchor = { status: 'ok' | 'drifted' | 'missing'; suggested_line?: number }
export type EntityStatus = { status: Anchor['status']; ports: Record<string, Anchor> }
export type Verify = {
  exists: boolean
  head_commit: string | null
  last_synced_commit: string | null
  changed_files: string[]
  affected_entities: string[]
  affected_graphs: string[]
  affected_playbooks?: string[]
  ok_ports: number
  drifted_ports: number
  missing_ports: number
  ok_playbooks?: number
  drifted_playbooks?: number
  missing_playbooks?: number
  stale: boolean
  entities: Record<string, EntityStatus>
  playbooks?: Record<string, PlaybookStatus>
}
export type BlueprintState = { exists: boolean; manifest: Manifest | null; graphs: GraphSummary[]; playbooks?: PlaybookSummary[]; entities: Entity[]; references: Record<string, string[]>; verify: Verify }
export type GraphView = { graph: Graph; entities: Entity[]; statuses: Record<string, EntityStatus> }
export type Reach = { graph_id: string; entity_id: string; port_id: string; depth: number; edge: GraphEdge }
export type Impact = { matches: { entity: Entity; graphs: string[] }[]; graphs: string[]; upstream: Reach[]; downstream: Reach[]; invariants: { text: string; severity: GraphEdge['severity'] }[]; test_guards: Entity[] }
export type Source = { file_path: string; line_start: number; line_end: number; lines: { line: number; text: string }[] }
export type BlueprintProjectStatus = {
  workspace_root: string
  exists: boolean
  status: 'unbuilt' | 'synced' | 'stale'
  project_kind: ProjectKind | null
  graph_count: number
  playbook_count?: number
  entity_count: number
  updated_at: string | null
  last_synced_commit: string | null
  head_commit: string | null
  drifted_ports: number
  missing_ports: number
  drifted_playbooks?: number
  missing_playbooks?: number
  changed_files: number
  stale: boolean
  error?: string
}
export const entryLabels: Record<EntryKind, string> = { ui_page: '页面交互', service_api: '纯接口', background_job: '后台任务', cli_command: '命令行', custom: '自定义入口' }
export const entityLabels: Record<EntityKind, string> = { trigger_ui: '页面触发', trigger_api: '接口触发', trigger_job: '任务触发', service: '服务', store: '存储', external: '外部系统', test_guard: '测试守卫' }
export const kindColors: Record<EntityKind, string> = { trigger_ui: 'var(--bp-ui)', trigger_api: 'var(--bp-api)', trigger_job: 'var(--bp-job)', service: 'var(--bp-service)', store: 'var(--bp-store)', external: 'var(--bp-external)', test_guard: 'var(--bp-test)' }

