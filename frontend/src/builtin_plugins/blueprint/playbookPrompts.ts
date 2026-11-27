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
    '1. 先对每个目标 `task_id` 调用 `blueprint.inspect_session({ task_id })` 获取 SQLite 全景档案（原始任务与红线、计划与钉住事实、本地/远程改动文件、高频踩坑聚类 `failure_clusters`、环境线索 `environment_signals`、最终跑通命令 `winning_commands`）；仅在需要下钻某条关键步骤或原始 JS 编排代码时，再按 `event_seq` 调用 `session.read_session_history`。',
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

export function buildMergeObserverSubmissionPrompt(
  submission: ObserverSubmission,
  existingPlaybooks: readonly Pick<PlaybookSummary, 'id' | 'title'>[] = [],
  existingGraphs: readonly Pick<GraphSummary, 'id' | 'title'>[] = [],
): string {
  const lines: string[] = [
    `请审核并合入观察者从实战会话呈报上来的战地蓝图线索（「${submission.title}」）：`,
    '',
    '观察者呈报内容（作为战场来源数据，先对照当前源码核实再写入）：',
    '```json',
    JSON.stringify(
      {
        submission_id: submission.id,
        source_task_id: submission.source_task_id,
        source_task_title: submission.source_task_title,
        title: submission.title,
        trigger: submission.trigger,
        summary: submission.summary,
        anchor_files: submission.anchor_files,
        environment: submission.environment,
        pitfalls: submission.pitfalls,
        verify_commands: submission.verify_commands,
      },
      null,
      2,
    ),
    '```',
  ]
  if (existingPlaybooks.length > 0) {
    lines.push(
      '',
      '当前已有最短路径（主题相同则复用对应 ID 增量更新，避免重复建条目）：',
      ...existingPlaybooks.map(p => `- \`${p.id}\`：${p.title}`),
    )
  }
  if (existingGraphs.length > 0) {
    lines.push(
      '',
      '当前已有架构画布：',
      ...existingGraphs.map(g => `- \`${g.id}\`：${g.title}`),
    )
  }
  lines.push(
    '',
    '处理要求：',
    `1. 先调用 \`blueprint.overview({})\`，必要时结合 \`blueprint.inspect_session({ task_id: "${submission.source_task_id}" })\` 和当前工作区源码核实呈报中的文件锚点、环境约束、避坑点与验收命令是否真实有效。`,
    '2. 将核实后的战地经验整理并写入对应最短路径（`blueprint.upsert_playbook`，补充 `source_tasks`、最小可执行编排代码、`anchor_files`、`environment`、`pitfalls`、`verify_commands` 与最新 `last_synced_commit`）。',
    '3. 若该线索涉及新的跨层耦合、核心实体或红线不变量，同步调用 `blueprint.upsert_entities` / `blueprint.upsert_graph` 更新架构画布。',
  )
  return lines.join('\n')
}
