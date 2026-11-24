import { useCallback, useEffect, useMemo, useState } from 'react'
import clsx from 'clsx'
import { MarkdownText } from '@deepseek-ai/dsh-client-ui-primitives'
import type { PluginPageProps } from '../../plugins/protocol.ts'
import {
  DailyError,
  dailyApi,
  emptyDraft,
  onDraftReady,
  today,
  type DailyDraft,
  type DailyStatus,
  type ProjectOption,
  type ReportRow,
  type TodayActivity,
} from './api.ts'
import css from './style.module.css'

/** The preview only needs chrome for code blocks; footnotes get a heading. */
const MARKDOWN_LABELS = { code: { copyLabel: '复制', copiedLabel: '已复制' }, footnotes: '脚注' }


/** What the server shows as a person's name; the token never reaches this page. */
const personName = (status: DailyStatus | undefined) =>
  status?.session.nick_name || status?.session.user_name || '未知用户'

const message = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

function Field({ label, children, wide }: { label: string; children: React.ReactNode; wide?: boolean }) {
  return <label className={clsx(css.field, wide && css.fieldWide)}>
    <span>{label}</span>
    {children}
  </label>
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
      <button type="submit" className={css.primary} disabled={busy}>{busy ? '正在登录…' : '登录'}</button>
      <p className={css.loginFoot}>令牌保存在本机的插件目录里，不会写进仓库，也不会回显。</p>
    </form>
  </div>
}

/** One project row: name from the server list, plus the day's work. */
function ProjectRow({ index, project, options, onChange, onRemove, removable }: {
  index: number
  project: DailyDraft['projects'][number]
  options: readonly ProjectOption[]
  onChange: (next: DailyDraft['projects'][number]) => void
  onRemove: () => void
  removable: boolean
}) {
  const pick = (name: string) => {
    const match = options.find(option => option.project_name === name)
    onChange({
      ...project,
      project_name: name,
      project_id: match && match.project_id !== null ? Number(match.project_id) : project.project_id,
    })
  }
  return <div className={css.projectCard}>
    <div className={css.projectHead}>
      <span className={css.projectIndex}>项目 {index + 1}</span>
      <input className={css.projectName} value={project.project_name} list="daily-report-projects"
        placeholder="选择或输入项目名称" onChange={event => pick(event.target.value)} />
      {removable && <button type="button" className={css.ghost} onClick={onRemove} title="删除这个项目">删除</button>}
    </div>
    <textarea className={css.workContent} rows={5} value={project.work_content} placeholder="1. 具体任务或功能：…&#10;2. 今日完成工作：…"
      onChange={event => onChange({ ...project, work_content: event.target.value })} />
  </div>
}

export default function DailyReportWorkbench({ workspaceRoot, theme }: PluginPageProps) {
  const [status, setStatus] = useState<DailyStatus>()
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [tab, setTab] = useState<'fill' | 'guide'>('fill')
  const [draft, setDraft] = useState<DailyDraft>(() => emptyDraft())
  const [projects, setProjects] = useState<readonly ProjectOption[]>([])
  const [history, setHistory] = useState<readonly ReportRow[]>([])
  const [historyBusy, setHistoryBusy] = useState(false)
  const [activity, setActivity] = useState<TodayActivity>()
  const [activityBusy, setActivityBusy] = useState(false)
  const [busy, setBusy] = useState(false)
  const [guide, setGuide] = useState('')
  const [guideSaved, setGuideSaved] = useState(true)

  const loggedIn = !!status?.session.logged_in

  const loadHistory = useCallback(async (date: string, userId: string) => {
    setHistoryBusy(true)
    try {
      const page = await dailyApi.reports({ reportDate: date, pageSize: '20', ...(userId ? { userId } : {}) })
      setHistory(page.rows ?? [])
    } catch { setHistory([]) } finally { setHistoryBusy(false) }
  }, [])

  /** A day already filed is an update, not a second report. */
  const reconcile = useCallback((date: string, rows: readonly ReportRow[]) => {
    const mine = rows.find(row => String(row.reportDate ?? '').slice(0, 10) === date)
    setDraft(current => mine && mine.id !== undefined && mine.id !== null
      ? { ...current, action: 'update', report_id: String(mine.id) }
      : { ...current, action: 'create', report_id: null })
    return mine
  }, [])

  const open = useCallback(async (next: DailyStatus) => {
    setStatus(next)
    setDraft(current => next.draft ? { ...next.draft } : current)
    if (!next.session.logged_in) return
    void dailyApi.projects().then(page => setProjects(page.projects), () => {})
    const rows = await dailyApi.reports({ reportDate: next.draft?.report_date ?? today(), pageSize: '20' }).catch(() => ({ rows: [], total: 0 }))
    setHistory(rows.rows ?? [])
    reconcile(next.draft?.report_date ?? today(), rows.rows ?? [])
  }, [reconcile])

  useEffect(() => {
    setLoading(true)
    void dailyApi.status().then(next => open(next), cause => setError(message(cause))).finally(() => setLoading(false))
  }, [open])

  // The AI tool stored a draft and brought this page forward; read it again.
  useEffect(() => onDraftReady(() => {
    void dailyApi.status().then(next => open(next), () => {})
  }), [open])

  useEffect(() => {
    if (!loggedIn) return
    void dailyApi.guide().then(page => setGuide(page.content), () => {})
  }, [loggedIn])

  const pickDate = (date: string) => {
    setDraft(current => ({ ...current, report_date: date }))
    void loadHistory(date, status?.session.user_id ?? '')
      .then(() => dailyApi.reports({ reportDate: date, pageSize: '20', ...(status?.session.user_id ? { userId: status.session.user_id } : {}) }))
      .then(page => reconcile(date, page.rows ?? []), () => {})
  }

  const submit = async () => {
    if (busy) return
    setBusy(true); setError(''); setNotice('')
    try {
      const projectsToSend = draft.projects.filter(project => project.project_name.trim() && project.work_content.trim())
      if (projectsToSend.length === 0) throw new Error('至少填写一个项目的工作内容')
      const answer = await dailyApi.submit({ ...draft, projects: projectsToSend })
      setNotice(answer.action === 'update' ? '已更新今日日报' : '已提交今日日报')
      setDraft(current => ({ ...current, action: 'update', origin: 'manual' }))
      await loadHistory(draft.report_date, status?.session.user_id ?? '')
    } catch (cause) {
      const problem = message(cause)
      setError(problem)
      if (cause instanceof DailyError && cause.authExpired) void dailyApi.status().then(setStatus, () => {})
    } finally { setBusy(false) }
  }

  const fillFromActivity = async () => {
    if (activityBusy || !workspaceRoot) return
    setActivityBusy(true); setError('')
    try { setActivity(await dailyApi.today(workspaceRoot, draft.report_date)) } catch (cause) { setError(message(cause)) } finally { setActivityBusy(false) }
  }

  /** Turn one aggregated project into a form row: facts only, no invented verbs. */
  const useActivityProject = (entry: TodayActivity['projects'][number]) => {
    const lines: string[] = []
    if (entry.sessions.length) lines.push('会话：' + entry.sessions.map(session => session.title).join('；'))
    if (entry.commits.length) lines.push('提交：' + entry.commits.join('；'))
    if (entry.files.length) lines.push('改动文件：' + entry.files.slice(0, 12).join('、') + (entry.files.length > 12 ? ' 等' : ''))
    setDraft(current => ({
      ...current,
      projects: [...current.projects.filter(project => project.project_name.trim() || project.work_content.trim()),
        { project_id: null, project_name: entry.project, work_content: lines.join('\n') }],
    }))
  }

  const historyRows = useMemo(() => history.slice(0, 12), [history])

  if (loading) return <div className={css.root} data-theme={theme}><p className={css.placeholder}>正在读取日报状态…</p></div>
  if (!status) return <div className={css.root} data-theme={theme}><p className={css.placeholder} role="alert">{error || '无法读取日报状态'}</p></div>
  if (!loggedIn) return <div className={css.root} data-theme={theme}>
    <Login initialBase={status.session.base_url} onDone={next => { void open(next) }} />
    {error && <p className={css.error} role="alert">{error}</p>}
  </div>

  return <div className={css.root} data-theme={theme}>
    <header className={css.bar}>
      <span className={css.person}>{personName(status)}</span>
      <span className={css.pill} title={status.session.base_url}>{status.session.base_url.replace(/^https?:\/\//, '')}</span>
      <div className={css.tabs} role="tablist" aria-label="日报页">
        <button type="button" role="tab" aria-selected={tab === 'fill'} className={clsx(css.tab, tab === 'fill' && css.tabActive)} onClick={() => setTab('fill')}>日报填报</button>
        <button type="button" role="tab" aria-selected={tab === 'guide'} className={clsx(css.tab, tab === 'guide' && css.tabActive)} onClick={() => setTab('guide')}>写作规范 (Markdown)</button>
      </div>
      <button type="button" className={css.ghost} onClick={async () => {
        await dailyApi.logout().catch(() => {})
        setStatus({ ...status, session: { ...status.session, logged_in: false } })
        setDraft(emptyDraft()); setHistory([]); setGuide('')
      }}>退出 / 切换账号</button>
    </header>

    {tab === 'fill' ? <div className={css.body}>
      <section className={css.formColumn} aria-label="今日日报">
        <div className={css.toolbar}>
          <label className={css.dateField}><span>日期</span>
            <input type="date" value={draft.report_date} onChange={event => pickDate(event.target.value)} />
          </label>
          <button type="button" className={css.secondary} disabled={activityBusy || !workspaceRoot} onClick={fillFromActivity} title={workspaceRoot ? '读取今天的会话、改动文件和提交' : '这个页面没有绑定工作区'}>
            {activityBusy ? '正在汇总…' : '拉取今日工作记录'}
          </button>
          {draft.action === 'update' && <span className={css.badgeUpdate}>今日已有日报，提交将更新它</span>}
        </div>

        {draft.origin === 'ai' && <p className={css.aiBanner} role="status">已载入 AI 生成的日报草稿{status.draft?.updated_at ? ' · ' + status.draft.updated_at : ''}，确认无误后再提交。</p>}
        {notice && <p className={css.ok} role="status">{notice}</p>}
        {error && <p className={css.error} role="alert">{error}</p>}

        {activity && <div className={css.activity}>
          <div className={css.activityHead}>
            <strong>今日工作事实</strong>
            <span>{activity.date} · {activity.totals.sessions} 个会话 · {activity.totals.files} 个文件 · {activity.totals.commits} 次提交</span>
          </div>
          {activity.projects.length === 0 && <p className={css.placeholder}>今天还没有可汇总的工作记录。</p>}
          {activity.projects.map(entry => <div key={entry.workspace || entry.project} className={css.activityRow}>
            <div>
              <strong>{entry.project}</strong>
              <span>{entry.sessions.length} 会话 · {entry.commits.length} 提交 · {entry.files.length} 文件</span>
            </div>
            <button type="button" className={css.ghost} onClick={() => useActivityProject(entry)}>加入表单</button>
          </div>)}
          {activity.warnings.length > 0 && <p className={css.warn}>{activity.warnings.join('；')}</p>}
        </div>}

        <datalist id="daily-report-projects">
          {projects.map(option => <option key={option.project_name} value={option.project_name} />)}
        </datalist>

        {draft.projects.map((project, index) => <ProjectRow key={index} index={index} project={project} options={projects}
          removable={draft.projects.length > 1}
          onChange={next => setDraft(current => ({ ...current, projects: current.projects.map((row, at) => at === index ? next : row) }))}
          onRemove={() => setDraft(current => ({ ...current, projects: current.projects.filter((_, at) => at !== index) }))} />)}

        <button type="button" className={css.addProject} onClick={() => setDraft(current => ({
          ...current, projects: [...current.projects, { project_id: null, project_name: '', work_content: '' }],
        }))}>+ 添加项目</button>

        <div className={css.commonFields}>
          <Field label="遇到的问题" wide><textarea rows={2} value={draft.problems} onChange={event => setDraft(current => ({ ...current, problems: event.target.value }))} /></Field>
          <Field label="加班时长"><input type="number" min={0} step={0.5} value={draft.overtime_hours} onChange={event => setDraft(current => ({ ...current, overtime_hours: Number(event.target.value) }))} /></Field>
          <Field label="请假时长"><input type="number" min={0} step={0.5} value={draft.leave_hours} onChange={event => setDraft(current => ({ ...current, leave_hours: Number(event.target.value) }))} /></Field>
          <Field label="报销费用"><input type="number" min={0} step={0.01} value={draft.reimbursement} onChange={event => setDraft(current => ({ ...current, reimbursement: Number(event.target.value) }))} /></Field>
          <Field label="报销事由"><input value={draft.reimbursement_reason} onChange={event => setDraft(current => ({ ...current, reimbursement_reason: event.target.value }))} /></Field>
          <Field label="备注" wide><textarea rows={2} value={draft.remark} onChange={event => setDraft(current => ({ ...current, remark: event.target.value }))} /></Field>
        </div>

        <div className={css.submitBar}>
          <button type="button" className={css.primary} disabled={busy} onClick={submit}>{busy ? '正在提交…' : draft.action === 'update' ? '更新今日日报' : '提交日报'}</button>
          <button type="button" className={css.ghost} disabled={busy} onClick={async () => {
            setError('')
            try { await dailyApi.saveDraft({ ...draft, origin: 'manual' }); setNotice('草稿已保存到本机，刷新页面也不会丢。') } catch (cause) { setError(message(cause)) }
          }}>保存草稿</button>
        </div>
      </section>

      <aside className={css.historyColumn} aria-label="历史日报">
        <div className={css.historyHead}>
          <strong>历史日报</strong>
          <button type="button" className={css.ghost} onClick={() => void loadHistory(draft.report_date, status.session.user_id)}>{historyBusy ? '读取中…' : '刷新'}</button>
        </div>
        {historyRows.length === 0 && <p className={css.placeholder}>这一天还没有已提交的日报。</p>}
        {historyRows.map((row, index) => <button key={String(row.id ?? index)} type="button" className={css.historyRow}
          onClick={() => setDraft(current => ({
            report_date: String(row.reportDate ?? current.report_date).slice(0, 10) || current.report_date,
            projects: (row.projects ?? []).map(project => ({
              project_id: project.projectId === undefined || project.projectId === null ? null : Number(project.projectId),
              project_name: String(project.projectName ?? ''),
              work_content: String(project.workContent ?? ''),
            })),
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
          }))}>
          <span className={css.historyDate}>{String(row.reportDate ?? '').slice(0, 10)}</span>
          <span className={css.historyWho}>{String(row.userName ?? row.nickName ?? '')}</span>
          <span className={css.historyProjects}>{(row.projects ?? []).map(project => project.projectName).filter(Boolean).join(' · ') || '未写项目'}</span>
        </button>)}
      </aside>
    </div> : <div className={css.guideBody}>
      <div className={css.guideHead}>
        <p>AI 生成日报前会自动读取这份规范；你可以直接编辑，或用数据库插件分析历史日报后让 AI 帮你总结写入。</p>
        <div>
          <button type="button" className={css.primary} disabled={guideSaved} onClick={async () => {
            setError('')
            try { await dailyApi.saveGuide(guide); setGuideSaved(true); setNotice('写作规范已保存。') } catch (cause) { setError(message(cause)) }
          }}>{guideSaved ? '已保存' : '保存规范'}</button>
        </div>
      </div>
      <div className={css.guideSplit}>
        <textarea className={css.guideEditor} value={guide} spellCheck={false}
          placeholder="# 日报写作规范&#10;&#10;- 按项目分段，每段先写任务再写今天完成的事情&#10;- 不写无法核实的内容"
          onChange={event => { setGuide(event.target.value); setGuideSaved(false) }} />
        <div className={css.guidePreview}>
          {guide.trim() ? <MarkdownText text={guide} variant="compact" labels={MARKDOWN_LABELS} /> : <p className={css.placeholder}>这份规范还是空的，AI 会先询问你想要的口径。</p>}
        </div>
      </div>
      <p className={css.guidePath}>文件位置：{status.guide_dir}</p>
    </div>}
  </div>
}
