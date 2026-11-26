import type { Task } from '../api.ts'
import { isChat, taskTitle, workspaceName } from '../sidebarModel.ts'

export type DockSessionScope = 'all' | 'workspace' | 'chat'

/**
 * Filter and order top-level sessions for the CodeChatDock session switcher.
 * Keeps pinned/running/recent sessions easy to find, includes the active task
 * even if it is still a draft, and never shows internal child subagent tasks.
 */
export function filterDockSessions(
  tasks: readonly Task[],
  options: {
    readonly scope: DockSessionScope
    readonly workspace: string
    readonly query: string
    readonly activeTaskId?: string | undefined
    readonly linkedTaskId?: string | undefined
  },
): Task[] {
  const q = options.query.trim().toLocaleLowerCase()
  const filtered = tasks.filter(task => {
    if (task.parent_task_id !== null || task.archived) return false
    const keepAsCurrent = task.task_id === options.activeTaskId || task.task_id === options.linkedTaskId
    const hasContent = !!task.prompt.trim() || task.status === 'running' || task.status === 'cancelling'
    if (!hasContent && !keepAsCurrent) return false
    if (options.scope === 'chat' && !isChat(task)) return false
    if (options.scope === 'workspace') {
      if (isChat(task)) return false
      if (options.workspace && task.workspace !== options.workspace) return false
    }
    if (q) {
      const title = taskTitle(task).toLocaleLowerCase()
      const ws = task.workspace ? workspaceName(task.workspace).toLocaleLowerCase() : ''
      if (!title.includes(q) && !ws.includes(q) && !task.task_id.toLocaleLowerCase().includes(q)) return false
    }
    return true
  })

  return filtered.sort((a, b) => {
    const aRunning = a.status === 'running' || a.status === 'cancelling'
    const bRunning = b.status === 'running' || b.status === 'cancelling'
    if (aRunning !== bRunning) return Number(bRunning) - Number(aRunning)
    if (!!a.pinned !== !!b.pinned) return Number(!!b.pinned) - Number(!!a.pinned)
    return b.updated_at - a.updated_at || a.task_id.localeCompare(b.task_id)
  })
}
