import { createContext } from 'react'
import type { Task } from '../../api.ts'
import type { BlueprintProjectStatus, BlueprintState, Entity, Graph, GraphNode, GraphView, Impact, Playbook, PlaybookUpdate, PlaybookView, ProjectKind, Source, Verify } from './types.ts'

export interface BlueprintProjectOption {
  readonly path: string
  readonly title: string
}

export const BlueprintContext = createContext<{
  sendToAi?: (text: string) => void
  refreshSignal?: string | undefined
  projects?: readonly BlueprintProjectOption[]
  selectWorkspace?: (path: string) => void
  sessions?: readonly Task[]
  openSession?: (taskId: string) => void
}>({})

export async function fetchBlueprintProjects(workspaces: readonly string[]): Promise<BlueprintProjectStatus[]> {
  if (!workspaces.length) return []
  const response = await fetch('/api/blueprint/projects', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspaces }),
  })
  const data = await response.json().catch(() => null)
  if (!response.ok) throw new Error(data?.error ?? '加载项目蓝图状态失败（' + response.status + '）')
  return (data?.projects ?? []) as BlueprintProjectStatus[]
}
export function blueprintApi(workspaceRoot: string) {
  async function request<T>(path: string, method = 'GET', body?: unknown, query: Record<string, string> = {}): Promise<T> {
    const params = new URLSearchParams({ workspace_root: workspaceRoot, ...query })
    const response = await fetch('/api/blueprint' + path + '?' + params, {
      method, ...(body === undefined ? {} : { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }),
    })
    const data = await response.json().catch(() => null)
    if (!response.ok) throw new Error(data?.error ?? '蓝图请求失败（' + response.status + '）')
    return data as T
  }
  const graph = (id: string) => '/graph/' + encodeURIComponent(id)
  const playbook = (id: string) => '/playbook/' + encodeURIComponent(id)
  return {
    state: () => request<BlueprintState>('/state'),
    graph: (id: string) => request<GraphView>(graph(id)),
    create: (value: Pick<Graph, 'id' | 'title' | 'entry_kind' | 'entry_summary'> & { project_kind?: ProjectKind }) => request<Graph>('/graph', 'POST', value),
    save: (id: string, value: Partial<Graph>) => request<Graph>(graph(id), 'PUT', value),
    deleteGraph: (id: string) => request(graph(id), 'DELETE'),
    layout: (id: string, nodes: GraphNode[]) => request(graph(id) + '/layout', 'PUT', { nodes }),
    playbook: (id: string) => request<PlaybookView>(playbook(id)),
    createPlaybook: (value: PlaybookUpdate & { id: string }) => request<Playbook>('/playbook', 'POST', value),
    savePlaybook: (id: string, value: PlaybookUpdate) => request<Playbook>(playbook(id), 'PUT', value),
    deletePlaybook: (id: string) => request(playbook(id), 'DELETE'),
    putEntity: (entity: Entity) => request('/entities', 'PUT', { entities: [entity], replace_ports: true }),
    deleteEntity: (id: string) => request('/entities/' + encodeURIComponent(id), 'DELETE'),
    impact: (entity: string, port?: string) => request<Impact>('/impact', 'GET', undefined, { entity_id: entity, ...(port ? { port_id: port } : {}) }),
    verify: () => request<Verify>('/verify'),
    source: (file: string, start: number, end: number) => request<Source>('/source', 'GET', undefined, { file_path: file, line_start: String(start), line_end: String(Math.min(end, start + 299)) }),
  }
}
export type BlueprintApi = ReturnType<typeof blueprintApi>

