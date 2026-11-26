import type { Playbook, PlaybookStatus, PlaybookSummary } from './types.ts'

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

export function buildDistillPlaybookPrompt(
  tasks: readonly DistillTaskRef[],
  existingPlaybooks: readonly Pick<PlaybookSummary, 'id' | 'title'>[] = [],
  focusNote = '',
): string {
  const lines: string[] = [
    '请把下面历史会话里的真实编排流程，提炼成当前项目的「代码蓝图最短路径（Playbook）」：',
    '',
    '目标历史会话：',
    ...tasks.map(t => `- \`${t.task_id}\`：${(t.title ?? '').trim() || '未命名会话'}`),
  ]
  if (focusNote.trim()) {
    lines.push('', `补充关注点：${focusNote.trim()}`)
  }
  if (existingPlaybooks.length > 0) {
    lines.push(
      '',
      '当前项目已有最短路径（如主题相同请直接复用对应 ID 覆盖更新，避免重复）：',
      ...existingPlaybooks.map(p => `- \`${p.id}\`：${p.title}`),
    )
  }
  lines.push(
    '',
    '执行要求：',
    '1. 调用 `session.read_session_history`（以及必要时 `session.list_saved_flows`）阅读上述会话中的流程代码与最终有效结果。',
    '2. **剔除弯路、复盘意图**：删掉当时的盲搜、试错、编译报错重试和已经过期的旧路径；即便某些历史编排代码已过期，也请先理解它当时解决的问题，再对照**当前工作区最新源码**核实有效的文件、符号和命令。',
    '3. 调用 `blueprint.overview({})` 了解已有画布与实体；提炼出最小可复用的最短路径编排代码（清晰可读的 JS / 伪代码流程），列出关键 `anchor_files`（工作区相对路径）、`environment`（已验证的环境事实：端口、venv/解释器路径、远程目录、生产/测试隔离边界）、`pitfalls`（当时踩过的坑/红线）、`verify_commands`（最短验收命令）、`related_graphs` / `related_entities` 与 `source_tasks`。这些 `environment`、`pitfalls` 与 `verify_commands` 会在后续每次会话执行时自动挂载在 AI 上下文尾部持续提醒。',
    '4. 调用 `blueprint.upsert_playbook` 保存这份最短路径，并传入当前最新 git `head_commit` 作为 `last_synced_commit`。',
    '5. 如果这次历史会话暴露了画布上尚未记录的跨层耦合、入口或红线不变量，请顺手用 `blueprint.upsert_entities` / `blueprint.upsert_graph` 补齐架构图。',
  )
  return lines.join('\n')
}

export function buildRunPlaybookPrompt(playbook: Playbook, status?: PlaybookStatus): string {
  const lines = [
    `请直接复用架构蓝图中的最短路径 \`${playbook.id}\`（「${playbook.title}」）来开展接下来的工作，避免盲目从头搜索：`,
    '',
    `- 适用场景：${playbook.trigger || '见代码注释'}`,
    `- 摘要：${playbook.summary || '按最短路径执行'}`,
  ]
  if (status && status.status !== 'ok') {
    const affected = [...status.missing_files, ...status.changed_files]
    lines.push(
      `- ⚠️ 注意：当前蓝图校验状态为 \`${status.status}\`${affected.length ? `（涉及变动/缺失文件：${affected.join('、')}）` : ''}。请先对照当前源码核实这些锚点，把原最短路径当作意图参考，完成后顺手用 \`blueprint.upsert_playbook\` 更新它。`,
    )
  } else {
    lines.push('- 状态：锚点已同步，可直接调用 `blueprint.read_playbook({ playbook_id: "' + playbook.id + '" })` 并按其中的最短路径与验收命令执行。')
  }
  lines.push('', '请先确认本次具体要改动/验证的目标，然后直接沿最短路径推进。')
  return lines.join('\n')
}

export function buildSyncPlaybookPrompt(playbook: Playbook, status?: PlaybookStatus): string {
  const affected = status ? [...status.missing_files, ...status.changed_files] : []
  return [
    `请校验并刷新代码蓝图最短路径 \`${playbook.id}\`（「${playbook.title}」）：`,
    `- 先调用 \`blueprint.read_playbook({ playbook_id: "${playbook.id}" })\` 和 \`blueprint.verify_sync({})\`。`,
    affected.length
      ? `- 重点检查已变更/缺失的锚点文件：${affected.map(f => `\`${f}\``).join('、')}。`
      : '- 对照当前最新源码核实所有 `anchor_files`、编排代码步骤和 `verify_commands` 是否仍然准确有效。',
    '- 用 `blueprint.upsert_playbook` 局部更新过期的路径、锚点与避坑提示，并传入最新 `head_commit` 作为 `last_synced_commit`。',
  ].join('\n')
}
