import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import { MarkdownText } from '@deepseek-ai/dsh-client-ui-primitives'
import { DAILY_REPORT_SYNC_EVENT, type DailyReportSyncAction, type PluginPageProps } from '../../plugins/protocol.ts'
import { usePluginPages } from '../../plugins/react.tsx'
import {
  DailyError,
  dailyApi,
  emptyDraft,
  onDraftReady,
  today,
  todaySessions,
  type DailyDraft,
  type DailyStatus,
  type ProjectOption,
  type ReportRow,
  type TodayActivity,
  type TodayProject,
} from './api.ts'
import { buildReportContext } from './prompt.ts'
import {
  buildMonthCalendar,
  createEmptyWorkContent,
  createEmptyWorkContentParts,
  extractUnfinishedProjects,
  formatWorkContentGroups,
  isStructuredWorkContent,
  parseWorkContentGroups,
  qualityWarnings,
  validateWorkContent,
  type WorkContentParts,
} from './workContent.ts'
import css from './style.module.css'

/** The preview only needs chrome for code blocks; footnotes get a heading. */
const MARKDOWN_LABELS = { code: { copyLabel: '复制', copiedLabel: '已复制' }, footnotes: '脚注' }

/** What the server shows as a person's name; the token never reaches this page. */
const personName = (status: DailyStatus | undefined) =>
  status?.session.nick_name || status?.session.user_name || '未知用户'

const message = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

const emptyProject = () => ({ project_id: null as number | null, project_name: '', work_content: createEmptyWorkContent() })

const OVERTIME_PRESETS = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] as const
const LEAVE_PRESETS = [
  { value: 0, label: '无请假' },
  { value: 1, label: '1 小时' },
  { value: 2, label: '2 小时' },
  { value: 3, label: '3 小时' },
  { value: 4, label: '4 小时' },
  { value: 5, label: '5 小时' },
  { value: 6, label: '6 小时' },
  { value: 7, label: '7 小时' },
  { value: 8, label: '1 天（8h）' },
  { value: 16, label: '2 天（16h）' },
  { value: 24, label: '3 天（24h）' },
] as const
const WEEKDAYS = ['一', '二', '三', '四', '五', '六', '日'] as const

function shiftMonth(yearMonth: string, delta: number): string {
  const [y, m] = yearMonth.split('-').map(Number)
  if (!y || !m) return today().slice(0, 7)
  const d = new Date(y, m - 1 + delta, 1)
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

function isMyReport(row: ReportRow, session: DailyStatus['session'] | undefined): boolean {
  if (!session) return false
  const rowUserId = row.userId ?? row.createBy
  if (session.user_id && rowUserId !== undefined && rowUserId !== null && String(rowUserId) !== '') {
    return String(rowUserId) === String(session.user_id)
  }
  const rowName = String(row.userName ?? row.nickName ?? '').trim()
  if (!rowName) return false
  return rowName === session.nick_name || rowName === session.user_name
}

const AUTO_SYNC_COOLDOWN_MS = 5000

function formatSyncClock(now = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`
}

function rowToDraft(row: ReportRow, fallbackDate: string): DailyDraft {
  const projects = (row.projects ?? []).map(project => ({
    project_id: project.projectId === undefined || project.projectId === null ? null : Number(project.projectId),
    project_name: String(project.projectName ?? ''),
    work_content: String(project.workContent ?? ''),
  }))
  return {
    report_date: String(row.reportDate ?? fallbackDate).slice(0, 10) || fallbackDate,
    projects: projects.length > 0 ? projects : [emptyProject()],
    problems: String(row.problems ?? ''),
    remark: String(row.remark ?? ''),
    overtime_hours: Number(row.overtimeHours ?? 0),
    leave_hours: Number(row.leaveHours ?? 0),
    reimbursement: Number(row.reimbursement ?? 0),
    reimbursement_reason: String(row.reimbursementReason ?? ''),
    action: 'update',
    report_id: row.id === undefined || row.id === null ? null : String(row.id),
    origin: 'manual',
    updated_at: '',
  }
}

/** Facts about one project, written the way a report can quote them. */
function factLines(entry: TodayProject): string[] {
  const lines: string[] = []
  if (entry.sessions.length > 0) lines.push('会话：' + entry.sessions.map(session => session.title).join('；'))
  if (entry.commits.length > 0) lines.push('提交：' + entry.commits.join('；'))
  if (entry.files.length > 0) {
    const shown = entry.files.slice(0, 12).join('、')
    lines.push('改动文件：' + shown + (entry.files.length > 12 ? ' 等' : ''))
  }
  return lines
}

function Icon({ name, className }: { name: 'folder' | 'search' | 'chevron' | 'check' | 'plus' | 'spark' | 'file' | 'chat' | 'close' | 'refresh' | 'copy'; className?: string | undefined }) {
  const common = { width: 14, height: 14, viewBox: '0 0 16 16', fill: 'none', stroke: 'currentColor', strokeWidth: 1.5, strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const, 'aria-hidden': true, className }
  if (name === 'folder') return <svg {...common}><path d="M1.8 4.2h4l1.3 1.6h7.1v6.6a.8.8 0 0 1-.8.8H2.6a.8.8 0 0 1-.8-.8Z" /><path d="M1.8 4.2V3a.8.8 0 0 1 .8-.8h2.6l1.2 1.6" /></svg>
  if (name === 'search') return <svg {...common}><circle cx="7" cy="7" r="4.3" /><path d="m10.2 10.2 3.3 3.3" /></svg>
  if (name === 'chevron') return <svg {...common}><path d="m4 6 4 4 4-4" /></svg>
  if (name === 'check') return <svg {...common}><path d="m3 8.4 3.2 3.2L13 4.8" /></svg>
  if (name === 'plus') return <svg {...common}><path d="M8 3.4v9.2M3.4 8h9.2" /></svg>
  if (name === 'spark') return <svg {...common}><path d="M8 2.2 9.4 6.2 13.4 7.6 9.4 9 8 13 6.6 9 2.6 7.6 6.6 6.2Z" /></svg>
  if (name === 'file') return <svg {...common}><path d="M9.2 1.8H4.6a.8.8 0 0 0-.8.8v10.8a.8.8 0 0 0 .8.8h6.8a.8.8 0 0 0 .8-.8V4.6Z" /><path d="M9.2 1.8v2.8h3" /></svg>
  if (name === 'chat') return <svg {...common}><path d="M13.6 8.3c0 2.7-2.5 4.9-5.6 4.9a6.9 6.9 0 0 1-1.8-.2l-3.4 1 1-2.5a4.6 4.6 0 0 1-1.4-3.2c0-2.7 2.5-4.9 5.6-4.9s5.6 2.2 5.6 4.9Z" /></svg>
  if (name === 'close') return <svg {...common}><path d="m4.4 4.4 7.2 7.2M11.6 4.4l-7.2 7.2" /></svg>
  if (name === 'refresh') return <svg {...common}><path d="M13 8a5 5 0 1 1-1.6-3.7" /><path d="M13.2 2.2v3h-3" /></svg>
  return <svg {...common}><rect x="5.6" y="5.6" width="7" height="8" rx="1" /><path d="M10.4 5.6V3.4a1 1 0 0 0-1-1H4.4a1 1 0 0 0-1 1v6.2a1 1 0 0 0 1 1h1.2" /></svg>
}

function Field({ label, children, wide }: { label: string; children: React.ReactNode; wide?: boolean }) {
  return <label className={clsx(css.field, wide && css.fieldWide)}>
    <span>{label}</span>
    {children}
  </label>
}

/** Highlights the part of a project name the typed text matched. */
function Highlight({ text, needle }: { text: string; needle: string }) {
  const index = needle ? text.toLowerCase().indexOf(needle.toLowerCase()) : -1
  if (index < 0 || !needle) return <>{text}</>
  return <>{text.slice(0, index)}<mark>{text.slice(index, index + needle.length)}</mark>{text.slice(index + needle.length)}</>
}

/** A project field that searches the server's list instead of a native datalist. */
function ProjectPicker({ value, options, onPick }: {
  value: string
  options: readonly ProjectOption[]
  onPick: (name: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [text, setText] = useState(value)
  const [active, setActive] = useState(0)
  const box = useRef<HTMLDivElement>(null)
  const field = useRef<HTMLInputElement>(null)

  // An outside change (a loaded history draft, an AI draft) wins while closed.
  useEffect(() => { if (!open) setText(value) }, [value, open])

  const needle = text.trim()
  const matches = useMemo(() => {
    const lower = needle.toLowerCase()
    const seen = new Set<string>()
    const hits: { option: ProjectOption; at: number }[] = []
    for (const option of options) {
      const name = option.project_name
      if (!name || seen.has(name)) continue
      const at = lower ? name.toLowerCase().indexOf(lower) : 0
      if (at < 0) continue
      seen.add(name)
      hits.push({ option, at })
    }
    // Prefix matches first, then the shortest name: the intended project is on top.
    hits.sort((left, right) => left.at - right.at || left.option.project_name.length - right.option.project_name.length)
    return hits.slice(0, 80)
  }, [options, needle])

  useEffect(() => { setActive(0) }, [needle])

  const commit = (name: string) => {
    const clean = name.trim()
    if (!clean) return
    onPick(clean)
    setText(clean)
    setOpen(false)
  }

  const close = () => { setOpen(false); setText(value) }

  useEffect(() => {
    if (!open) return
    const onPointerDown = (event: PointerEvent) => {
      if (box.current && !box.current.contains(event.target as Node)) close()
    }
    document.addEventListener('pointerdown', onPointerDown)
    return () => document.removeEventListener('pointerdown', onPointerDown)
  }, [open, value])

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'ArrowDown') {
      event.preventDefault(); setOpen(true); setActive(index => Math.min(index + 1, Math.max(matches.length - 1, 0)))
    } else if (event.key === 'ArrowUp') {
      event.preventDefault(); setActive(index => Math.max(index - 1, 0))
    } else if (event.key === 'Enter') {
      event.preventDefault()
      const hit = matches[active]
      commit(hit ? hit.option.project_name : text)
    } else if (event.key === 'Escape') {
      event.preventDefault(); close()
    } else if (event.key === 'Tab') {
      setOpen(false)
    }
  }

  return <div className={css.picker} ref={box}>
    <div className={clsx(css.pickerField, open && css.pickerFieldOpen)}>
      <span className={css.pickerIcon}><Icon name="folder" /></span>
      <input
        ref={field}
        role="combobox"
        aria-expanded={open}
        aria-controls="daily-report-project-menu"
        aria-autocomplete="list"
        value={text}
        placeholder="选择或输入项目名称"
        spellCheck={false}
        onChange={event => { setText(event.target.value); setOpen(true) }}
        onFocus={() => setOpen(true)}
        onKeyDown={onKeyDown}
        onBlur={event => {
          if (box.current && event.relatedTarget instanceof Node && box.current.contains(event.relatedTarget)) return
          if (text.trim() && text.trim() !== value) commit(text)
          else close()
        }}
      />
      {text && <button type="button" className={css.pickerClear} aria-label="清空项目" title="清空"
        onMouseDown={event => event.preventDefault()}
        onClick={() => { setText(''); onPick(''); field.current?.focus(); setOpen(true) }}><Icon name="close" /></button>}
      <span className={css.pickerCaret}><Icon name="chevron" /></span>
    </div>
    {open && <div className={css.menu} id="daily-report-project-menu" role="listbox" aria-label="项目列表">
      <div className={css.menuList}>
        {matches.length === 0 && <p className={css.menuEmpty}>
          {options.length === 0 ? '还没有读到项目列表，可以直接输入名称。' : '没有匹配的项目，按 Enter 使用「' + (needle || '新项目') + '」。'}
        </p>}
        {matches.map((hit, index) => <button key={hit.option.project_name} type="button" role="option"
          aria-selected={index === active}
          className={clsx(css.option, index === active && css.optionActive, hit.option.project_name === value && css.optionPicked)}
          onMouseDown={event => event.preventDefault()}
          onMouseEnter={() => setActive(index)}
          onClick={() => commit(hit.option.project_name)}>
          <span className={css.optionName}><Highlight text={hit.option.project_name} needle={needle} /></span>
          {hit.option.project_name === value && <span className={css.optionTag}>当前</span>}
          {hit.option.project_id !== null && hit.option.project_id !== '' && <span className={css.optionId}>#{String(hit.option.project_id)}</span>}
        </button>)}
      </div>
      <div className={css.menuFoot}>
        <span>{matches.length} / {options.length} 个项目</span>
        <span>↑↓ 选择 · Enter 确认 · Esc 关闭</span>
      </div>
    </div>}
  </div>
}

/** One project row: name from the server list, plus structured WorkContentInput blocks matching report_vue. */
function ProjectCard({ index, project, options, facts, showValidation, onChange, onRemove, removable }: {
  index: number
  project: DailyDraft['projects'][number]
  options: readonly ProjectOption[]
  facts?: TodayProject | undefined
  showValidation?: boolean | undefined
  onChange: (next: DailyDraft['projects'][number]) => void
  onRemove: () => void
  removable: boolean
}) {
  const [rawMode, setRawMode] = useState(() => !isStructuredWorkContent(project.work_content))
  const groups = useMemo(() => parseWorkContentGroups(project.work_content), [project.work_content])
  const warnings = useMemo(() => qualityWarnings(groups), [groups])
  const validationError = showValidation ? validateWorkContent(project.work_content) : null

  const updateGroups = (nextGroups: WorkContentParts[]) => {
    onChange({ ...project, work_content: formatWorkContentGroups(nextGroups) })
  }
  const updateGroup = (at: number, patch: Partial<WorkContentParts>) => {
    updateGroups(groups.map((item, idx) => idx === at ? { ...item, ...patch } : item))
  }
  const addGroup = () => {
    updateGroups([...groups, createEmptyWorkContentParts()])
  }
  const removeGroup = (at: number) => {
    if (groups.length <= 1) return
    updateGroups(groups.filter((_, idx) => idx !== at))
  }

  const pick = (name: string) => {
    const match = options.find(option => option.project_name === name)
    const projectId = match && match.project_id !== null && match.project_id !== '' ? Number(match.project_id) : null
    onChange({ ...project, project_name: name, project_id: projectId })
  }
  const fill = () => {
    if (!facts) return
    const lines = factLines(facts)
    if (lines.length === 0) return
    if (rawMode) {
      const body = project.work_content.trim()
      onChange({ ...project, work_content: (body ? body + '\n' : '') + lines.join('\n') })
      return
    }
    const next = groups.map((g, idx) => idx === groups.length - 1
      ? { ...g, today: (g.today.trim() ? g.today.trim() + '\n' : '') + lines.join('\n') }
      : g)
    updateGroups(next)
  }

  return <section className={clsx(css.projectCard, validationError && css.projectCardInvalid)}>
    <header className={css.projectHead}>
      <span className={css.projectIndex}>{index + 1}</span>
      <ProjectPicker value={project.project_name} options={options} onPick={pick} />
      <div className={css.projectActions}>
        {facts && (facts.sessions.length > 0 || facts.commits.length > 0 || facts.files.length > 0) && <button type="button" className={css.chipButton} onClick={fill} title="把今天这个项目的会话、提交和改动文件追加到今日完成工作">
          <Icon name="spark" />填入事实
        </button>}
        <button type="button" className={css.chipButton} onClick={() => setRawMode(v => !v)} title="在结构化三段式与纯文本之间切换">
          {rawMode ? '结构化' : '纯文本'}
        </button>
        {removable && <button type="button" className={css.chipButton} onClick={onRemove} title="删除这个项目">删除</button>}
      </div>
    </header>

    {rawMode ? (
      <textarea className={css.workContent} rows={5} value={project.work_content}
        placeholder={'1. 具体任务或功能：…\n2. 今日完成工作：…\n3. 进度 100 %'}
        onChange={event => onChange({ ...project, work_content: event.target.value })} />
    ) : (
      <div className={css.featureGroups}>
        {groups.map((group, gIndex) => (
          <div key={gIndex} className={css.featureGroup}>
            <div className={css.featureHead}>
              <span>功能 {gIndex + 1}</span>
              {groups.length > 1 && (
                <button type="button" className={css.featureDel} onClick={() => removeGroup(gIndex)}>删除功能</button>
              )}
            </div>
            <label className={css.featureRow}>
              <span className={css.featureLabel}>1. 具体任务或功能</span>
              <textarea rows={1} value={group.task} placeholder="负责这个项目中的具体任务或功能模块"
                onChange={event => updateGroup(gIndex, { task: event.target.value })} />
            </label>
            <label className={css.featureRow}>
              <span className={css.featureLabel}>2. 今日完成工作</span>
              <textarea rows={2} value={group.today} placeholder="今天具体做了什么、达到什么可验收结果"
                onChange={event => updateGroup(gIndex, { today: event.target.value })} />
            </label>
            <div className={css.featureRowInline}>
              <span className={css.featureLabel}>3. 进度</span>
              <div className={css.progressControl}>
                <input
                  type="text"
                  inputMode="numeric"
                  maxLength={3}
                  value={group.progress}
                  placeholder="0-100"
                  aria-label={`功能 ${gIndex + 1} 进度`}
                  onChange={event => updateGroup(gIndex, { progress: event.target.value.replace(/\D/g, '').slice(0, 3) })}
                />
                <span className={css.progressUnit}>%</span>
              </div>
              <div className={css.progressQuick}>
                {['50', '80', '100'].map(pct => (
                  <button key={pct} type="button"
                    className={clsx(css.pctChip, group.progress.trim() === pct && css.pctChipActive)}
                    onClick={() => updateGroup(gIndex, { progress: pct })}>
                    {pct}%
                  </button>
                ))}
              </div>
            </div>
          </div>
        ))}
        <button type="button" className={css.addFeatureBtn} onClick={addGroup}>
          <Icon name="plus" />添加功能
        </button>
      </div>
    )}

    {validationError && <p className={css.fieldError} role="alert">{validationError}</p>}
    {!validationError && warnings.length > 0 && (
      <p className={css.qualityTip}>{warnings[0]}</p>
    )}
    <footer className={css.projectFoot}>
      <span>{groups.length} 个功能块</span>
      {facts && <span className={css.projectFacts}>
        {facts.sessions.length} 会话 · {facts.commits.length} 提交 · {facts.files.length} 文件
      </span>}
    </footer>
  </section>
}

/** The login card: one control for the address, one pair of credentials. */
function Login({ onDone, initialBase }: { onDone: (status: DailyStatus) => void; initialBase: string }) {
  const [mode, setMode] = useState<'password' | 'token'>('password')
  const [base, setBase] = useState(initialBase || 'http://192.168.100.145:8882/prod-api')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [token, setToken] = useState('')
  const [clientId, setClientId] = useState('')
  const [advanced, setAdvanced] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (busy) return
    setBusy(true); setError('')
    try {
      const next = await dailyApi.login(mode === 'password'
        ? { base_url: base, username, password, ...(clientId.trim() ? { client_id: clientId.trim() } : {}) }
        : { base_url: base, token, ...(clientId.trim() ? { client_id: clientId.trim() } : {}) })
      onDone(next)
    } catch (cause) {
      // The password is never echoed, in a message or in the field.
      setError(message(cause)); setPassword('')
    } finally { setBusy(false) }
  }

  return <div className={css.loginWrap}>
    <form className={css.loginCard} onSubmit={submit}>
      <h2>登录团队日报系统</h2>
      <p className={css.loginHint}>登录后在这里填报日报；也可以让 AI 读取写作规范、汇总今日工作并起草。</p>
      <div className={css.modeSwitch} role="tablist" aria-label="登录方式">
        <button type="button" role="tab" aria-selected={mode === 'password'} className={clsx(css.modeTab, mode === 'password' && css.modeTabActive)} onClick={() => { setMode('password'); setError('') }}>账号密码登录</button>
        <button type="button" role="tab" aria-selected={mode === 'token'} className={clsx(css.modeTab, mode === 'token' && css.modeTabActive)} onClick={() => { setMode('token'); setError('') }}>Token 登录</button>
      </div>
      <Field label="服务地址" wide><input value={base} onChange={event => setBase(event.target.value)} spellCheck={false} autoComplete="off" /></Field>
      {mode === 'password' ? <>
        <Field label="用户名"><input value={username} onChange={event => setUsername(event.target.value)} autoComplete="username" /></Field>
        <Field label="密码"><input type="password" value={password} onChange={event => setPassword(event.target.value)} autoComplete="current-password" /></Field>
      </> : <Field label="Bearer Token / API Key" wide>
        <input type="password" value={token} onChange={event => setToken(event.target.value)} autoComplete="off" placeholder="粘贴运维给的令牌" />
      </Field>}
      <button type="button" className={css.advancedToggle} aria-expanded={advanced} onClick={() => setAdvanced(value => !value)}>
        {advanced ? '收起高级设置' : '高级设置'}
      </button>
      {advanced && <Field label="clientId" wide><input value={clientId} onChange={event => setClientId(event.target.value)} placeholder="留空使用系统默认值" spellCheck={false} /></Field>}
      {error && <p className={css.error} role="alert">{error}</p>}
      <button type="submit" className={clsx(css.primary, css.loginSubmit)} disabled={busy} data-busy={busy ? 'true' : undefined}>
        {busy ? '正在登录…' : '登录'}
      </button>
      <p className={css.loginFoot}>令牌保存在本机的插件目录里，不会写进仓库，也不会回显。</p>
    </form>
  </div>
}

/** Today's facts, per project: the raw material a report may quote. */
function ActivityFacts({ activity, busy, onAdd, onRefresh }: {
  activity: TodayActivity
  busy: boolean
  onAdd: (entry: TodayProject) => void
  onRefresh: () => void
}) {
  const [open, setOpen] = useState<Record<string, boolean>>({})
  return <section className={css.facts} aria-label="今日工作事实">
    <header className={css.factsHead}>
      <strong><Icon name="file" />今日工作事实</strong>
      <span className={css.factsTotals}>{activity.totals.sessions} 会话 · {activity.totals.files} 文件 · {activity.totals.commits} 提交</span>
      <button type="button" className={css.iconButton} onClick={onRefresh} disabled={busy} title="重新扫描今天的会话、改动和提交" aria-label="重新汇总">
        <Icon name="refresh" />
      </button>
    </header>
    {activity.projects.length === 0 && <p className={css.placeholder}>今天还没有可汇总的工作记录。</p>}
    {activity.projects.map(entry => {
      const key = entry.workspace || entry.project
      const expanded = open[key] === true
      return <div key={key} className={css.factRow}>
        <div className={css.factMain}>
          <strong>{entry.project}</strong>
          <span>{entry.sessions.length} 会话 · {entry.commits.length} 提交 · {entry.files.length} 文件</span>
          <button type="button" className={css.factToggle} aria-expanded={expanded}
            onClick={() => setOpen(current => ({ ...current, [key]: !expanded }))}>
            {expanded ? '收起细节' : '看细节'}
          </button>
        </div>
        <button type="button" className={css.chipButton} onClick={() => onAdd(entry)}>加入表单</button>
        {expanded && <div className={css.factDetail}>
          {entry.sessions.length > 0 && <p><b>会话</b>{entry.sessions.map(session => session.title).join('；')}</p>}
          {entry.commits.length > 0 && <p><b>提交</b>{entry.commits.join('；')}</p>}
          {entry.files.length > 0 && <p><b>文件</b>{entry.files.join('、')}</p>}
          {entry.sessions.length === 0 && entry.commits.length === 0 && entry.files.length === 0 && <p>这一条没有可展开的内容。</p>}
        </div>}
      </div>
    })}
    {activity.warnings.length > 0 && <p className={css.warn}>{activity.warnings.join('；')}</p>}
  </section>
}

/** One past report: a date, a person, and the projects it covers. */
function HistoryRow({ row, onLoad }: { row: ReportRow; onLoad: () => void }) {
  const projects = (row.projects ?? []).map(project => project.projectName).filter(Boolean)
  return <button type="button" className={css.historyRow} onClick={onLoad} title="载入表单修改">
    <span className={css.historyTop}>
      <span className={css.historyDate}>{String(row.reportDate ?? '').slice(0, 10)}</span>
      <span className={css.historyWho}>{String(row.userName ?? row.nickName ?? '')}</span>
    </span>
    <span className={css.historyProjects}>{projects.length > 0 ? projects.join(' · ') : '未写项目'}</span>
    {Boolean(row.problems) && <span className={css.historyProblems}>问题：{String(row.problems).slice(0, 60)}</span>}
  </button>
}

function EmbeddedMarkdownGuide({
  ownerId,
  pageId,
  value,
  saved,
  theme,
  onChange,
  onSave,
}: {
  ownerId: string
  pageId: string
  value: string
  saved: boolean
  theme?: 'light' | 'dark' | undefined
  onChange: (next: string) => void
  onSave: (next: string) => Promise<void>
}) {
  const frameRef = useRef<HTMLIFrameElement>(null)
  const readyRef = useRef(false)
  const valueRef = useRef(value)
  valueRef.current = value
  const savedRef = useRef(saved)
  savedRef.current = saved
  const lastFromIframeRef = useRef<string | null>(null)
  const onChangeRef = useRef(onChange)
  onChangeRef.current = onChange
  const onSaveRef = useRef(onSave)
  onSaveRef.current = onSave

  // Keep iframe src stable across theme/value changes; live updates flow over postMessage.
  const initialThemeRef = useRef(theme)
  const src = useMemo(() => {
    const query = new URLSearchParams({ mode: 'embed', name: 'guide.md' })
    if (initialThemeRef.current) query.set('theme', initialThemeRef.current)
    return `/agent/skill-packages/${encodeURIComponent(ownerId)}/pages/${encodeURIComponent(pageId)}/index?${query.toString()}`
  }, [ownerId, pageId])

  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      if (event.source !== frameRef.current?.contentWindow || event.origin !== location.origin) return
      const msg = event.data as Record<string, unknown> | null | undefined
      if (!msg || typeof msg.type !== 'string') return
      if (msg.type === 'quickagent:md-ready') {
        readyRef.current = true
        lastFromIframeRef.current = valueRef.current
        frameRef.current?.contentWindow?.postMessage(
          { type: 'quickagent:md-set-content', text: valueRef.current, title: 'guide.md', markSaved: savedRef.current },
          location.origin,
        )
        return
      }
      if (msg.type === 'quickagent:md-change' && typeof msg.text === 'string') {
        lastFromIframeRef.current = msg.text
        onChangeRef.current(msg.text)
        return
      }
      if (msg.type === 'quickagent:md-save' && typeof msg.text === 'string') {
        lastFromIframeRef.current = msg.text
        void onSaveRef.current(msg.text)
      }
    }
    window.addEventListener('message', onMessage)
    return () => window.removeEventListener('message', onMessage)
  }, [])

  useEffect(() => {
    if (!readyRef.current) return
    if (value === lastFromIframeRef.current) return
    lastFromIframeRef.current = value
    frameRef.current?.contentWindow?.postMessage(
      { type: 'quickagent:md-set-content', text: value, title: 'guide.md', markSaved: saved },
      location.origin,
    )
  }, [value, saved])

  useEffect(() => {
    if (!readyRef.current || !saved) return
    frameRef.current?.contentWindow?.postMessage({ type: 'quickagent:md-mark-saved' }, location.origin)
  }, [saved])

  useEffect(() => {
    if (!readyRef.current || !theme) return
    frameRef.current?.contentWindow?.postMessage({ type: 'quickagent:theme', theme }, location.origin)
  }, [theme])

  return (
    <div className={css.guideFrameWrap}>
      <iframe
        ref={frameRef}
        src={src}
        title="写作规范 Markdown 编辑器"
        sandbox="allow-scripts allow-same-origin"
        className={css.guideFrame}
      />
    </div>
  )
}

export default function DailyReportWorkbench({ workspaceRoot, theme, onAskAi, onStudioContext }: PluginPageProps) {
  const { packages: pluginPackages } = usePluginPages()
  const markdownEmbed = useMemo(() => {
    const pkg = pluginPackages.find(
      item => item.enabled && !item.remote && item.id === 'markdown' && item.pages.some(p => p.renderer === 'html'),
    )
    const page = pkg?.pages.find(p => p.renderer === 'html')
    return pkg && page ? { ownerId: pkg.id, pageId: page.id } : null
  }, [pluginPackages])
  const [status, setStatus] = useState<DailyStatus>()
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [tab, setTab] = useState<'fill' | 'guide'>('fill')
  const [draft, setDraft] = useState<DailyDraft>(() => emptyDraft())
  const [projects, setProjects] = useState<readonly ProjectOption[]>([])
  const [monthRows, setMonthRows] = useState<readonly ReportRow[]>([])
  const [calendarMonth, setCalendarMonth] = useState(() => today().slice(0, 7))
  const [historyBusy, setHistoryBusy] = useState(false)
  const [syncing, setSyncing] = useState(false)
  const [lastSyncedAt, setLastSyncedAt] = useState('')
  const [showValidation, setShowValidation] = useState(false)
  const [customOvertime, setCustomOvertime] = useState(false)
  const [activity, setActivity] = useState<TodayActivity>()
  const [activityBusy, setActivityBusy] = useState(false)
  const [busy, setBusy] = useState(false)
  const [guide, setGuide] = useState('')
  const [guideSaved, setGuideSaved] = useState(true)
  const formColumnRef = useRef<HTMLElement>(null)
  const statusRef = useRef<DailyStatus | undefined>(status)
  statusRef.current = status
  const calendarMonthRef = useRef(calendarMonth)
  calendarMonthRef.current = calendarMonth
  const guideSavedRef = useRef(guideSaved)
  guideSavedRef.current = guideSaved
  const draftDirtyRef = useRef(false)
  const lastTodayRef = useRef(today())
  const syncInFlightRef = useRef(false)
  const lastSyncMsRef = useRef(0)

  const loggedIn = !!status?.session.logged_in
  const sessions = useMemo(() => todaySessions(activity), [activity])
  const canAsk = typeof onAskAi === 'function'
  const isFullDayLeave = Number(draft.leave_hours || 0) >= 8

  const editDraft = useCallback((updater: (current: DailyDraft) => DailyDraft) => {
    draftDirtyRef.current = true
    setDraft(updater)
  }, [])

  useEffect(() => {
    const h = Number(draft.overtime_hours || 0)
    setCustomOvertime(!(Number.isInteger(h) && h >= 0 && h <= 12))
  }, [draft.overtime_hours])

  const handleRemoteError = useCallback((cause: unknown, prefix: string, baseStatus?: DailyStatus) => {
    if (cause instanceof DailyError && cause.authExpired) {
      const current = baseStatus ?? statusRef.current
      if (current) {
        setStatus({ ...current, session: { ...current.session, logged_in: false } })
      }
      setMonthRows([])
      setError(`登录状态已过期，请重新登录：${cause.message}`)
      return true
    }
    setError(`${prefix}：${message(cause)}`)
    return false
  }, [])

  const loadMonth = useCallback(async (yearMonth: string, session: DailyStatus['session'] | undefined) => {
    const [reportYear] = yearMonth.split('-')
    const userId = session?.user_id ?? ''
    const page = await dailyApi.reports({
      ...(reportYear ? { reportYear } : {}),
      reportMonth: yearMonth,
      pageSize: '100',
      ...(userId ? { userId } : {}),
    })
    const rows = (page.rows ?? []).filter(row => !session || isMyReport(row, session))
    setMonthRows(rows)
    setLastSyncedAt(formatSyncClock())
    return rows
  }, [])

  /** A day already filed BY THE CURRENT USER is an update, not a second report. */
  const reconcile = useCallback((date: string, rows: readonly ReportRow[], session: DailyStatus['session'] | undefined) => {
    const mine = rows.find(row => String(row.reportDate ?? '').slice(0, 10) === date && (!session || isMyReport(row, session)))
    setDraft(current => {
      if (current.report_date !== date) return current
      return mine && mine.id !== undefined && mine.id !== null
        ? { ...current, action: 'update', report_id: String(mine.id) }
        : { ...current, action: 'create', report_id: null }
    })
    return mine
  }, [])

  const syncRemote = useCallback(async (options?: {
    mode?: 'open' | 'refresh'
    initialStatus?: DailyStatus
    showNotice?: boolean
    force?: boolean
  }) => {
    const mode = options?.mode ?? 'refresh'
    if (syncInFlightRef.current) return
    if (!options?.force && mode === 'refresh' && Date.now() - lastSyncMsRef.current < AUTO_SYNC_COOLDOWN_MS) {
      return
    }
    syncInFlightRef.current = true
    setSyncing(true)
    setHistoryBusy(true)
    if (options?.showNotice || mode === 'open') {
      setError('')
    }
    try {
      const next = options?.initialStatus ?? await dailyApi.status()
      setStatus(next)
      if (!next.session.logged_in) {
        setMonthRows([])
        return
      }

      const currentToday = today()
      const rolledOver = currentToday !== lastTodayRef.current
      const prevToday = lastTodayRef.current
      lastTodayRef.current = currentToday

      let targetDate = mode === 'open'
        ? (next.draft?.report_date || currentToday)
        : draftRef.current.report_date
      let targetMonth = mode === 'open'
        ? targetDate.slice(0, 7)
        : calendarMonthRef.current

      if (mode === 'refresh' && rolledOver && !draftDirtyRef.current && targetDate === prevToday) {
        targetDate = currentToday
        targetMonth = currentToday.slice(0, 7)
        setDraft(emptyDraft(currentToday))
      }

      setCalendarMonth(targetMonth)

      if (mode === 'open' && next.draft) {
        draftDirtyRef.current = false
        setDraft({ ...next.draft })
      }

      const [projectsRes, rowsRes, guideRes] = await Promise.allSettled([
        dailyApi.projects(),
        loadMonth(targetMonth, next.session),
        guideSavedRef.current ? dailyApi.guide() : Promise.resolve(null),
      ])

      if (projectsRes.status === 'fulfilled') {
        setProjects(projectsRes.value.projects)
      }
      if (guideRes.status === 'fulfilled' && guideRes.value) {
        setGuide(guideRes.value.content)
        setGuideSaved(true)
      }

      if (rowsRes.status === 'rejected') {
        handleRemoteError(rowsRes.reason, '同步远程日报失败', next)
        return
      }
      if (projectsRes.status === 'rejected') {
        if (handleRemoteError(projectsRes.reason, '同步项目列表失败', next)) return
      }

      const rows = rowsRes.value
      lastSyncMsRef.current = Date.now()
      setError('')

      if (targetDate.slice(0, 7) === targetMonth) {
        const existingForDate = reconcile(targetDate, rows, next.session)
        if (!draftDirtyRef.current && !next.draft) {
          if (existingForDate) {
            setDraft(rowToDraft(existingForDate, targetDate))
          } else if (mode === 'open') {
            setDraft({ ...emptyDraft(targetDate), projects: [emptyProject()] })
          }
        }
      }

      if (options?.showNotice) {
        setNotice(`已同步远程日报数据（${targetMonth} 共 ${rows.length} 条）`)
      }
    } catch (cause) {
      handleRemoteError(cause, '同步远程日报失败')
    } finally {
      syncInFlightRef.current = false
      setSyncing(false)
      setHistoryBusy(false)
    }
  }, [handleRemoteError, loadMonth, reconcile])

  const refreshMonth = useCallback(async (yearMonth: string, options?: { showNotice?: boolean | undefined; reconcileDate?: string | undefined }) => {
    const session = statusRef.current?.session
    if (!session?.logged_in) return [] as ReportRow[]
    setHistoryBusy(true)
    try {
      const rows = await loadMonth(yearMonth, session)
      setError('')
      if (options?.reconcileDate) {
        const found = reconcile(options.reconcileDate, rows, session)
        if (found && !draftDirtyRef.current) {
          setDraft(rowToDraft(found, options.reconcileDate))
        }
      }
      if (options?.showNotice) {
        setNotice(`已刷新 ${yearMonth} 日报（共 ${rows.length} 条）`)
      }
      return rows
    } catch (cause) {
      handleRemoteError(cause, '刷新本月日报失败')
      return [] as ReportRow[]
    } finally {
      setHistoryBusy(false)
    }
  }, [handleRemoteError, loadMonth, reconcile])

  // Auto-sync remote status, projects, and monthly reports every time the page is entered.
  useEffect(() => {
    setLoading(true)
    void syncRemote({ mode: 'open', force: true }).finally(() => setLoading(false))
  }, [syncRemote])

  // Re-sync automatically when returning to the app window / tab (throttled by AUTO_SYNC_COOLDOWN_MS).
  useEffect(() => {
    if (!loggedIn) return
    const triggerAutoSync = () => {
      if (document.visibilityState === 'hidden') return
      void syncRemote({ mode: 'refresh' })
    }
    const onVisibility = () => {
      if (document.visibilityState === 'visible') triggerAutoSync()
    }
    window.addEventListener('focus', triggerAutoSync)
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      window.removeEventListener('focus', triggerAutoSync)
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [loggedIn, syncRemote])

  // The AI tool stored a draft and brought this page forward; read it again.
  useEffect(() => onDraftReady(() => {
    void syncRemote({ mode: 'open', force: true })
  }), [syncRemote])

  const loadActivity = useCallback(async (date: string) => {
    if (!workspaceRoot) return
    setActivityBusy(true); setError('')
    try { setActivity(await dailyApi.today(workspaceRoot, date)) } catch (cause) { setError(message(cause)) } finally { setActivityBusy(false) }
  }, [workspaceRoot])

  // Real-time UI manipulation from the universal CodeChatDock beside this page
  // (mirrors Database Studio's DB_SYNC_EVENT pattern).
  useEffect(() => {
    const onSync = (event: Event) => {
      const detail = (event as CustomEvent<DailyReportSyncAction>).detail
      if (!detail) return
      setError('')
      if (detail.action === 'prepare_draft') {
        setTab('fill')
        void syncRemote({ mode: 'open', force: true }).then(() => {
          const dateLabel = draftRef.current.report_date || detail.reportDate || ''
          setNotice(`已同步 AI 填写的日报草稿${dateLabel ? ' · ' + dateLabel : ''}`)
          formColumnRef.current?.scrollTo({ top: 0, behavior: 'smooth' })
        })
        return
      }
      if (detail.action === 'write_guide') {
        setTab('guide')
        if (typeof detail.content === 'string') {
          setGuide(detail.content)
          setGuideSaved(true)
          setNotice('已同步 AI 更新的写作规范')
        }
        void dailyApi.guide().then(page => {
          setGuide(page.content)
          setGuideSaved(true)
          setNotice('已同步 AI 更新的写作规范')
        }, () => {})
        return
      }
      if (detail.action === 'submit') {
        setTab('fill')
        draftDirtyRef.current = false
        void syncRemote({ mode: 'open', force: true }).then(() => {
          setNotice(`已同步 AI 提交的日报${detail.reportDate ? ' · ' + detail.reportDate : ''}`)
        })
        return
      }
      if (detail.action === 'today_activity') {
        const targetDate = detail.reportDate || draftRef.current.report_date
        if (detail.reportDate && detail.reportDate !== draftRef.current.report_date) {
          setDraft(current => ({ ...current, report_date: detail.reportDate! }))
        }
        void loadActivity(targetDate)
        return
      }
      if (detail.action === 'list') {
        const targetDate = detail.reportDate || draftRef.current.report_date
        const targetMonth = targetDate.slice(0, 7)
        setCalendarMonth(targetMonth)
        void refreshMonth(targetMonth, { reconcileDate: targetDate })
      }
    }
    window.addEventListener(DAILY_REPORT_SYNC_EVENT, onSync)
    return () => window.removeEventListener(DAILY_REPORT_SYNC_EVENT, onSync)
  }, [syncRemote, loadActivity, refreshMonth])

  // The chat beside this page is offered today's facts and live form state, so a follow-up needs no re-explaining.
  const publish = useRef(onStudioContext)
  publish.current = onStudioContext
  const draftRef = useRef(draft)
  draftRef.current = draft
  useEffect(() => () => { publish.current?.(null) }, [])
  useEffect(() => {
    if (!publish.current) return
    const timer = window.setTimeout(() => {
      const latest = draftRef.current
      const filled = latest.projects.filter(project => project.project_name.trim() || project.work_content.trim()).length
      const contentSig = latest.projects.map(p => `${p.project_name}:${p.work_content.length}`).join(',')
      const baseText = buildReportContext({ date: latest.report_date, draft: latest, sessions, activity })
      const tabLabel = tab === 'guide' ? '写作规范' : '日报填报'
      publish.current?.({
        plugin: 'daily-report',
        id: ['daily-report', tab, latest.report_date, latest.action, filled, contentSig, latest.problems.length, sessions.length].join(':'),
        badge: tab === 'guide' ? '日报规范' : '日报填报',
        detail: `${latest.report_date} · ${filled} 个项目`,
        text: `当前页签：${tabLabel}\n${baseText}`,
      })
    }, 120)
    return () => window.clearTimeout(timer)
  }, [tab, draft, sessions, activity])

  // The page is about today, so today's facts are read without being asked for.
  useEffect(() => {
    if (!loggedIn || !workspaceRoot) return
    void loadActivity(draft.report_date)
  }, [loggedIn, workspaceRoot, draft.report_date, loadActivity])

  const pickDate = (date: string, existingRow?: ReportRow) => {
    setShowValidation(false)
    setError(''); setNotice('')
    draftDirtyRef.current = false
    const nextMonth = date.slice(0, 7)
    setActivity(undefined)
    if (existingRow) {
      setDraft(rowToDraft(existingRow, date))
      setCalendarMonth(nextMonth)
      return
    }
    const foundLocal = monthRows.find(row => String(row.reportDate ?? '').slice(0, 10) === date)
    if (foundLocal) {
      setDraft(rowToDraft(foundLocal, date))
      setCalendarMonth(nextMonth)
      return
    }
    setDraft({ ...emptyDraft(date), projects: [emptyProject()] })
    if (nextMonth !== calendarMonth) {
      setCalendarMonth(nextMonth)
      void refreshMonth(nextMonth, { reconcileDate: date })
    }
  }

  const switchCalendarMonth = (delta: number) => {
    const nextMonth = shiftMonth(calendarMonth, delta)
    setCalendarMonth(nextMonth)
    void refreshMonth(nextMonth)
  }

  /** Turn one aggregated project into a structured form row: facts only, no invented verbs. */
  const addFacts = (entry: TodayProject) => {
    const lines = factLines(entry)
    const content = formatWorkContentGroups([{ task: entry.project, today: lines.join('\n'), progress: '' }])
    editDraft(current => ({
      ...current,
      projects: [...current.projects.filter(project => project.project_name.trim() || project.work_content.trim()),
        { project_id: null, project_name: entry.project, work_content: content }],
    }))
  }

  const latestCarryOver = useMemo(() => {
    const sorted = [...monthRows].sort((a, b) => String(b.reportDate ?? '').localeCompare(String(a.reportDate ?? '')))
    for (const row of sorted) {
      if (String(row.reportDate ?? '').slice(0, 10) === draft.report_date) continue
      const carried = extractUnfinishedProjects(row)
      if (carried.length > 0) return { date: String(row.reportDate ?? '').slice(0, 10), projects: carried }
    }
    return null
  }, [monthRows, draft.report_date])

  const applyCarryOver = () => {
    if (!latestCarryOver) return
    setShowValidation(false)
    editDraft(current => ({ ...current, projects: latestCarryOver.projects }))
    setNotice(`已带入 ${latestCarryOver.date} 未完成（<100%）的 ${latestCarryOver.projects.length} 个项目，补充今日进展即可。`)
  }

  const submit = async () => {
    if (busy) return
    setShowValidation(true)
    setError(''); setNotice('')
    const projectsToSend = draft.projects.filter(project => project.project_name.trim() || project.work_content.trim())
    if (!isFullDayLeave) {
      if (projectsToSend.length === 0) {
        setError('至少填写一个项目的工作内容（整天请假 ≥ 8h 可跳过）')
        return
      }
      for (let i = 0; i < projectsToSend.length; i += 1) {
        const p = projectsToSend[i]!
        if (!p.project_name.trim()) {
          setError(`项目 ${i + 1}：请选择或输入项目名称`)
          return
        }
        const err = validateWorkContent(p.work_content)
        if (err) {
          setError(`项目 ${i + 1}（${p.project_name}）：${err}`)
          return
        }
      }
    }
    if (Number(draft.reimbursement || 0) > 0 && !draft.reimbursement_reason.trim()) {
      setError('报销费用大于 0 时，请填写报销事由')
      return
    }
    setBusy(true)
    try {
      const answer = await dailyApi.submit({ ...draft, projects: projectsToSend })
      draftDirtyRef.current = false
      setShowValidation(false)
      setNotice(answer.action === 'update' ? `已更新 ${draft.report_date} 日报` : `已提交 ${draft.report_date} 日报`)
      const rows = await loadMonth(draft.report_date.slice(0, 7), status?.session)
      reconcile(draft.report_date, rows, status?.session)
    } catch (cause) {
      handleRemoteError(cause, '提交日报失败')
    } finally { setBusy(false) }
  }

  const todayStr = today()
  const calendar = useMemo(() => buildMonthCalendar(calendarMonth, monthRows, todayStr), [calendarMonth, monthRows, todayStr])
  const historyRows = useMemo(
    () => [...monthRows].sort((a, b) => String(b.reportDate ?? '').localeCompare(String(a.reportDate ?? ''))).slice(0, 15),
    [monthRows],
  )
  const factsOf = (name: string) => name.trim() ? activity?.projects.find(entry => entry.project === name.trim()) : undefined
  const filledCount = draft.projects.filter(project => project.project_name.trim() && validateWorkContent(project.work_content) === null).length

  if (loading) return <div className={css.root} data-theme={theme}><p className={css.placeholder}>正在读取日报状态…</p></div>
  if (!status) return <div className={css.root} data-theme={theme}><p className={css.placeholder} role="alert">{error || '无法读取日报状态'}</p></div>
  if (!loggedIn) return <div className={css.root} data-theme={theme}>
    {error && <p className={css.loginErrorBanner} role="alert">{error}</p>}
    <Login initialBase={status.session.base_url} onDone={next => { void syncRemote({ mode: 'open', initialStatus: next, force: true }) }} />
  </div>

  return <div className={css.root} data-theme={theme}>
    <header className={css.top}>
      <div className={css.who}>
        <span className={css.avatar}>{personName(status).slice(0, 1)}</span>
        <span className={css.whoText}>
          <strong>{personName(status)}</strong>
          <span title={status.session.base_url}>{status.session.base_url.replace(/^https?:\/\//, '')}</span>
        </span>
      </div>
      <div className={css.tabs} role="tablist" aria-label="日报页">
        <button type="button" role="tab" aria-selected={tab === 'fill'} className={clsx(css.tab, tab === 'fill' && css.tabActive)} onClick={() => {
          if (tab !== 'fill') {
            setTab('fill')
            void syncRemote({ mode: 'refresh', force: true })
          }
        }}>日报填报</button>
        <button type="button" role="tab" aria-selected={tab === 'guide'} className={clsx(css.tab, tab === 'guide' && css.tabActive)} onClick={() => setTab('guide')}>写作规范</button>
      </div>
      <button type="button" className={css.secondary} disabled={syncing}
        onClick={() => void syncRemote({ mode: 'refresh', showNotice: true, force: true })}
        title="从远程服务器重新同步本月日报、项目列表与写作规范">
        <Icon name="refresh" className={syncing ? css.spin : undefined} />{syncing ? '同步中…' : '刷新'}
      </button>
      {canAsk && <button type="button" className={css.secondary} onClick={() => onAskAi?.({ plugin: 'daily-report' })} title="打开右侧通用 AI 聊天侧边栏">
        <Icon name="spark" />问 AI
      </button>}
      <button type="button" className={css.ghost} onClick={async () => {
        await dailyApi.logout().catch(() => {})
        draftDirtyRef.current = false
        setStatus({ ...status, session: { ...status.session, logged_in: false } })
        setDraft(emptyDraft()); setMonthRows([]); setGuide(''); setActivity(undefined); setLastSyncedAt('')
      }}>退出 / 切换账号</button>
    </header>

    {tab === 'fill' ? <div className={css.body}>
      <main ref={formColumnRef} className={css.formColumn} aria-label="今日日报">
        <section className={css.overview}>
          <div className={css.overviewTop}>
            <label className={css.dateField}><span>日期</span>
              <input type="date" value={draft.report_date} onChange={event => pickDate(event.target.value)} />
            </label>
            <div className={css.stats}>
              <span className={css.stat}><b>{sessions.length}</b>会话</span>
              <span className={css.stat}><b>{activity?.totals.files ?? 0}</b>文件</span>
              <span className={css.stat}><b>{activity?.totals.commits ?? 0}</b>提交</span>
            </div>
            <span className={clsx(css.state, draft.action === 'update' ? css.stateUpdate : css.stateNew)}>
              {draft.action === 'update' ? '今日已有日报，提交会更新它' : '今天还没有提交'}
            </span>
            <button type="button" className={css.secondary} disabled={activityBusy || !workspaceRoot}
              onClick={() => void loadActivity(draft.report_date)}
              title={workspaceRoot ? '读取今天的会话、改动文件和提交' : '这个页面没有绑定工作区'}>
              <Icon name="refresh" />{activityBusy ? '正在汇总…' : '拉取今日工作记录'}
            </button>
          </div>
          {draft.origin === 'ai' && <p className={css.aiBanner} role="status">已载入 AI 生成的日报草稿{status.draft?.updated_at ? ' · ' + status.draft.updated_at : ''}，确认无误后再提交。</p>}
          {notice && <p className={css.ok} role="status">{notice}</p>}
          {error && <p className={css.error} role="alert">{error}</p>}
        </section>

        {activity && <ActivityFacts activity={activity} busy={activityBusy} onAdd={addFacts} onRefresh={() => void loadActivity(draft.report_date)} />}

        <div className={css.projects}>
          {draft.projects.map((project, index) => <ProjectCard key={index} index={index} project={project} options={projects}
            facts={factsOf(project.project_name)}
            showValidation={showValidation && !isFullDayLeave}
            removable={draft.projects.length > 1}
            onChange={next => editDraft(current => ({ ...current, projects: current.projects.map((row, at) => at === index ? next : row) }))}
            onRemove={() => editDraft(current => ({ ...current, projects: current.projects.filter((_, at) => at !== index) }))} />)}
        </div>
        <div className={css.projectBarActions}>
          <button type="button" className={css.addProject} onClick={() => editDraft(current => ({ ...current, projects: [...current.projects, emptyProject()] }))}>
            <Icon name="plus" />添加项目
          </button>
          {latestCarryOver && (
            <button type="button" className={css.carryOverBtn} onClick={applyCarryOver}
              title={`从 ${latestCarryOver.date} 带入进度小于 100% 的功能块`}>
              <Icon name="spark" />续写 {latestCarryOver.date.slice(5)} 未完成（{latestCarryOver.projects.length} 个项目）
            </button>
          )}
        </div>

        <section className={css.common}>
          <Field label="遇到的问题" wide><textarea rows={2} value={draft.problems} placeholder="没有就留空；写清楚卡在哪、需要谁配合。" onChange={event => editDraft(current => ({ ...current, problems: event.target.value }))} /></Field>
          <Field label="加班时长">
            {customOvertime ? (
              <div className={css.inlineInputGroup}>
                <input type="number" min={0} max={24} step={0.5} value={draft.overtime_hours}
                  onChange={event => editDraft(current => ({ ...current, overtime_hours: Number(event.target.value) || 0 }))} />
                <button type="button" className={css.chipButton} onClick={() => { setCustomOvertime(false); editDraft(current => ({ ...current, overtime_hours: 0 })) }}>快捷</button>
              </div>
            ) : (
              <select value={draft.overtime_hours} onChange={event => {
                if (event.target.value === 'custom') {
                  setCustomOvertime(true)
                  editDraft(current => ({ ...current, overtime_hours: current.overtime_hours > 0 ? current.overtime_hours : 1.5 }))
                } else {
                  editDraft(current => ({ ...current, overtime_hours: Number(event.target.value) }))
                }
              }}>
                {OVERTIME_PRESETS.map(h => <option key={h} value={h}>{h === 0 ? '无加班' : `${h} 小时`}</option>)}
                <option value="custom">自定义…</option>
              </select>
            )}
          </Field>
          <Field label="请假时长">
            <select value={draft.leave_hours} onChange={event => editDraft(current => ({ ...current, leave_hours: Number(event.target.value) }))}>
              {LEAVE_PRESETS.map(item => <option key={item.value} value={item.value}>{item.label}</option>)}
            </select>
          </Field>
          <Field label="报销费用（元）">
            <input type="number" min={0} step={0.01} value={draft.reimbursement} onChange={event => {
              const amount = Math.max(0, Number(event.target.value) || 0)
              editDraft(current => ({
                ...current,
                reimbursement: amount,
                ...(amount <= 0 ? { reimbursement_reason: '' } : {}),
              }))
            }} />
          </Field>
          {Number(draft.reimbursement || 0) > 0 && (
            <Field label="报销事由（必填）" wide>
              <input value={draft.reimbursement_reason} placeholder="请填写报销事由，例如：项目出差打车费"
                onChange={event => editDraft(current => ({ ...current, reimbursement_reason: event.target.value }))} />
            </Field>
          )}
          <Field label="备注" wide><textarea rows={2} value={draft.remark} placeholder="选填" onChange={event => editDraft(current => ({ ...current, remark: event.target.value }))} /></Field>
        </section>

        <div className={css.submitBar}>
          <span className={css.submitHint}>
            {isFullDayLeave
              ? `整天请假（${draft.leave_hours}h），可直接提交`
              : filledCount > 0 ? `${filledCount} 个项目格式完整` : '请按「任务 / 完成情况 / 进度%」填好至少一个项目'}
          </span>
          <button type="button" className={css.primary} disabled={busy} onClick={() => void submit()}>
            {busy ? '正在提交…' : draft.action === 'update' ? '更新日报' : '提交日报'}
          </button>
          <button type="button" className={css.ghost} disabled={busy} onClick={async () => {
            setError('')
            try { await dailyApi.saveDraft({ ...draft, origin: 'manual' }); draftDirtyRef.current = false; setNotice('草稿已保存到本机，刷新页面也不会丢。') } catch (cause) { setError(message(cause)) }
          }}>保存草稿</button>
        </div>
      </main>

      <aside className={css.rail} aria-label="填报日历与历史日报">
        <section className={css.calCard} aria-label="本月填报日历">
          <header className={css.calHead}>
            <strong>{calendarMonth.replace('-', ' 年 ')} 月</strong>
            <div className={css.calNav}>
              <button type="button" className={css.iconButton} onClick={() => switchCalendarMonth(-1)} title="上个月" aria-label="上个月">‹</button>
              <button type="button" className={css.iconButton} onClick={() => switchCalendarMonth(1)} title="下个月" aria-label="下个月">›</button>
              <button type="button" className={css.iconButton} disabled={historyBusy}
                onClick={() => void refreshMonth(calendarMonth, { showNotice: true, reconcileDate: draft.report_date.slice(0, 7) === calendarMonth ? draft.report_date : undefined })}
                title="刷新本月记录" aria-label="刷新本月记录"><Icon name="refresh" className={historyBusy ? css.spin : undefined} /></button>
            </div>
          </header>
          <div className={css.calStats}>
            <span>已填 <b>{calendar.filledDays}</b> 天</span>
            {calendar.missedWorkdays > 0 && <span className={css.calStatMissed}>漏填 <b>{calendar.missedWorkdays}</b></span>}
            {calendar.overtimeHours > 0 && <span>加班 <b>{calendar.overtimeHours}h</b></span>}
            {calendar.leaveHours > 0 && <span>请假 <b>{calendar.leaveHours}h</b></span>}
          </div>
          <div className={css.calWeekHead}>
            {WEEKDAYS.map(w => <span key={w}>{w}</span>)}
          </div>
          <div className={css.calGrid}>
            {calendar.cells.map((cell, idx) => cell.date === null ? (
              <span key={idx} className={css.calEmpty} />
            ) : (
              <button
                key={cell.date}
                type="button"
                className={clsx(
                  css.calCell,
                  cell.status === 'filled' && css.calFilled,
                  cell.status === 'missed' && cell.date < todayStr && css.calMissed,
                  cell.status === 'weekend' && css.calWeekend,
                  cell.status === 'future' && css.calFuture,
                  cell.date === todayStr && css.calToday,
                  cell.date === draft.report_date && css.calSelected,
                )}
                title={cell.row ? `${cell.date} · 已填报（点击载入修改）` : cell.date < todayStr && cell.status === 'missed' ? `${cell.date} · 漏填（点击补写）` : `${cell.date}（点击填报）`}
                onClick={() => pickDate(cell.date!, cell.row)}
              >
                <span className={css.calDay}>{cell.day}</span>
                <span className={css.calDots}>
                  {cell.status === 'filled' && <i className={css.dotFilled} />}
                  {cell.status === 'missed' && cell.date < todayStr && <i className={css.dotMissed} />}
                  {cell.overtimeHours > 0 && <i className={css.dotOvertime} />}
                  {cell.leaveHours > 0 && <i className={css.dotLeave} />}
                </span>
              </button>
            ))}
          </div>
          <div className={css.calLegend}>
            <span><i className={css.dotFilled} />已填</span>
            <span><i className={css.dotMissed} />漏填</span>
            <span><i className={css.dotOvertime} />加班</span>
            <span><i className={css.dotLeave} />请假</span>
          </div>
        </section>

        <section className={css.history} aria-label="本月我的日报">
          <header className={css.historyHead}>
            <strong>本月我的日报</strong>
            {lastSyncedAt && <span className={css.syncTime} title="最近一次从远程服务器同步成功的时间">同步于 {lastSyncedAt}</span>}
            <span className={css.historyCount}>{monthRows.length} 条</span>
          </header>
          {historyRows.length === 0 && (
            <p className={css.placeholder}>
              {historyBusy ? '正在同步远程日报数据…' : '本月还没有已提交的日报。'}
            </p>
          )}
          {historyRows.map((row, index) => <HistoryRow key={String(row.id ?? index)} row={row} onLoad={() => pickDate(String(row.reportDate ?? draft.report_date).slice(0, 10), row)} />)}
        </section>
      </aside>
    </div> : <div className={css.guideBody}>
      <div className={css.guideHead}>
        <div>
          <strong>写作规范（Markdown）</strong>
          <p>AI 生成日报前会自动读取这份规范；也可以让数据库插件分析历史日报，再让 AI 帮你把口径写进来。</p>
        </div>
        <button type="button" className={css.primary} disabled={guideSaved} onClick={async () => {
          setError('')
          try { await dailyApi.saveGuide(guide); setGuideSaved(true); setNotice('写作规范已保存。') } catch (cause) { setError(message(cause)) }
        }}>{guideSaved ? '已保存' : '保存规范'}</button>
      </div>
      {notice && <p className={css.ok} role="status">{notice}</p>}
      {error && <p className={css.error} role="alert">{error}</p>}
      {markdownEmbed ? (
        <EmbeddedMarkdownGuide
          ownerId={markdownEmbed.ownerId}
          pageId={markdownEmbed.pageId}
          value={guide}
          saved={guideSaved}
          theme={theme}
          onChange={next => { setGuide(next); setGuideSaved(false) }}
          onSave={async next => {
            setError('')
            setGuide(next)
            try {
              await dailyApi.saveGuide(next)
              setGuideSaved(true)
              setNotice('写作规范已保存。')
            } catch (cause) {
              setError(message(cause))
            }
          }}
        />
      ) : (
        <div className={css.guideSplit}>
          <textarea className={css.guideEditor} value={guide} spellCheck={false}
            placeholder={'# 日报写作规范\n\n- 按项目分段，每段先写任务再写今天完成的事情\n- 不写无法核实的内容'}
            onChange={event => { setGuide(event.target.value); setGuideSaved(false) }} />
          <div className={css.guidePreview}>
            {guide.trim() ? <MarkdownText text={guide} variant="compact" labels={MARKDOWN_LABELS} /> : <p className={css.placeholder}>这份规范还是空的，AI 会先询问你想要的口径。</p>}
          </div>
        </div>
      )}
      <p className={css.guidePath}>文件位置：{status.guide_dir}</p>
    </div>}
  </div>
}
