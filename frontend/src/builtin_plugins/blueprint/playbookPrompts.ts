import type { GraphSummary, ObserverSubmission, Playbook, PlaybookStatus, PlaybookSummary } from './types.ts'

export interface DistillTaskRef {
  readonly task_id: string
  readonly title?: string | null
  readonly workspace?: string | null
  readonly status?: string | null
  readonly updated_at?: number | null
}

function normalizePath(path: string | null | undefined): string {
  return (path ?? '').replace(/\\/g, '/').replace(/\/+$/, '')
}

export function filterWorkspaceSessions<T extends DistillTaskRef>(
  tasks: readonly T[],
  workspaceRoot: string,
  query = '',
  includeAllWorkspaces = false,
): T[] {
  const target = normalizePath(workspaceRoot)
  const q = query.trim().toLowerCase()
  return [...tasks]
    .filter(task => {
      if (!includeAllWorkspaces && target && normalizePath(task.workspace) !== target) return false
      if (!q) return true
      const hay = `${task.title ?? ''} ${task.task_id} ${task.workspace ?? ''}`.toLowerCase()
      return hay.includes(q)
    })
    .sort((a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0))
}

/**
 * Concise factual trigger for Blueprint's own `kind: "blueprint"` Agent.
 * All distillation methodology lives in `src/server/builtin_plugins/blueprint/SKILL.md`.
 */
export function buildDistillPlaybookPrompt(
  tasks: readonly DistillTaskRef[],
  existingPlaybooks: readonly Pick<PlaybookSummary, 'id' | 'title'>[] = [],
  focusNote = '',
): string {
  const refs = tasks.map(t => `${t.task_id}（${(t.title ?? '').trim() || '未命名会话'}）`).join('、')
  const parts = [`从历史会话提炼最短路径蓝图：${refs}。`]
  if (focusNote.trim()) parts.push(`关注点：${focusNote.trim()}。`)
  if (existingPlaybooks.length > 0) {
    parts.push(`已有路径：${existingPlaybooks.map(p => `${p.id}（${p.title}）`).join('、')}。`)
  }
  return parts.join('\n')
}

export function buildRunPlaybookPrompt(playbook: Playbook, status?: PlaybookStatus): string {
  const parts = [`按最短路径 \`${playbook.id}\`（「${playbook.title}」）执行。`]
  if (status && status.status !== 'ok') {
    const affected = [...status.missing_files, ...status.changed_files]
    parts.push(`当前锚点状态：${status.status}${affected.length ? `（${affected.join('、')}）` : ''}，请先核实变动。`)
  }
  return parts.join('\n')
}

export function buildSyncPlaybookPrompt(playbook: Playbook, status?: PlaybookStatus): string {
  const affected = status ? [...status.missing_files, ...status.changed_files] : []
  return affected.length
    ? `校验并刷新最短路径 \`${playbook.id}\`（「${playbook.title}」），重点核实：${affected.join('、')}。`
    : `校验并刷新最短路径 \`${playbook.id}\`（「${playbook.title}」）。`
}

/**
 * Concise factual trigger for Blueprint's own `kind: "blueprint"` Agent.
 * Merge workflow lives in `src/server/builtin_plugins/blueprint/SKILL.md`.
 */
export function buildMergeObserverSubmissionPrompt(
  submission: ObserverSubmission,
  existingPlaybooks: readonly Pick<PlaybookSummary, 'id' | 'title'>[] = [],
  existingGraphs: readonly Pick<GraphSummary, 'id' | 'title'>[] = [],
): string {
  const parts = [
    `审核并合入观察者战地呈报 \`${submission.id}\`（「${submission.title}」，来源会话 \`${submission.source_task_id}\`）。`,
  ]
  if (existingPlaybooks.length > 0) {
    parts.push(`已有路径：${existingPlaybooks.map(p => `${p.id}（${p.title}）`).join('、')}。`)
  }
  if (existingGraphs.length > 0) {
    parts.push(`已有画布：${existingGraphs.map(g => `${g.id}（${g.title}）`).join('、')}。`)
  }
  return parts.join('\n')
}
