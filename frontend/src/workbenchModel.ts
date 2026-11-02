import type { Project, ProgramStatus, SessionProgram, SessionTurn, SkillPackage, Task } from './api.ts'

const basename = (path: string): string => path.replace(/^\\\\\?\\/, '').split(/[\\/]/).filter(Boolean).at(-1) ?? path
const title = (task: Task): string => task.title?.trim() || task.prompt.trim().split('\n', 1)[0] || '新会话'

/** A project folder by the name the user gave it, else by its folder name. */
export function projectName(path: string, projects: readonly Project[]): string {
  return projects.find(project => project.path === path)?.title || basename(path)
}

export interface SessionGroup {
  readonly key: string
  readonly title: string
  /** Plugin groups say whose they are; project groups carry their folder. */
  readonly detail: string
  readonly tasks: readonly Task[]
}

/**
 * The workbench's session list: code sessions by project, plugin sessions by
 * the plugin that started them, plain chats on their own. Draft conversations
 * (`hidden`) belong to their draft and stay out of the list.
 */
export function sessionGroups(tasks: readonly Task[], projects: readonly Project[], packages: readonly SkillPackage[],
  defaultWorkspace: string, hidden: ReadonlySet<string>, query: string) {
  const q = query.trim().toLocaleLowerCase()
  const pluginTitle = (id: string) => packages.find(item => item.id === id)?.title || id
  const shown = tasks
    .filter(task => !task.parent_task_id && !task.archived && !hidden.has(task.task_id)
      && (!!task.prompt.trim() || task.status === 'running' || task.status === 'cancelling'))
    .filter(task => !q || `${title(task)}\n${task.prompt}\n${task.plugin_id ? pluginTitle(task.plugin_id) : ''}`.toLocaleLowerCase().includes(q))
    .sort((a, b) => Number(!!b.pinned) - Number(!!a.pinned) || b.updated_at - a.updated_at || a.task_id.localeCompare(b.task_id))
  const byProject = new Map<string, Task[]>()
  const byPlugin = new Map<string, Task[]>()
  const chats: Task[] = []
  for (const task of shown) {
    if (task.plugin_id) byPlugin.set(task.plugin_id, [...byPlugin.get(task.plugin_id) ?? [], task])
    else if (task.kind === 'chat') chats.push(task)
    else {
      const path = task.workspace ?? defaultWorkspace
      byProject.set(path, [...byProject.get(path) ?? [], task])
    }
  }
  // Registered projects keep their order; folders that are not registered follow by recency.
  const order = projects.map(project => project.path)
  const projectList: SessionGroup[] = [...byProject]
    .sort(([a, x], [b, y]) => (order.includes(a) ? order.indexOf(a) : order.length) - (order.includes(b) ? order.indexOf(b) : order.length)
      || (y[0]?.updated_at ?? 0) - (x[0]?.updated_at ?? 0))
    .map(([path, rows]) => ({ key: `project:${path}`, title: projectName(path, projects), detail: path, tasks: rows }))
  const pluginList: SessionGroup[] = [...byPlugin].map(([id, rows]) => {
    const origin = packages.find(item => item.id === id)?.origin
    return { key: `plugin:${id}`, title: pluginTitle(id), detail: origin === 'builtin' ? '内置插件' : origin === 'external' ? '外部插件' : '插件', tasks: rows }
  })
  return { projects: projectList, plugins: pluginList, chats }
}

export type Tone = 'ok' | 'error' | 'warn' | 'live' | 'muted'

const STATUS: Record<ProgramStatus, { readonly label: string; readonly tone: Tone; readonly words: string }> = {
  completed: { label: '已完成', tone: 'ok', words: '程序已完成' },
  rewritten: { label: '出错 · 已交回 AI', tone: 'error', words: '程序出错，错误已交回 AI' },
  failed: { label: '出错停止', tone: 'error', words: '程序出错停止' },
  waiting: { label: '等待回答', tone: 'warn', words: '程序停下等用户回答' },
  paused: { label: '已暂停', tone: 'warn', words: '程序被用户的新消息暂停' },
  running: { label: '运行中', tone: 'live', words: '程序正在运行' },
  unknown: { label: '结果未记录', tone: 'muted', words: '会话里没有这段程序的结果记录' },
}

export function statusOf(program: SessionProgram): { readonly label: string; readonly tone: Tone; readonly words: string } {
  const status = STATUS[program.status] ?? STATUS.unknown
  return program.status === 'rewritten' && program.rewritten_by !== null ? { ...status, label: '出错 · 已改写' } : status
}

/** A turn's state is where its last program ended. */
export function turnTone(turn: SessionTurn): Tone {
  const last = turn.programs.at(-1)
  return last === undefined ? 'muted' : statusOf(last).tone
}

/** `ppt_master__apply_html` reads as the method a program calls: `apply_html`. */
export function callName(name: string): string {
  return name.includes('__') ? name.slice(name.lastIndexOf('__') + 2) : name
}

export function duration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000))
  if (seconds < 60) return `${seconds} 秒`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return seconds % 60 ? `${minutes} 分 ${seconds % 60} 秒` : `${minutes} 分`
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分`
}

export function clock(time: number, withDate = true): string {
  const date = new Date(time)
  const pad = (value: number) => String(value).padStart(2, '0')
  const day = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
  return withDate ? `${day} ${pad(date.getHours())}:${pad(date.getMinutes())}` : `${pad(date.getHours())}:${pad(date.getMinutes())}`
}

/**
 * Which turn tabs to show: all of them when they fit, else the first, the last
 * and the ones around the selected turn, with gaps between.
 */
export function turnWindow(count: number, selected: number, size = 7): (number | 'gap')[] {
  if (count <= size) return Array.from({ length: count }, (_, index) => index)
  const side = Math.max(1, Math.floor((size - 3) / 2))
  const start = Math.max(1, Math.min(selected - side, count - 2 - 2 * side))
  const end = Math.min(count - 2, start + 2 * side)
  const window: (number | 'gap')[] = [0]
  if (start > 1) window.push('gap')
  for (let index = start; index <= end; index++) window.push(index)
  if (end < count - 2) window.push('gap')
  window.push(count - 1)
  return window
}

/** The program and where it sits: its turn and its place among that turn's runs. */
export interface ProgramEntry {
  readonly turn: SessionTurn
  readonly program: SessionProgram
  readonly index: number
}

export function programEntries(turns: readonly SessionTurn[]): ProgramEntry[] {
  return turns.flatMap(turn => turn.programs.map((program, index) => ({ turn, program, index: index + 1 })))
}

export const draftFileOf = (seq: number): string => `programs/program-${seq}.js`

/** An AI reply as a short plain-text preview: markdown marks dropped, link text kept. */
export function plainText(markdown: string): string {
  return markdown
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/(\*\*|__)(.+?)\1/g, '$2')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/^#{1,6}\s+/gm, '')
    .replace(/^\s*[-*]\s+/gm, '· ')
}
