/** One-line argument summary for a tool row; kept React-free so it can be unit tested. */

export const text = (value: unknown): string =>
  typeof value === 'string' ? value.replace(/\s+/g, ' ').trim() : ''
const count = (value: unknown): number => Array.isArray(value) ? value.length : 0
/** Host node IDs are long hashes; the leading characters are enough to tell them apart. */
const shortNode = (value: unknown): string =>
  text(value) === 'n0' ? '主任务' : `子任务 ${text(value).slice(0, 7)}`
/** A shell session ID is a 38-character `shell_<uuid>`; the collapsed row only needs its head. */
export const shortSession = (value: unknown): string => {
  const raw = text(value)
  const match = /^shell_([0-9a-f]+)$/i.exec(raw)
  return match?.[1] === undefined ? raw : `shell_${match[1].slice(0, 6)}`
}

/** Durations past a second read better as seconds than as five raw digits beside the exit code. */
export const formatDuration = (ms: number): string =>
  ms >= 1000 ? `${Number((ms / 1000).toFixed(1))} s` : `${ms} ms`

/** A polled session that reported an exit code, a stop reason, an error text or a stop is not still waiting. */
function settled(result: unknown, stopped: boolean): boolean {
  if (stopped) return true
  if (result === undefined) return false
  // A plain error string is a finished call, never a live poll.
  if (typeof result !== 'object' || result === null) return true
  const record = result as Record<string, unknown>
  if (typeof record.exit_code === 'number' || typeof record.termination_reason === 'string') return true
  return record.running === false
}

/**
 * The text a tool row shows beside its title.
 *
 * @param name - the host tool name.
 * @param args - its parsed arguments.
 * @param result - its parsed result when the call has settled.
 * @param options.stopped - the turn ended without this call producing a result.
 * @returns the summary, or undefined when the row's generic argument summary fits.
 */
export function summaryFor(name: string, args: Record<string, unknown>, result?: unknown, options?: { stopped?: boolean }): string | undefined {
  switch (name) {
    case 'web__search': return text(args.query)
    case 'web__fetch': return `${args.cursor ? '续读 · ' : ''}${text(args.url)}`
    case 'browser_fill': return text(args.selector)
    case 'browser_set_viewport': return [typeof args.width === 'number' && typeof args.height === 'number' ? String(args.width) + ' × ' + String(args.height) : '', text(args.color_scheme)].filter(Boolean).join(' · ')
    case 'browser_scroll': return text(args.selector) || [args.x, args.y].map((v, i) => typeof v === 'number' ? (i === 0 ? 'x=' : 'y=') + v : '').filter(Boolean).join(' · ')
    case 'browser_screenshot': return text(args.selector) || (args.full_page === true ? '整页' : '当前视口')
    case 'verify_check': return [text(args.path) || '.', text(args.ecosystem), text(args.target)].filter(Boolean).join(' · ')
    case 'verify_test': return [text(args.path) || '.', text(args.ecosystem), text(args.filter)].filter(Boolean).join(' · ')
    case 'plan_set': return String(count(args.steps)) + ' 项'
    case 'plan_update': return [text(args.id), ({ pending: '待开始', in_progress: '进行中', done: '完成', blocked: '受阻' } as Record<string, string>)[text(args.status)], text(args.note)].filter(Boolean).join(' · ')
    case 'plan_add_substeps': return [text(args.parent_id), String(count(args.steps)) + ' 项子步骤'].filter(Boolean).join(' · ')
    case 'exec_command': return text(args.cmd)
    case 'run_readonly_command': return text(args.cmd)
    case 'create_dir': return text(args.path)
    case 'delete_path': return [text(args.path), args.recursive === true ? '整个目录' : ''].filter(Boolean).join(' · ')
    case 'rename_path': return [text(args.path), text(args.to) && `→ ${text(args.to)}`].filter(Boolean).join(' ')
    case 'git_blame': return [text(args.path), typeof args.start_line === 'number' && typeof args.end_line === 'number' ? 'L' + args.start_line + '–' + args.end_line : ''].filter(Boolean).join(' · ')
    case 'git_commit': return [String(count(args.paths)) + ' 个文件', text(args.message)].filter(Boolean).join(' · ')
    case 'git_restore': return String(count(args.paths)) + ' 个文件 · ' + (args.staged === true ? '暂存区' : '工作区')
    case 'apply_edits': return String(count(args.files)) + ' 个文件' + (args.atomic === false ? ' · 逐文件尝试' : '')
    case 'git_status': return [text(args.repo), Array.isArray(args.paths) && args.paths.length > 0 ? '路径：' + args.paths.map(value => text(value)).filter(Boolean).join('、') : '工作区状态'].filter(Boolean).join(' · ')
    case 'git_diff': return [text(args.repo), args.check === true ? '空白检查' : '', args.stat_only === true ? '变更统计' : [text(args.rev_range), Array.isArray(args.paths) && args.paths.length > 0 ? args.paths.map(value => text(value)).filter(Boolean).join('、') : '工作区变更'].filter(Boolean).join(' · ')].filter(Boolean).join(' · ')
    case 'git_log': return [text(args.repo), text(args.rev), Array.isArray(args.paths) && args.paths.length > 0 ? args.paths.map(value => text(value)).filter(Boolean).join('、') : '提交历史'].filter(Boolean).join(' · ')
    case 'http_request': return [text(args.method) || 'GET', text(args.url)].filter(Boolean).join(' · ')
    case 'stat_paths': return Array.isArray(args.paths) ? String(args.paths.length) + ' 个路径' : text(args.path)
    case 'copy_path': return [text(args.from), text(args.to) && '→ ' + text(args.to)].filter(Boolean).join(' ')
    case 'sys_disk_space': return text(args.path) || '系统盘空间'
    case 'sys_which': return Array.isArray(args.names) ? args.names.map(value => text(value)).filter(Boolean).join('、') : ''
    case 'sys_listening_ports': return typeof args.port === 'number' ? '端口 ' + args.port : '监听端口'
    case 'sys_processes': return text(args.filter) || '进程列表'
    case 'read_git': return [text(args.view), text(args.path ?? args.file ?? args.ref)].filter(Boolean).join(' · ') || 'Git 证据'
    // A finished session is reported by the right-hand exit status; calling it
    // "waiting for output" beside that status contradicts it.
    case 'write_stdin': {
      const typed = typeof args.chars === 'string' && args.chars.length > 0 ? JSON.stringify(args.chars) : undefined
      const outcome = settled(result, options?.stopped === true) ? '拉取输出' : '等待输出'
      return `会话 ${shortSession(args.session_id)} · ${typed ?? outcome}`
    }
    case 'kill_session': return `会话 ${shortSession(args.session_id)}`
    case 'create_task': return [text(args.goal), text(args.reason) && `原因：${text(args.reason)}`].filter(Boolean).join(' · ')
    case 'create_step': return [text(args.result) && `完成：${text(args.result)}`, text(args.next) && `下一步：${text(args.next)}`].filter(Boolean).join(' · ')
    case 'handoff_step': {
      const situation = text(args.situation ?? args.completed)
      const sliceCount = Array.isArray(args.slices) ? args.slices.length : 0
      return [
        text(args.step_name) && `完成 ${text(args.step_name)}`,
        situation && `当前情形：${situation}`,
        text(args.next_step) && `进入 ${text(args.next_step)}`,
        text(args.next_goal) && `下一轮目的：${text(args.next_goal)}`,
        sliceCount > 0 ? `截取 ${sliceCount} 段上下文` : '',
      ].filter(Boolean).join(' · ')
    }
    case 'set_next_step': return [text(args.current_step), text(args.next_step) && `下一步：${text(args.next_step)}`].filter(Boolean).join(' · ')
    case 'plan_tasks': {
      const tasks = Array.isArray(args.tasks) ? args.tasks : []
      const titles = tasks.map(task => text((task as Record<string, unknown> | null)?.title)).filter(Boolean)
      return `${tasks.length} 项${titles.length > 0 ? `：${titles.join('、')}` : ''}`
    }
    case 'start_task': return text(args.briefing) || text(args.node_id)
    case 'branch_task': return [text(args.title), text(args.reason) && `原因：${text(args.reason)}`].filter(Boolean).join(' · ')
    case 'return_task': return text(args.status === 'needs_user' ? args.question : args.result)
    case 'split_task': return `${text(args.title)}${count(args.tasks) > 0 ? ` · 剩余 ${count(args.tasks)} 项` : ''}`
    case 'scope_message': return `作用于${shortNode(args.node_id)}`
    case 'update_task_definition': return text(args.objective)
    case 'spawn_agent': return text(args.name) || text(args.task)
    case 'compress_context': return text(args.reason)
    default: return undefined
  }
}
