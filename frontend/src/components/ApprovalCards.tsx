import { useMemo, useState } from 'react'
import type { AgentEvent } from '../api.ts'
import css from './NoteProposal.module.css'

type Decision = 'once' | 'session' | 'project' | 'deny'

interface PendingApproval {
  readonly callId: string
  readonly tool: string
  readonly summary: string
  readonly project: boolean
  readonly workdir: string
  readonly shell: string
  readonly unsandboxed: boolean
}

const TOOL_LABELS: Record<string, string> = {
  exec_command: '运行命令', write_stdin: '向命令会话写入输入或读取输出', kill_session: '终止命令会话',
  run_program: '运行命令', run_project_script: '运行项目脚本', install_dependencies: '安装依赖',
  stop_project_process: '停止项目进程', browser_open: '打开浏览器', browser_click: '在浏览器里点击',
  browser_press_key: '在浏览器里按键', browser_upload: '在浏览器里上传文件', browser_screenshot: '浏览器截图',
  browser_read: '读取浏览器页面', browser_wait: '等待浏览器页面', browser_diagnostics: '读取浏览器诊断', browser_close: '关闭浏览器',
}

function argumentRecord(raw: unknown): Record<string, unknown> | undefined {
  try {
    const value: unknown = typeof raw === 'string' ? JSON.parse(raw) : raw
    return typeof value === 'object' && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : undefined
  } catch {
    return undefined
  }
}

const text = (value: unknown) => typeof value === 'string' ? value : ''

/** The most telling part of the call: a command line, script, URL or path. */
function summarize(args: Record<string, unknown>, fallback: string): string {
  if (typeof args.cmd === 'string') return args.cmd
  const list = (value: unknown) => Array.isArray(value) ? value.map(String).join(' ') : ''
  const program = text(args.program) || text(args.command)
  if (program) return [program, list(args.args)].filter(Boolean).join(' ')
  if (args.session_id !== undefined) {
    return `会话 ${String(args.session_id)}${typeof args.chars === 'string' ? ` · 输入：${JSON.stringify(args.chars)}` : ''}`
  }
  const script = text(args.script) || text(args.name)
  if (script) return [text(args.project) || text(args.cwd), script].filter(Boolean).join(' · ')
  return text(args.url) || text(args.path) || fallback
}

/** Approval requests the agent is waiting on, oldest first. */
export function pendingApprovals(events: readonly AgentEvent[]): PendingApproval[] {
  const decided = new Set(events.filter(event => event.type === 'approval/decision').map(event => String(event.data.call_id)))
  return events
    .filter(event => event.type === 'approval/request' && !decided.has(String(event.data.call_id)))
    .map(event => {
      const raw = String(event.data.arguments ?? '')
      const fullArgs = argumentRecord(event.data.full_arguments)
      const args = { ...argumentRecord(raw), ...fullArgs }
      return {
        callId: String(event.data.call_id),
        tool: String(event.data.tool),
        summary: summarize(args, fullArgs ? JSON.stringify(fullArgs, null, 2) : raw),
        project: typeof event.data.project === 'string',
        workdir: text(args.workdir) || text(args.project_path) || text(args.cwd),
        shell: text(event.data.shell) || text(args.shell),
        unsandboxed: event.data.unsandboxed === true || args.unsandboxed === true,
      }
    })
}

/** "Ask before running" questions, shown at the end of the conversation while the agent waits. */
export function ApprovalCards({ taskId, events, className }: { taskId: string; events: readonly AgentEvent[]; className?: string | undefined }) {
  const pending = useMemo(() => pendingApprovals(events), [events])
  const [sent, setSent] = useState<Record<string, Decision>>({})
  const [error, setError] = useState('')
  if (pending.length === 0) return null
  const decide = (callId: string, decision: Decision) => {
    setError('')
    setSent(current => ({ ...current, [callId]: decision }))
    fetch(`/agent/tasks/${encodeURIComponent(taskId)}/approvals`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ call_id: callId, decision }),
    }).then(async response => {
      if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? response.statusText)
    }).catch((cause: unknown) => {
      setSent(current => { const next = { ...current }; delete next[callId]; return next })
      setError(cause instanceof Error ? cause.message : String(cause))
    })
  }
  return <div className={className}>{pending.map(item => {
    const chosen = sent[item.callId]
    return <div key={item.callId} className={css.card} role="alertdialog" aria-label={`是否允许${TOOL_LABELS[item.tool] ?? item.tool}`}>
      <div className={css.title}>要{TOOL_LABELS[item.tool] ?? `调用 ${item.tool}`}吗？</div>
      {item.unsandboxed && <p className={css.warning}>警告：此命令将在沙箱外执行（无沙箱隔离），可访问当前用户权限范围内的文件和系统资源。</p>}
      {item.shell && <p className={css.hint}>Shell：{item.shell}</p>}
      {item.workdir && <p className={css.hint}>工作目录：{item.workdir}</p>}
      <code className={css.command}>{item.summary}</code>
      {error && <p className={css.error} role="alert">{error}</p>}
      <div className={css.actions}>
        <button type="button" className={css.secondary} disabled={!!chosen} onClick={() => decide(item.callId, 'deny')}>拒绝</button>
        {item.project && <button type="button" className={css.secondary} disabled={!!chosen} onClick={() => decide(item.callId, 'project')}>这个项目以后都允许</button>}
        <button type="button" className={css.secondary} disabled={!!chosen} onClick={() => decide(item.callId, 'session')}>本会话都允许</button>
        <button type="button" className={css.primary} disabled={!!chosen} onClick={() => decide(item.callId, 'once')}>{chosen ? '已提交…' : '允许这一次'}</button>
      </div>
    </div>
  })}</div>
}
