import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import clsx from 'clsx'
import { api, type Project, type SessionProgram, type SessionTurn, type SkillPackage, type Task, type WorkflowDraftSummary, type WorkflowDraftView } from '../api.ts'
import { taskTitle } from '../sidebarModel.ts'
import { callName, clock, draftFileOf, duration, plainText, programEntries, programName, projectName, sessionGroups, statusOf, turnTone, turnWindow, type SessionGroup, type Tone } from '../workbenchModel.ts'
import { CodeView, DiffView, diffStats } from './WorkbenchCode.tsx'
import { DraftChat } from './WorkbenchChat.tsx'
import { Icon, type IconName } from './WorkbenchIcon.tsx'
import styles from './SkillWorkbench.module.css'

const TONE_ICON: Record<Tone, IconName> = { ok: 'check', error: 'close', warn: 'bars', live: 'spark', muted: 'minus' }
const TONE_CLASS: Record<Tone, string | undefined> = { ok: styles.toneOk, error: styles.toneError, warn: styles.toneWarn, live: styles.toneLive, muted: styles.toneMuted }
/** Sessions a group shows before "显示其余"; a long project must not push the other groups away. */
const GROUP_ROWS = 6
/** Turn tabs shown at once, so they fit one line even in the narrow layout. */
const TURN_TABS = 5
/** Earlier turns the expanded request shows. */
const EARLIER_TURNS = 5

const stored = <T,>(key: string, fallback: T): T => {
  try { const raw = localStorage.getItem(key); return raw === null ? fallback : JSON.parse(raw) as T } catch { return fallback }
}
const store = (key: string, value: unknown) => { try { localStorage.setItem(key, JSON.stringify(value)) } catch { /* storage unavailable */ } }
const message = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)
const lineCount = (source: string) => source.replace(/\r\n/g, '\n').replace(/\n$/, '').split('\n').length

/** A short, recognisable account of a run: the methods it called, most first-seen first. */
function callSummary(program: SessionProgram, limit = 3): string {
  if (program.calls.length === 0) return '没有调用工具或函数'
  const shown = program.calls.slice(0, limit).map(call => `${callName(call.name)}${call.count > 1 ? ` ×${call.count}` : ''}`)
  return program.calls.length > limit ? `${shown.join(' · ')} 等 ${program.calls.length} 种` : shown.join(' · ')
}

function Status({ tone, label }: { tone: Tone; label: string }) {
  return <span className={clsx(styles.pill, TONE_CLASS[tone])}>{label}</span>
}

/** Recorded workflows of real sessions, and drafts of them that an Agent conversation edits. */
export function SkillWorkbench({ tasks, projects, defaultWorkspace, onOpenChat, onPackages }: {
  tasks: readonly Task[]
  projects: readonly Project[]
  defaultWorkspace: string
  onOpenChat: (id: string) => void
  onPackages: () => void
}) {
  const [query, setQuery] = useState('')
  const [searchOpen, setSearchOpen] = useState(false)
  const [selectedId, setSelectedId] = useState(() => stored<string | null>('skill-workbench:session', null) ?? undefined)
  const [closed, setClosed] = useState(() => stored<Record<string, boolean>>('skill-workbench:closed', {}))
  const [showAll, setShowAll] = useState<Record<string, boolean>>({})
  const [packages, setPackages] = useState<readonly SkillPackage[]>([])
  const [drafts, setDrafts] = useState<readonly WorkflowDraftSummary[]>([])
  const [turns, setTurns] = useState<readonly SessionTurn[]>()
  const [loadError, setLoadError] = useState('')
  const [selectedSeq, setSelectedSeq] = useState<number>()
  const [requestOpen, setRequestOpen] = useState(false)
  const [draftId, setDraftId] = useState<string>()
  const [draft, setDraft] = useState<WorkflowDraftView>()
  const [view, setView] = useState<'original' | 'draft' | 'diff'>('original')
  const [filePath, setFilePath] = useState<string>()
  const [listOpen, setListOpen] = useState(false)
  const [contextOpen, setContextOpen] = useState(false)
  const [chatId, setChatId] = useState<string>()
  const [chatOpen, setChatOpen] = useState(false)
  const [chatExpanded, setChatExpanded] = useState(false)
  const [chatRunning, setChatRunning] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [copied, setCopied] = useState(false)
  const searchRef = useRef<HTMLInputElement>(null)
  const cardsRef = useRef<HTMLDivElement>(null)

  const refreshDrafts = useCallback(() => api.workflowDrafts().then(setDrafts, () => {}), [])
  useEffect(() => {
    void api.skillPackages().then(result => setPackages(result.packages), () => {})
    void refreshDrafts()
  }, [refreshDrafts])

  const hidden = useMemo(() => new Set(drafts.flatMap(item => item.chat_task ? [item.chat_task] : [])), [drafts])
  const groups = useMemo(() => sessionGroups(tasks, projects, packages, defaultWorkspace, hidden, query), [tasks, projects, packages, defaultWorkspace, hidden, query])
  const task = tasks.find(item => item.task_id === selectedId)
  const running = task?.status === 'running' || task?.status === 'cancelling'
  const plugin = task?.plugin_id ? packages.find(item => item.id === task.plugin_id) : undefined
  const pluginTitle = task?.plugin_id ? plugin?.title || task.plugin_id : undefined

  // The selected session's programs; re-read while it is still working.
  useEffect(() => {
    setTurns(undefined); setLoadError(''); setSelectedSeq(undefined); setFilePath(undefined); setView('original')
    setRequestOpen(false); setError(''); setContextOpen(false)
    if (!selectedId) return
    store('skill-workbench:session', selectedId)
    let active = true
    void api.sessionPrograms(selectedId).then(result => { if (active) setTurns(result.turns) }, cause => { if (active) setLoadError(message(cause)) })
    return () => { active = false }
  }, [selectedId])
  useEffect(() => {
    if (!selectedId || !running) return
    let active = true
    const timer = window.setInterval(() => { void api.sessionPrograms(selectedId).then(result => { if (active) setTurns(result.turns) }, () => {}) }, 4000)
    return () => { active = false; window.clearInterval(timer) }
  }, [selectedId, running])

  // The session's newest draft is the one being edited.
  useEffect(() => {
    const own = drafts.filter(item => item.source_task === selectedId)
    setDraftId(current => own.some(item => item.id === current) ? current : own[0]?.id)
  }, [drafts, selectedId])
  const reloadDraft = useCallback(() => {
    if (draftId) void api.workflowDraft(draftId).then(setDraft, () => {})
  }, [draftId])
  useEffect(() => {
    setDraft(undefined); setChatId(undefined); setChatRunning(false)
    if (!draftId) return
    let active = true
    void api.workflowDraft(draftId).then(value => {
      if (!active) return
      setDraft(value)
      setView(current => current === 'original' ? 'draft' : current)
      // The draft's conversation is started on first use; older drafts get their notes then too.
      void api.workflowDraftChat(draftId).then(id => { if (active) setChatId(id) }, cause => { if (active) setError(message(cause)) })
    }, cause => { if (active) setError(message(cause)) })
    return () => { active = false }
  }, [draftId])
  // What the conversation writes shows while it works.
  useEffect(() => {
    if (!chatRunning) return
    const timer = window.setInterval(reloadDraft, 2500)
    return () => window.clearInterval(timer)
  }, [chatRunning, reloadDraft])

  const entries = useMemo(() => programEntries(turns ?? []), [turns])
  const current = entries.find(entry => entry.program.seq === selectedSeq) ?? entries.at(-1)
  const programTurns = (turns ?? []).filter(turn => turn.programs.length > 0 || turn.rejected.length > 0)
  const turnIndex = Math.max(0, programTurns.findIndex(turn => turn.turn === current?.turn.turn))
  const currentTurn = programTurns[turnIndex]
  const files = draft?.files ?? []
  const file = filePath ? files.find(item => item.path === filePath) : current ? files.find(item => item.path === draftFileOf(current.program.seq)) : undefined
  const before = filePath ? file?.original ?? null : file?.original ?? current?.program.source ?? null
  const after = file ? file.current : null
  const shownView = !file ? 'original' : view === 'original' && before === null ? 'draft' : view
  const shownSource = shownView === 'original' ? before ?? '' : after ?? ''
  const shownPath = filePath ?? (current ? file?.path ?? current.program.path ?? `program-${current.program.seq}.js` : '')
  const changed = files.filter(item => item.status !== 'unchanged')
  const stats = file ? diffStats(before, after) : undefined
  const rejectedCount = (turns ?? []).reduce((sum, turn) => sum + turn.rejected.length + turn.programs.reduce((count, program) => count + program.rejected.length, 0), 0)
  const draftLabel = draft ? `${draft.draft.id.slice(0, 6)}${draft.draft.created_at ? `（${clock(draft.draft.created_at)} 创建）` : ''}` : ''

  const selectSession = (id: string) => { setSelectedId(id); setListOpen(false) }
  const selectProgram = (seq: number) => { setSelectedSeq(seq); setFilePath(undefined) }
  const selectTurn = (turn: SessionTurn) => { setRequestOpen(false); const last = turn.programs.at(-1); if (last) selectProgram(last.seq) }
  const showFile = useCallback((path: string) => {
    const seq = Number(/^programs\/program-(\d+)\.js$/.exec(path)?.[1] ?? Number.NaN)
    if (entries.some(entry => entry.program.seq === seq)) { setSelectedSeq(seq); setFilePath(undefined) } else setFilePath(path)
    setView('diff')
  }, [entries])
  const toggle = (key: string) => setClosed(state => { const next = { ...state, [key]: !state[key] }; store('skill-workbench:closed', next); return next })
  const createDraft = async () => {
    if (!selectedId || busy) return
    setBusy(true); setError('')
    try {
      const created = await api.createWorkflowDraft(selectedId)
      await refreshDrafts()
      setDraftId(created.id)
      setView('draft')
      setChatOpen(true)
    } catch (cause) { setError(message(cause)) } finally { setBusy(false) }
  }
  const copy = () => {
    void navigator.clipboard.writeText(shownSource).then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 1200) }, () => {})
  }

  const sessionRow = (item: Task) => <button key={item.task_id} type="button" title={taskTitle(item)} onClick={() => selectSession(item.task_id)}
    className={clsx(styles.sessionRow, item.task_id === selectedId && styles.sessionActive)}>
    <Icon name="file" size={18} />
    <span className={styles.sessionTitle}>{taskTitle(item)}</span>
    {drafts.some(entry => entry.source_task === item.task_id) && <span className={styles.draftMark}>草稿</span>}
    {(item.status === 'running' || item.status === 'cancelling') && <span className={styles.spinner} aria-label="运行中" />}
  </button>
  const rowsOf = (key: string, rows: readonly Task[]) => {
    const searching = !!query.trim()
    const limit = showAll[key] || searching ? rows.length : GROUP_ROWS
    const selected = rows.findIndex(item => item.task_id === selectedId)
    return <>
      {rows.slice(0, limit).map(sessionRow)}
      {selected >= limit && sessionRow(rows[selected]!)}
      {rows.length > limit && <button type="button" className={styles.moreRow} onClick={() => setShowAll(state => ({ ...state, [key]: true }))}>显示其余 {rows.length - limit} 个会话</button>}
      {showAll[key] && !searching && rows.length > GROUP_ROWS && <button type="button" className={styles.moreRow} onClick={() => setShowAll(state => ({ ...state, [key]: false }))}>收起</button>}
    </>
  }
  const group = (entry: SessionGroup, icon: IconName, labelled: boolean) => {
    const open = !closed[entry.key] || !!query.trim()
    return <div key={entry.key} className={styles.group}>
      <button type="button" className={styles.groupHead} title={entry.detail} aria-expanded={open} onClick={() => toggle(entry.key)}>
        <Icon name={icon} size={19} />
        <span className={styles.groupTitle}>{entry.title}{labelled && <small> · {entry.detail}</small>}</span>
        <span className={styles.groupCount}>{entry.tasks.length}</span>
        <span className={clsx(styles.chevron, open && styles.chevronOpen)}><Icon name="chevron" size={14} /></span>
      </button>
      {open && <div className={styles.groupRows}>{rowsOf(entry.key, entry.tasks)}</div>}
    </div>
  }
  const section = (key: string, label: string, body: ReactNode) => {
    const open = !closed[key] || !!query.trim()
    return <section className={styles.listSection}>
      <button type="button" className={styles.sectionToggle} aria-expanded={open} onClick={() => toggle(key)}>
        <span>{label}</span><span className={clsx(styles.chevron, open && styles.chevronOpen)}><Icon name="chevron" size={13} /></span>
      </button>
      {open && body}
    </section>
  }
  const empty = groups.projects.length === 0 && groups.plugins.length === 0 && groups.chats.length === 0

  const program = current?.program
  const status = program ? statusOf(program) : undefined
  const callTotal = program ? program.calls.reduce((sum, call) => sum + call.count, 0) : 0
  const request = current?.turn.request ?? null
  const earlier = (turns ?? []).filter(turn => current && turn.turn < current.turn.turn).slice(-EARLIER_TURNS)
  const earlierHidden = (turns ?? []).filter(turn => current && turn.turn < current.turn.turn).length - earlier.length
  const requestLong = !!request && (request.text.length > 140 || request.text.includes('\n'))
  const rewriteIndex = (seq: number | null) => seq === null ? undefined : entries.find(entry => entry.program.seq === seq)
  const lastOfTurn = !!current && current.turn.programs.at(-1)?.seq === program?.seq
  const cards = currentTurn?.programs ?? []
  const name = program ? programName(program) : null

  // The strip scrolls sideways; keep the selected run in view.
  useEffect(() => {
    cardsRef.current?.querySelector<HTMLElement>('[aria-current="true"]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
  }, [program?.seq])

  return <div className={clsx(styles.root, listOpen && styles.listOpen, contextOpen && styles.contextOpen, chatOpen && draft && styles.chatSpace)}>
    <aside className={styles.list} aria-label="工作台会话">
      <div className={styles.listHead}>
        <strong>会话</strong>
        <button type="button" className={styles.iconButton} title="搜索会话" aria-label="搜索会话" onClick={() => { setSearchOpen(value => !value); window.setTimeout(() => searchRef.current?.focus(), 0) }}><Icon name="search" /></button>
        <button type="button" className={styles.iconButton} title="已安装的插件" aria-label="已安装的插件" onClick={onPackages}><Icon name="plug" /></button>
      </div>
      {(searchOpen || query) && <label className={styles.search}><Icon name="search" size={15} /><input ref={searchRef} value={query} onChange={event => setQuery(event.target.value)}
        onKeyDown={event => { if (event.key === 'Escape') { setQuery(''); setSearchOpen(false) } }} placeholder="搜索会话标题和内容" aria-label="搜索会话" /></label>}
      <div className={styles.listScroll}>
        {groups.projects.length > 0 && section('section:projects', '我的项目', groups.projects.map(entry => group(entry, 'folder', false)))}
        {groups.plugins.length > 0 && section('section:plugins', '插件会话', groups.plugins.map(entry => group(entry, 'plug', true)))}
        {groups.chats.length > 0 && section('section:chats', '普通聊天', <div className={styles.groupRows}>{rowsOf('chats', groups.chats)}</div>)}
        {empty && <p className={styles.hint}>{query.trim() ? '没有匹配的会话' : '还没有会话'}</p>}
      </div>
    </aside>
    <div className={styles.scrim} onClick={() => { setListOpen(false); setContextOpen(false) }} />

    <main className={styles.main}>
      <div className={styles.narrowBar}>
        <button type="button" className={clsx(styles.ghostButton, styles.listToggle)} onClick={() => setListOpen(true)}><Icon name="list" size={16} />会话</button>
        {task && <button type="button" className={clsx(styles.ghostButton, styles.contextToggle)} onClick={() => setContextOpen(true)}><Icon name="panel" size={16} />流程上下文</button>}
      </div>
      {!task ? <div className={styles.empty}>
        <span className={styles.emptyIcon}><Icon name="code" size={26} /></span>
        <strong>选择一个会话</strong>
        <span>查看它运行过的流程代码、当时的用户请求和运行结果，再把流程整理成可复用的技能草稿。</span>
      </div> : <>
        <header className={styles.header}>
          <span className={styles.headerIcon}><Icon name="file" size={24} /></span>
          <h2 title={taskTitle(task)}>{taskTitle(task)}</h2>
          {task.plugin_id ? <span className={styles.badge} title={plugin ? undefined : '这个插件现在没有连接或没有安装，名称显示为它的标识'}><Icon name="plug" size={15} />{pluginTitle} · {plugin?.origin === 'builtin' ? '内置插件' : plugin?.origin === 'external' ? '外部插件' : '插件'}</span>
            : task.kind === 'chat' ? <span className={styles.badge}><Icon name="chat" size={15} />普通聊天</span>
              : <span className={styles.badge} title={task.workspace ?? defaultWorkspace}><Icon name="folder" size={15} />{projectName(task.workspace ?? defaultWorkspace, projects)}</span>}
          <button type="button" className={styles.textButton} onClick={() => onOpenChat(task.task_id)}>打开原会话<Icon name="external" size={14} /></button>
        </header>
        {error && <p role="alert" className={styles.error}>操作失败：{error}</p>}

        {turns === undefined ? <div className={styles.loading}>{loadError ? `读取流程记录失败：${loadError}` : '正在读取流程记录…'}</div>
          : entries.length === 0 ? <div className={styles.noFlows}>
            <span className={styles.emptyIcon}><Icon name="code" size={24} /></span>
            <strong>这个会话没有运行过流程代码</strong>
            <span>{rejectedCount > 0 ? `有 ${rejectedCount} 次交出的程序没有通过编译，没有运行。` : '直接回答或在对话里直接用工具完成的工作，不会留下流程代码。'}</span>
          </div> : current && program && status && <>
            <section className={styles.request}>
              <span className={styles.avatar}><Icon name="user" size={19} /></span>
              <div className={styles.requestBody}>
                <div className={styles.requestHead}><strong>用户请求</strong><span>第 {current.turn.turn} 轮{request ? ` · ${clock(request.time)}` : ''}</span></div>
                <p className={clsx(styles.requestText, !requestOpen && styles.clamp2)}>{request?.text.trim() || (request ? '（这条消息只有附件）' : '（会话里没有记录这一轮的用户消息）')}</p>
                {request && request.attachments.length > 0 && <div className={styles.attachments}>{request.attachments.map((item, index) => <span key={index}><Icon name="clip" size={13} />{item.name}</span>)}</div>}
                {requestOpen && earlier.length > 0 && <div className={styles.earlier}>
                  <h5>此前的对话{earlierHidden > 0 ? `（更早的 ${earlierHidden} 轮见完整会话）` : ''}</h5>
                  {earlier.map(turn => <div key={turn.turn} className={styles.earlierTurn}>
                    <span>第 {turn.turn} 轮</span>
                    <p><b>用户</b>{turn.request?.text.trim() || '（没有文字）'}</p>
                    {turn.reply && <p><b>AI</b>{plainText(turn.reply.text)}</p>}
                  </div>)}
                </div>}
                {(requestLong || earlier.length > 0) && <button type="button" className={styles.linkButton} onClick={() => setRequestOpen(value => !value)}>
                  {requestOpen ? '收起' : earlier.length > 0 ? '展开完整上下文' : '展开完整内容'}<span className={clsx(styles.chevron, requestOpen && styles.chevronUp)}><Icon name="chevronDown" size={14} /></span>
                </button>}
              </div>
            </section>

            <div className={styles.sectionHead}>
              <span className={styles.sectionIcon}><Icon name="code" size={21} /></span>
              <h3>本会话的流程代码</h3>
              <span className={styles.sectionMeta}>{programTurns.length} 轮 · {entries.length} 段程序{rejectedCount > 0 ? ` · ${rejectedCount} 次提交未通过编译` : ''}</span>
            </div>
            {programTurns.length > 1 && <div className={styles.turnBar} role="tablist" aria-label="轮次">
              {turnWindow(programTurns.length, turnIndex, TURN_TABS).map((index, position) => {
                if (index === 'gap') return <span key={`gap-${position}`} className={styles.turnGap}>…</span>
                const turn = programTurns[index]!
                return <button key={turn.turn} type="button" role="tab" aria-selected={index === turnIndex} title={turn.request?.text.slice(0, 120) || `第 ${turn.turn} 轮`}
                  className={clsx(styles.turnTab, index === turnIndex && styles.turnTabActive)} onClick={() => selectTurn(turn)}>
                  <span className={clsx(styles.dot, TONE_CLASS[turnTone(turn)])} />第 {turn.turn} 轮<span className={styles.turnCount}>{turn.programs.length}</span>
                </button>
              })}
            </div>}
            <div ref={cardsRef} className={styles.cards} aria-label={`第 ${current.turn.turn} 轮的程序`}>
              {cards.map((item, index) => {
                const tone = statusOf(item)
                const of = rewriteIndex(item.rewrite_of)
                const label = programName(item)
                const active = item.seq === program.seq && !filePath
                return <button key={item.seq} type="button" onClick={() => selectProgram(item.seq)} aria-current={active}
                  title={[label && `第 ${index + 1} 段：${label}`, item.note && `AI 交出时说：${item.note}`, item.calls.map(call => `${call.name} ×${call.count}`).join('\n')].filter(Boolean).join('\n\n')}
                  className={clsx(styles.card, active && styles.cardActive)}>
                  <span className={clsx(styles.cardIcon, TONE_CLASS[tone.tone])}><Icon name={TONE_ICON[tone.tone]} size={18} /></span>
                  <span className={styles.cardMain}>
                    <span className={styles.cardTop}><strong>{label ?? `第 ${index + 1} 段`}</strong><Status tone={tone.tone} label={tone.label} /></span>
                    <span className={styles.cardCalls}>{label && <em>第 {index + 1} 段</em>}<code>{callSummary(item, 2)}</code></span>
                    {(of || item.rejected.length > 0) && <span className={styles.cardNote}>
                      {of && `改写自第 ${of.index} 段`}{of && item.rejected.length > 0 && ' · '}{item.rejected.length > 0 && `交出前 ${item.rejected.length} 次未通过编译`}
                    </span>}
                  </span>
                </button>
              })}
            </div>

            <section className={styles.codePanel}>
              <div className={styles.codeHead}>
                <div className={styles.codeTitle}>
                  <h4 title={filePath ?? name ?? undefined}>{filePath ?? name ?? `第 ${current.turn.turn} 轮 · 第 ${current.index} 段`}</h4>
                  <div className={styles.codeMeta}>
                    {filePath ? <span>{file?.status === 'added' ? '草稿里新增的文件' : file?.status === 'deleted' ? '草稿里已删除' : '草稿文件'}</span> : <>
                      <Status tone={status.tone} label={status.label} />
                      {name && <span>第 {current.turn.turn} 轮 · 第 {current.index} 段</span>}
                      {program.ended_at !== null && <span>用时 {duration(program.ended_at - program.time)}</span>}
                      {callTotal > 0 && <span>调用 {callTotal} 次</span>}
                      <span title={`${clock(program.time)} 开始 · ${lineCount(program.source)} 行`}>{clock(program.time, false)}</span>
                    </>}
                  </div>
                  {!filePath && program.note && <p className={styles.codeNote} title={program.note}>AI 交出时说：{program.note}</p>}
                </div>
                <div className={styles.codeActions}>
                  {draft ? <>
                    <button type="button" disabled title="插件页面工具的真实测试尚未接入，现在不能在这里运行"><Icon name="play" size={15} />运行测试<span className={styles.soon}>未接入</span></button>
                    <button type="button" className={styles.primary} disabled title="技能的保存、发布和插件侧发现尚未实现"><Icon name="box" size={15} />保存为技能包<span className={styles.soon}>未接入</span></button>
                  </> : <button type="button" className={styles.primary} disabled={busy} onClick={() => void createDraft()}><Icon name="box" size={15} />{busy ? '正在创建…' : '创建技能草稿'}</button>}
                </div>
              </div>
              <div className={styles.tabs} role="tablist">
                <button type="button" role="tab" aria-selected={shownView === 'original'} disabled={before === null} className={clsx(shownView === 'original' && styles.tabActive)} onClick={() => setView('original')}>原始流程</button>
                <button type="button" role="tab" aria-selected={shownView === 'draft'} disabled={!file} title={file ? undefined : '创建草稿后可以查看和修改'} className={clsx(shownView === 'draft' && styles.tabActive)} onClick={() => setView('draft')}>当前草稿</button>
                <button type="button" role="tab" aria-selected={shownView === 'diff'} disabled={!file} title={file ? undefined : '创建草稿后可以对照改动'} className={clsx(shownView === 'diff' && styles.tabActive)} onClick={() => setView('diff')}>
                  <Icon name="external" size={14} />查看差异{stats && (stats.added > 0 || stats.removed > 0) && <span className={styles.stats}><b>+{stats.added}</b><i>−{stats.removed}</i></span>}
                </button>
                <span className={styles.tabSpacer} />
                {pluginTitle && <span className={styles.owner}>所属插件<Icon name="plug" size={14} />{pluginTitle}</span>}
              </div>
              {draft && changed.length > 0 && <div className={styles.fileStrip}>
                <span>草稿改动</span>
                {changed.map(item => <button key={item.path} type="button" onClick={() => showFile(item.path)} className={clsx(item.path === file?.path && styles.fileActive)}>
                  {item.path}<b>{item.status === 'added' ? '新增' : item.status === 'deleted' ? '删除' : '已改'}</b>
                </button>)}
              </div>}
              <div className={styles.sourceHead}>
                <span className={styles.sourceTitle}><Icon name="code" size={16} />代码<small>（JavaScript）</small></span>
                <span className={styles.sourcePath} title={shownPath}>{shownView === 'diff' ? '原始流程 → 当前草稿' : shownView === 'draft' ? shownPath : '会话记录里的原样'}</span>
                <button type="button" onClick={copy} disabled={shownView === 'diff'}><Icon name={copied ? 'check' : 'copy'} size={15} />{copied ? '已复制' : '复制代码'}</button>
              </div>
              {file?.binary && shownView !== 'original' ? <div className={styles.diffEmpty}>这个文件是二进制文件或太大，不在这里显示。</div>
                : shownView === 'diff' ? <DiffView before={before} after={after} />
                  : shownView === 'draft' && after === null ? <div className={styles.diffEmpty}>这个文件已从草稿里删除。</div>
                    : <CodeView source={shownSource} path={shownPath} />}
            </section>
            <p className={styles.footHint}>{draft
              ? `草稿保存在 ${draft.draft.workspace.replace(/^\\\\\?\\/, '')}，与原会话分开；原始流程不会被修改。`
              : '创建草稿会复制这个会话的全部主流程、模块和运行记录，并为草稿开一个独立的 AI 对话；原会话不受影响。'}</p>
          </>}
      </>}
    </main>

    {task && <aside className={styles.context} aria-label="流程上下文">
      <div className={styles.contextHead}>
        <Icon name="file" size={20} /><h3>流程上下文</h3>
        <button type="button" className={clsx(styles.iconButton, styles.contextClose)} onClick={() => setContextOpen(false)} aria-label="关闭流程上下文"><Icon name="close" size={16} /></button>
      </div>
      <div className={styles.contextBody}>
        {!program || !status ? <p className={styles.hint}>{turns === undefined ? '正在读取…' : '这个会话没有可展示的流程。'}</p> : <>
          <div className={styles.contextBlock}>
            <div className={styles.contextLabel}><span className={styles.avatarSmall}><Icon name="user" size={17} /></span><h4>关联用户消息</h4><span>第 {current.turn.turn} 轮</span></div>
            <div className={styles.quote}><p className={styles.clamp6}>{request?.text.trim() || '（没有文字）'}</p></div>
            {request && <time className={styles.time}>{clock(request.time)}</time>}
            {request?.page_selection && <details className={styles.details}>
              <summary>插件页面附加的数据（不是用户原话）</summary>
              <pre>{request.page_selection}</pre>
            </details>}
          </div>
          <div className={styles.contextBlock}>
            <div className={styles.contextLabel}><span className={styles.avatarSmall}><Icon name="file" size={17} /></span><h4>运行结果</h4></div>
            <div className={clsx(styles.outcome, TONE_CLASS[status.tone])}><span className={styles.outcomeIcon}><Icon name={TONE_ICON[status.tone]} size={15} /></span><strong>{status.words}</strong></div>
            <p className={styles.outcomeMeta}>
              {program.ended_at !== null ? `用时 ${duration(program.ended_at - program.time)}` : '没有结束时间'}
              {callTotal > 0 ? ` · 调用 ${callTotal} 次` : ''}
            </p>
            {program.error && <p className={styles.errorLine}>{program.error.line ? `第 ${program.error.line} 行：` : ''}{program.error.message}</p>}
            {program.status === 'rewritten' && <p className={styles.outcomeNote}>{program.rewritten_by !== null
              ? <>错误已交回 AI，它改写成了 <button type="button" className={styles.inlineLink} onClick={() => selectProgram(program.rewritten_by!)}>第 {rewriteIndex(program.rewritten_by)?.index} 段</button>。</>
              : '错误已交回 AI，它没有再交出新的程序。'}</p>}
            {program.calls.length > 0 && <div className={styles.callChips}>{program.calls.map(call => <span key={`${call.kind}:${call.name}`} title={call.name}
              className={clsx(call.kind !== 'tool' && styles.callAi)}>{callName(call.name)}{call.kind === 'virtual' ? '（AI）' : call.kind === 'module' ? '（模块）' : ''}{call.count > 1 && <b>×{call.count}</b>}</span>)}</div>}
            {program.returned && <p className={styles.returned}><b>返回</b>{program.returned}</p>}
            {program.output && <details className={styles.details}>
              <summary>{program.status === 'rewritten' ? '出错前的程序输出' : '程序输出'}（{program.output_chars.toLocaleString()} 字符）</summary>
              <pre>{program.output}</pre>
              {program.output_chars > program.output.length && <p className={styles.hint}>这里只显示前 {program.output.length.toLocaleString()} 字符，完整内容在原会话里。</p>}
            </details>}
            {program.rejected.length > 0 && <details className={styles.details}>
              <summary>交出前 {program.rejected.length} 次未通过编译（没有运行）</summary>
              {program.rejected.map(item => <div key={item.seq} className={styles.rejected}>
                {item.diagnostics.map((diagnostic, index) => <p key={index}>{diagnostic.line ? `第 ${diagnostic.line} 行：` : ''}{diagnostic.message}</p>)}
                {item.source && <pre>{item.source}</pre>}
              </div>)}
            </details>}
            {lastOfTurn && current.turn.reply && <div className={styles.reply}>
              <span>AI 随后告诉用户</span>
              <p className={styles.clamp4} title={plainText(current.turn.reply.text)}>{plainText(current.turn.reply.text)}</p>
            </div>}
          </div>
        </>}
        <button type="button" className={styles.openChat} onClick={() => onOpenChat(task.task_id)}><Icon name="external" size={16} /><span>查看完整会话</span><Icon name="chevron" size={16} /></button>
        <div className={styles.info}><Icon name="info" size={17} /><span>{draft
          ? '可以在右下角与 AI 对话修改这份草稿。插件真实测试、技能发布和插件侧发现尚未实现。'
          : '创建技能草稿后，可与 AI 对话优化流程。插件真实测试、技能发布和插件侧发现尚未实现。'}</span></div>
      </div>
    </aside>}

    {/* Collapsed, the conversation stays mounted so a running turn keeps updating the draft. */}
    {task && draft && chatId && <DraftChat key={chatId} chatId={chatId} draftLabel={draftLabel} draftFolder={draft.draft.workspace} source={task}
      hidden={!chatOpen} expanded={chatExpanded} onExpand={() => setChatExpanded(value => !value)} onClose={() => setChatOpen(false)}
      onRunning={setChatRunning} onSettled={reloadDraft} onShowFile={showFile} />}
    {task && draft && !chatOpen && <button type="button" className={styles.chatToggle} onClick={() => setChatOpen(true)} disabled={!chatId} aria-expanded={false}>
      <Icon name="chat" size={17} />与 AI 协作{chatRunning && <span className={styles.spinner} />}
    </button>}
  </div>
}
