import type { Task, Project } from './api.ts'

/**
 * Format a task timestamp for the compact sidebar display.
 * Timestamps from the API may be in seconds or milliseconds.
 */
export function compactRelativeTime(ts?: number): string {
  if (typeof ts !== 'number' || !Number.isFinite(ts) || ts <= 0) return ''

  const ms = ts < 1e12 ? ts * 1000 : ts
  if (!Number.isFinite(ms) || ms <= 0) return ''

  const diffSec = Math.max(0, Math.floor((Date.now() - ms) / 1000))
  if (diffSec < 60) return '刚刚'

  const mins = Math.floor(diffSec / 60)
  if (mins < 60) return `${mins}m`

  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h`

  const days = Math.floor(hours / 24)
  if (days === 1) return '昨天'
  if (days < 7) return `${days}d`

  const date = new Date(ms)
  if (Number.isNaN(date.getTime())) return ''
  if (days >= 365) return `${date.getFullYear()}/${date.getMonth() + 1}/${date.getDate()}`
  return `${date.getMonth() + 1}/${date.getDate()}`
}
export function workspaceName(path: string): string {
  return path.replace(/^\\\\\?\\/, '').split(/[\\/]/).filter(Boolean).at(-1) ?? path
}
export function taskTitle(task: Task): string {
  return task.title?.trim() || task.prompt.trim().split('\n', 1)[0] || '新会话'
}
export const isChat = (task: Task): boolean => !!task.plugin_id || task.kind === 'chat'
/** `contentHits`: sessions whose messages (not only titles) match the query. */
export function visibleTasks(tasks: readonly Task[], query: string, archived: boolean, contentHits?: ReadonlySet<string>): Task[] {
  const q = query.trim().toLocaleLowerCase()
  return tasks.filter(task => (!!task.archived === archived)
    && (!!task.prompt.trim() || task.status === 'running' || task.status === 'cancelling')
    && (!q || taskTitle(task).toLocaleLowerCase().includes(q) || !!contentHits?.has(task.task_id)))
    .sort((a, b) => Number(!!b.pinned) - Number(!!a.pinned) || b.updated_at - a.updated_at || a.task_id.localeCompare(b.task_id))
}
export function projectGroups(projects: readonly Project[], tasks: readonly Task[], defaultWorkspace: string) {
  // Registered order is stable; selection never changes it. Identity is the full
  // canonical path, so two real folders with the same basename stay distinct.
  const unique = [...new Map(projects.map(project => [project.path, project])).values()]
  const groups = unique.map(project => ({...project, tasks: tasks.filter(task => !isChat(task) && (task.workspace ?? defaultWorkspace) === project.path)}))
  const paths = new Set(unique.map(project => project.path))
  const ungrouped = tasks.filter(task => !isChat(task) && !paths.has(task.workspace ?? defaultWorkspace))
  return {groups, ungrouped}
}
