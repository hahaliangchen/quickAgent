import { useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import type { AgentEvent } from '../api.ts'
import css from './ApprovalCards.module.css'

type Decision = 'once' | 'session' | 'project' | 'deny'
type AllowScope = 'once' | 'session' | 'project'

interface PendingApproval {
  readonly callId: string
  readonly tool: string
  readonly summary: string
  readonly project: boolean
  readonly onceOnly: boolean
  readonly workdir: string
  readonly shell: string
  readonly unsandboxed: boolean
  readonly outside: readonly string[]
  readonly outsideWrite: boolean
  readonly reviewReason: string
}

const TOOL_LABELS: Record<string, string> = {
  exec_command: '运行命令', write_stdin: '向命令会话写入输入或读取输出', kill_session: '终止命令会话',
  run_program: '运行命令', run_project_script: '运行项目脚本', install_dependencies: '安装依赖',
  stop_project_process: '停止项目进程', browser_open: '打开浏览器', browser_click: '在浏览器里点击',
  browser_press_key: '在浏览器里按键', browser_upload: '在浏览器里上传文件', browser_screenshot: '浏览器截图',
  browser_read: '读取浏览器页面', browser_wait: '等待浏览器页面', browser_diagnostics: '读取浏览器诊断', browser_close: '关闭浏览器',
  write_file: '写入工作区外的文件', edit_file: '修改工作区外的文件', replace_range: '修改工作区外的文件',
  sync_git: '同步 Git 仓库', read_git: '读取工作区外的 Git 仓库', blueprint__overview: '读取工作区外的架构蓝图', blueprint__read_graph: '读取工作区外的蓝图画布', blueprint__impact: '查询工作区外的蓝图影响面', blueprint__verify_sync: '校验工作区外的架构蓝图',
  workspace_info: '读取工作区外的信息',
  code_index__index: '索引工作区外的代码', code_index__status: '读取工作区外的索引状态',
  code_index__list: '列出工作区外的符号', code_index__search: '搜索工作区外的符号', code_index__read: '读取工作区外的符号',
  index_rust_workspace: '索引工作区外的代码', index_ts_workspace: '索引工作区外的代码',
  index_go_workspace: '索引工作区外的代码', index_python_workspace: '索引工作区外的代码',
  list_rust_symbols: '列出工作区外的符号', list_ts_symbols: '列出工作区外的符号',
  list_go_symbols: '列出工作区外的符号', list_python_symbols: '列出工作区外的符号',
  search_rust_symbols: '搜索工作区外的符号', search_ts_symbols: '搜索工作区外的符号',
  search_go_symbols: '搜索工作区外的符号', search_python_symbols: '搜索工作区外的符号',
  rust_index_status: '读取工作区外的索引状态', ts_index_status: '读取工作区外的索引状态',
  go_index_status: '读取工作区外的索引状态', python_index_status: '读取工作区外的索引状态',
  read_file: '读取工作区外的文件', read_file_lines: '读取工作区外的文件', list_dir: '查看工作区外的目录', search_text: '在工作区外搜索',
  read_rust_symbol: '读取工作区外的代码', read_ts_symbol: '读取工作区外的代码', read_go_symbol: '读取工作区外的代码', read_python_symbol: '读取工作区外的代码',
}

const SCOPE_META: Readonly<Record<AllowScope, { label: string; desc: string; button: string }>> = {
  once: { label: '仅允许这一次', desc: '仅放行当前这一项操作', button: '允许这一次' },
  session: { label: '本会话都允许', desc: '当前对话同类操作不再询问', button: '本会话允许' },
  project: { label: '该项目都允许', desc: '记住此项目的该类权限', button: '该项目允许' },
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
  if (script) return [text(args.project) || text(args.cwd) || text(args.project_path), script].filter(Boolean).join(' · ')
  const pathWithRoot = [text(args.workspace_root), text(args.path)].filter(Boolean).join(' · ')
  return text(args.url) || pathWithRoot || fallback
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
        onceOnly: event.data.once_only === true,
        workdir: text(args.workdir) || text(args.project_path) || text(args.cwd) || text(args.workspace_root),
        shell: text(event.data.shell) || text(args.shell),
        unsandboxed: event.data.unsandboxed === true || args.unsandboxed === true,
        outside: Array.isArray(event.data.outside_paths) ? event.data.outside_paths.map(String) : [],
        outsideWrite: event.data.outside_write === true,
        reviewReason: text(event.data.review_reason),
      }
    })
}

/** "Ask before running" questions, shown at the end of the conversation while the agent waits. */
export function ApprovalCards({ taskId, events, className }: { taskId: string; events: readonly AgentEvent[]; className?: string | undefined }) {
  const pending = useMemo(() => pendingApprovals(events), [events])
  const [sent, setSent] = useState<Record<string, Decision>>({})
  const [scopes, setScopes] = useState<Record<string, AllowScope>>({})
  const [openMenuId, setOpenMenuId] = useState<string | null>(null)
  const [error, setError] = useState('')
  const rootRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!openMenuId) return
    const onPointerDown = (event: PointerEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) {
        setOpenMenuId(null)
      }
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpenMenuId(null)
    }
    document.addEventListener('pointerdown', onPointerDown)
    window.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [openMenuId])

  if (pending.length === 0) return null

  const decide = (callId: string, decision: Decision) => {
    setError('')
    setOpenMenuId(null)
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

  return (
    <div ref={rootRef} className={clsx(css.stack, className)}>
      {pending.map(item => {
        const chosen = sent[item.callId]
        const highRisk = item.unsandboxed || item.outsideWrite || item.outside.length > 0
        const availableScopes: AllowScope[] = item.onceOnly
          ? ['once']
          : item.project ? ['once', 'session', 'project'] : ['once', 'session']
        const selectedScope: AllowScope = availableScopes.includes(scopes[item.callId] ?? 'once')
          ? (scopes[item.callId] ?? 'once')
          : 'once'
        const menuOpen = openMenuId === item.callId
        const metaParts = [
          item.shell && item.shell,
          item.workdir && item.workdir,
          item.onceOnly && '仅支持单次授权',
        ].filter(Boolean) as string[]

        return (
          <div
            key={item.callId}
            className={css.card}
            role="alertdialog"
            aria-label={`是否允许${TOOL_LABELS[item.tool] ?? item.tool}`}
          >
            <div className={css.header}>
              <span className={clsx(css.ringIcon, highRisk ? css.ringWarn : css.ringBrand)} aria-hidden="true">
                {highRisk ? (
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
                    <line x1="12" y1="8" x2="12" y2="12" />
                    <line x1="12" y1="16" x2="12.01" y2="16" />
                  </svg>
                ) : (
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <polyline points="4 17 10 11 4 5" />
                    <line x1="12" y1="19" x2="20" y2="19" />
                  </svg>
                )}
              </span>
              <div className={css.headCopy}>
                <div className={css.title}>{TOOL_LABELS[item.tool] ?? `调用 ${item.tool}`}</div>
                {metaParts.length > 0 && (
                  <div className={css.meta}>
                    {metaParts.map((part, idx) => (
                      <span key={part} className={css.metaMono}>
                        {idx > 0 && <span className={css.metaSep}>· </span>}
                        {part}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {item.unsandboxed && (
              <p className={css.calloutWarn}>命令将以当前系统用户权限运行，可访问该用户有权限使用的文件与系统资源。</p>
            )}
            {item.outsideWrite && (
              <p className={css.calloutWarn}>跨项目目录审批：允许后，此工具可在下列工作区外的目标目录写入或执行。</p>
            )}
            {item.reviewReason && (
              <p className={css.calloutWarn}>AI 审查转交给你：{item.reviewReason}</p>
            )}
            {item.outside.length > 0 && (
              <p className={css.hint}>工作区外路径：{item.outside.join('、')}</p>
            )}

            <code className={css.command}>{item.summary}</code>
            {error && <p className={css.error} role="alert">{error}</p>}

            <div className={clsx(css.footer, availableScopes.length === 1 && css.footerRightOnly)}>
              {availableScopes.length > 1 && (
                <div className={css.scopeWrap}>
                  <button
                    type="button"
                    className={clsx(css.scopeTrigger, menuOpen && css.scopeTriggerOpen)}
                    disabled={!!chosen}
                    aria-haspopup="listbox"
                    aria-expanded={menuOpen}
                    onClick={() => setOpenMenuId(curr => curr === item.callId ? null : item.callId)}
                  >
                    <svg className={css.scopeShield} width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
                      <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
                    </svg>
                    <span>{SCOPE_META[selectedScope].label}</span>
                    <svg className={css.scopeChev} width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" aria-hidden="true">
                      <polyline points="6 9 12 15 18 9" />
                    </svg>
                  </button>
                  {menuOpen && (
                    <div className={css.scopeMenu} role="listbox" aria-label="授权范围">
                      {availableScopes.map(scope => {
                        const active = scope === selectedScope
                        return (
                          <button
                            key={scope}
                            type="button"
                            role="option"
                            aria-selected={active}
                            className={clsx(css.scopeOption, active && css.scopeOptionActive)}
                            onClick={() => {
                              setScopes(prev => ({ ...prev, [item.callId]: scope }))
                              setOpenMenuId(null)
                            }}
                          >
                            <span>
                              <span className={css.scopeOptTitle}>{SCOPE_META[scope].label}</span>
                              <span className={css.scopeOptDesc}>{SCOPE_META[scope].desc}</span>
                            </span>
                            <svg className={css.scopeCheck} width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
                              <polyline points="20 6 9 17 4 12" />
                            </svg>
                          </button>
                        )
                      })}
                    </div>
                  )}
                </div>
              )}

              <div className={css.actions}>
                <button type="button" className={css.secondary} disabled={!!chosen} onClick={() => decide(item.callId, 'deny')}>拒绝</button>
                <button type="button" className={css.primary} disabled={!!chosen} onClick={() => decide(item.callId, selectedScope)}>
                  {chosen ? '已提交…' : SCOPE_META[selectedScope].button}
                </button>
              </div>
            </div>
          </div>
        )
      })}
    </div>
  )
}
