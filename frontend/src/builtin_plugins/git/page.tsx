import { useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import type { PluginPageProps } from '../../plugins/protocol.ts'
import { usePluginTools } from '../../plugins/react.tsx'
import { Icon } from '../../components/WorkbenchIcon.tsx'
import {
  absoluteTime, layoutGraph, parsePatch, relativeTime,
  type GitCommitDetail, type GitFile, type GitOverview, type GitPatch, type GraphRow,
} from './graph.ts'
import { FileTree } from './FileTree.tsx'
import { CodeView, type GitFileText } from './CodeView.tsx'
import { useCordis } from '../../cordis/react.tsx'
import css from './style.module.css'

type View = 'graph' | 'changes' | 'branches' | 'stashes'
/** What the right panel shows: a commit, or the working tree when hash is null. */
type Selection = { hash: string | null }

const LANE_COLORS = ['#1d9e75', '#3b82f6', '#d97706', '#8b5cf6', '#ec4899', '#0ea5e9', '#65a30d', '#ef4444']
const ROW = 44, LANE = 18, DOT = 7
const laneX = (lane: number) => 14 + lane * LANE

function hue(text: string) { let h = 0; for (const c of text) h = (h * 31 + c.charCodeAt(0)) >>> 0; return h % 360 }
function Avatar({ name, email }: { name: string; email: string }) {
  return <span className={css.avatar} style={{ background: `hsl(${hue(email || name)} 55% 46%)` }} title={`${name} <${email}>`}>{[...name.trim()][0]?.toUpperCase() ?? '?'}</span>
}

function GraphCell({ row, width }: { row: GraphRow; width: number }) {
  const mid = ROW / 2
  const path = (from: number, to: number, y0: number, y1: number) => {
    const x0 = laneX(from), x1 = laneX(to)
    return x0 === x1 ? `M${x0} ${y0}V${y1}` : `M${x0} ${y0}C${x0} ${(y0 + y1) / 2} ${x1} ${(y0 + y1) / 2} ${x1} ${y1}`
  }
  return <svg className={css.graphCell} width={laneX(width - 1) + 14} height={ROW} aria-hidden="true">
    {row.top.map(([from, to], i) => <path key={`t${i}`} d={path(from, to, 0, mid)} stroke={LANE_COLORS[from % LANE_COLORS.length]} />)}
    {row.bottom.map(([from, to], i) => <path key={`b${i}`} d={path(from, to, mid, ROW)} stroke={LANE_COLORS[to % LANE_COLORS.length]} />)}
    <circle cx={laneX(row.column)} cy={mid} r={DOT} fill="var(--codex-bg-main)" stroke={LANE_COLORS[row.column % LANE_COLORS.length]} strokeWidth={2.5} />
  </svg>
}

/** The upstream branch is drawn as the cloud at the row's end, not as a badge. */
function RefBadges({ refs, upstream }: { refs: string[]; upstream: string | null }) {
  return <span className={css.refs}>{refs.filter(ref => !ref.endsWith('/HEAD') && ref !== upstream).flatMap(ref => {
    if (ref.startsWith('HEAD -> ')) return [<span key={ref} className={clsx(css.ref, css.refHead)}>{ref.slice(8)}</span>, <span key="HEAD" className={clsx(css.ref, css.refHeadTag)}>HEAD</span>]
    if (ref.startsWith('tag: ')) return [<span key={ref} className={clsx(css.ref, css.refTag)}>{ref.slice(5)}</span>]
    if (ref === 'HEAD') return [<span key={ref} className={clsx(css.ref, css.refHeadTag)}>HEAD</span>]
    return [<span key={ref} className={clsx(css.ref, ref.includes('/') ? css.refRemote : css.refBranch)}>{ref}</span>]
  })}</span>
}

const STATUS_LABEL: Record<string, string> = { M: '修改', A: '新增', D: '删除', R: '重命名', C: '复制', U: '冲突', '?': '未跟踪', T: '类型' }

export default function GitPage({ workspaceRoot, owner }: PluginPageProps) {
  const tools = usePluginTools(owner)
  const [data, setData] = useState<GitOverview>()
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [busy, setBusy] = useState<'' | 'load' | 'fetch' | 'pull' | 'push'>('')
  const [view, setView] = useState<View>('graph')
  const [branch, setBranch] = useState('')
  const [query, setQuery] = useState('')
  const [author, setAuthor] = useState('')
  const [selected, setSelected] = useState<Selection>()
  const [detail, setDetail] = useState<GitCommitDetail>()
  const [detailError, setDetailError] = useState('')
  const [openFile, setOpenFile] = useState<string>()
  const [patch, setPatch] = useState<GitPatch>()
  const [copied, setCopied] = useState(false)
  const [files, setFiles] = useState<{ paths: string[]; truncated: boolean; prefix: string }>()
  /** The file open in the code view: the working copy, or its version at a commit. */
  const [codeFile, setCodeFile] = useState<{ path: string; commit: string | null }>()
  const codeChat = useCordis().codeChat
  const generation = useRef(0), detailGeneration = useRef(0), patchGeneration = useRef(0)

  async function load(nextBranch = branch) {
    const token = ++generation.current
    setBusy(current => current || 'load'); setError('')
    try {
      const [next, listed] = await Promise.all([
        tools.call<GitOverview>('read_git', { workspace_root: workspaceRoot, view: 'overview', ...(nextBranch ? { branch: nextBranch } : {}) }),
        tools.call<{ paths: string[]; truncated: boolean; prefix: string }>('read_git', { workspace_root: workspaceRoot, view: 'files' }),
      ])
      if (token !== generation.current) return
      setData(next)
      setFiles(listed)
      setSelected(current => current ?? (next.commits[0] ? { hash: next.commits[0].hash } : undefined))
    } catch (reason) {
      if (token === generation.current) { setError(message(reason)); setData(undefined) }
    } finally { if (token === generation.current) setBusy(current => current === 'load' ? '' : current) }
  }

  useEffect(() => {
    setSelected(undefined); setDetail(undefined)
    void load()
    // The agent or the user's editor may change the repository while the page is away.
    const refresh = () => { if (document.visibilityState === 'visible') void load() }
    window.addEventListener('focus', refresh)
    return () => { window.removeEventListener('focus', refresh); generation.current++ }
  }, [workspaceRoot])

  useEffect(() => {
    if (!selected) return
    const token = ++detailGeneration.current
    setDetailError('')
    tools.call<GitCommitDetail>('read_git', { workspace_root: workspaceRoot, view: 'commit', ...(selected.hash ? { commit: selected.hash } : {}) })
      .then(next => {
        if (token !== detailGeneration.current) return
        setDetail(next)
        // A file picked in the changes list stays open; otherwise show the first.
        setOpenFile(current => next.files.some(file => file.path === current) ? current : next.files[0]?.path)
      })
      .catch(reason => { if (token === detailGeneration.current) { setDetail(undefined); setDetailError(message(reason)) } })
  }, [selected, data])

  useEffect(() => {
    if (!openFile || !detail) return
    const token = ++patchGeneration.current
    setPatch(undefined)
    tools.call<GitPatch>('read_git', { workspace_root: workspaceRoot, view: 'file_diff', path: openFile, ...(detail.hash ? { commit: detail.hash } : {}) })
      .then(next => { if (token === patchGeneration.current) setPatch(next) })
      .catch(reason => { if (token === patchGeneration.current) { setPatch({ commit: detail.hash, path: openFile, patch: '', truncated: false }); setDetailError(message(reason)) } })
  }, [openFile, detail])

  async function sync(action: 'fetch' | 'pull' | 'push') {
    setBusy(action); setError(''); setNotice('')
    try {
      const result = await tools.call<{ stdout: string; stderr: string }>('sync_git', { workspace_root: workspaceRoot, action })
      setNotice(({ fetch: '已获取远程更新', pull: '已拉取', push: '已推送' })[action] + (result.stderr ? `：${result.stderr.split('\n').pop()}` : ''))
    } catch (reason) { setError(message(reason)) }
    finally { setBusy(''); void load() }
  }

  const commits = data?.commits ?? []
  const rows = useMemo(() => layoutGraph(commits), [commits])
  const graphWidth = Math.min(8, Math.max(1, ...rows.map(row => row.width)))
  const authors = useMemo(() => [...new Set(commits.map(c => c.author))].sort(), [commits])
  const needle = query.trim().toLowerCase()
  const filtered = !needle && !author
  const visible = commits.map((commit, i) => ({ commit, row: rows[i]! })).filter(({ commit }) =>
    (!author || commit.author === author) && (!needle || commit.subject.toLowerCase().includes(needle) || commit.hash.startsWith(needle) || commit.author.toLowerCase().includes(needle)))
  const localBranches = data?.branches.filter(b => !b.remote) ?? []
  const changeCodes = useMemo(() => new Map((data?.changes ?? []).map(change => [change.path,
    change.conflict ? '!' : change.index !== '.' && change.index !== '?' ? change.index : change.worktree])), [data])
  function showWorkingDiff(path: string) {
    setCodeFile(undefined); setView('changes'); setSelected(current => current?.hash === null ? current : { hash: null }); setOpenFile(path)
  }
  // Chat mentions are relative to the workspace, which may sit below the repository root.
  const prefix = files?.prefix ?? ''
  const toWorkspace = (path: string) => !prefix ? path : path.startsWith(prefix) ? path.slice(prefix.length) : undefined
  useEffect(() => {
    codeChat.setCurrent(codeFile ? toWorkspace(codeFile.path) : undefined)
  }, [codeFile, prefix])
  useEffect(() => () => codeChat.setCurrent(undefined), [])
  const synced = data?.upstream && data.ahead === 0 && data.behind === 0

  return <div className={css.root}>
    {data && files && <FileTree repository={data.repository} paths={files.paths} truncated={files.truncated} changes={changeCodes}
      active={codeFile?.path} onOpen={path => setCodeFile({ path, commit: null })} dragPath={toWorkspace} />}
    <div className={css.content}>
    <header className={css.header}>
      <div className={css.titleGroup}>
        <Icon name="folder" size={20} />
        <h1 title={data?.root ?? workspaceRoot}>{data?.repository ?? workspaceRoot.split(/[\\/]/).pop()}</h1>
        {data?.head && <span className={css.branchPill}><BranchIcon />{data.head}</span>}
      </div>
      <div className={css.actions}>
        <button type="button" className={css.ghost} disabled={!!busy} onClick={() => { void load() }} title="刷新"><Icon name="refresh" size={15} />刷新</button>
        <button type="button" className={css.ghost} disabled={!!busy || !data} onClick={() => { void sync('fetch') }} title="git fetch --prune">获取</button>
        <button type="button" className={css.primary} disabled={!!busy || !data} onClick={() => { void sync('pull') }} title="git pull --ff-only"><ArrowIcon down />{busy === 'pull' ? '拉取中…' : `拉取${data?.behind ? ` ${data.behind}` : ''}`}</button>
        <button type="button" className={css.primary} disabled={!!busy || !data} onClick={() => { void sync('push') }} title="git push"><ArrowIcon />{busy === 'push' ? '推送中…' : `推送${data?.ahead ? ` ${data.ahead}` : ''}`}</button>
      </div>
    </header>

    <nav className={css.tabs}>
      {([['graph', '提交图谱'], ['changes', `文件变更${data?.changes.length ? ` ${data.changes.length}` : ''}`], ['branches', '分支'], ['stashes', `储藏${data?.stashes.length ? ` ${data.stashes.length}` : ''}`]] as const).map(([id, label]) =>
        <button key={id} type="button" className={clsx(css.tab, view === id && css.tabActive)} onClick={() => {
          setView(id); setCodeFile(undefined)
          if (id === 'changes') setSelected({ hash: null })
        }}>{label}</button>)}
    </nav>

    {error && <p role="alert" className={css.error}>{error}</p>}
    {notice && <p className={css.notice} onClick={() => setNotice('')}>{notice}</p>}

    {codeFile ? <div className={css.body}>
      <CodeView path={codeFile.path} commit={codeFile.commit} changed={changeCodes.has(codeFile.path)}
        load={(path, commit) => tools.call<GitFileText>('read_git', { workspace_root: workspaceRoot, view: 'file', path, ...(commit ? { commit } : {}) })}
        onShowDiff={() => showWorkingDiff(codeFile.path)} onClose={() => setCodeFile(undefined)} />
    </div> : <div className={css.body}>
      <main className={css.main}>
        {data && <div className={css.statusBar}>
          <span><BranchIcon />当前分支 <code className={css.code}>{data.head ?? '分离 HEAD'}</code></span>
          {data.upstream && <span className={css.muted}>领先远程 {data.ahead} 个提交 · 落后 {data.behind} 个提交</span>}
          <span className={css.sep} />
          <button type="button" className={css.linkButton} onClick={() => { setView('changes'); setSelected({ hash: null }) }}>
            <Icon name="code" size={14} />本地变更：<u>{data.changes.length} 个文件</u></button>
          <span className={css.grow} />
          <span className={css.muted}>远程 {data.upstream ?? '未设置上游'}</span>
          {synced && <span className={css.synced}><Icon name="check" size={13} />已同步</span>}
        </div>}

        {view === 'graph' && <>
          <div className={css.toolbar}>
            <label className={css.select}><BranchIcon />
              <select value={branch} onChange={event => { setBranch(event.target.value); void load(event.target.value) }}>
                <option value="">全部分支</option>
                {localBranches.map(b => <option key={b.name} value={b.name}>{b.name}</option>)}
              </select>
            </label>
            <label className={css.search}><Icon name="search" size={15} />
              <input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索提交（消息、哈希、作者…）" />
            </label>
            <label className={css.select}><Icon name="user" size={15} />
              <select value={author} onChange={event => setAuthor(event.target.value)}>
                <option value="">全部作者</option>
                {authors.map(name => <option key={name} value={name}>{name}</option>)}
              </select>
            </label>
          </div>
          <div className={css.table} role="table">
            <div className={clsx(css.tr, css.th)} role="row">
              {/* The lanes only mean something when every commit is shown. */}
              <span style={{ width: filtered ? laneX(graphWidth - 1) + 14 : 36 }}>图谱</span>
              <span className={css.colSubject}>提交信息（共 {visible.length} 条）</span>
              <span className={css.colAuthor}>作者</span><span className={css.colTime}>提交时间</span>
              <span className={css.colHash}>提交哈希</span><span className={css.colRefs}>分支 / 标签</span><span className={css.colCloud} />
            </div>
            {busy === 'load' && !data && <p className={css.empty}>正在读取仓库…</p>}
            {data && commits.length === 0 && <p className={css.empty}>这个仓库还没有提交。</p>}
            {visible.map(({ commit, row }) => <div key={commit.hash} role="row" tabIndex={0}
              className={clsx(css.tr, selected?.hash === commit.hash && css.trActive)}
              onClick={() => setSelected({ hash: commit.hash })} onKeyDown={event => { if (event.key === 'Enter') setSelected({ hash: commit.hash }) }}>
              <span className={css.graphCol} style={{ width: filtered ? laneX(graphWidth - 1) + 14 : 36 }}>
                {filtered ? <GraphCell row={row} width={graphWidth} /> : <span className={css.dot} />}
              </span>
              <span className={css.colSubject} title={commit.subject}>
                {commit.parents.length > 1 ? <MergeIcon /> : <Icon name="file" size={15} />}<span className={css.ellipsis}>{commit.subject}</span>
              </span>
              <span className={css.colAuthor}><Avatar name={commit.author} email={commit.email} /><span className={css.ellipsis}>{commit.author}</span></span>
              <span className={clsx(css.colTime, css.muted)} title={absoluteTime(commit.time)}>{relativeTime(commit.time)}</span>
              <span className={css.colHash}><code className={css.hash}>{commit.hash.slice(0, 7)}</code></span>
              <span className={css.colRefs}><RefBadges refs={commit.refs} upstream={data?.upstream ?? null} /></span>
              <span className={css.colCloud}>{data?.upstream && commit.refs.includes(data.upstream) &&
                <span className={css.cloud} title={`远程 ${data.upstream} 在这里：这个提交及更早的提交已推送`}><CloudIcon /></span>}</span>
            </div>)}
          </div>
        </>}

        {view === 'changes' && <div className={css.list}>
          {data?.changes.length === 0 && <p className={css.empty}>工作区是干净的，没有未提交的改动。</p>}
          {data?.changes.map(change => {
            const code = change.index !== '.' && change.index !== '?' ? change.index : change.worktree
            return <button key={change.path} type="button" className={clsx(css.listRow, selected?.hash === null && openFile === change.path && css.trActive)}
              onClick={() => { setSelected(current => current?.hash === null ? current : { hash: null }); setOpenFile(change.path) }}>
              <span className={clsx(css.statusCode, css[`status${change.conflict ? 'U' : code === '?' ? 'Q' : code}`])} title={STATUS_LABEL[change.conflict ? 'U' : code]}>{change.conflict ? '!' : code === '?' ? 'U' : code}</span>
              <span className={css.ellipsis}>{change.orig_path ? `${change.orig_path} → ` : ''}{change.path}</span>
              <span className={css.grow} />
              <span className={css.muted}>{change.index !== '.' && change.index !== '?' ? '已暂存' : STATUS_LABEL[code] ?? ''}</span>
            </button>
          })}
        </div>}

        {view === 'branches' && <div className={css.list}>
          {(['本地分支', '远程分支'] as const).map((title, remote) => <section key={title}>
            <h2 className={css.listTitle}>{title}</h2>
            {data?.branches.filter(b => b.remote === !!remote).map(b => <button key={b.name} type="button" className={css.listRow}
              onClick={() => { setView('graph'); setSelected({ hash: b.hash }) }}>
              <BranchIcon />
              <span className={clsx(css.ellipsis, b.current && css.bold)}>{b.name}</span>
              {b.current && <span className={clsx(css.ref, css.refHeadTag)}>HEAD</span>}
              {b.upstream && <span className={css.muted}>→ {b.upstream}</span>}
              <span className={css.grow} />
              <code className={css.hash}>{b.hash.slice(0, 7)}</code>
              <span className={clsx(css.muted, css.colTime)}>{relativeTime(b.time)}</span>
            </button>)}
          </section>)}
        </div>}

        {view === 'stashes' && <div className={css.list}>
          {data?.stashes.length === 0 && <p className={css.empty}>没有储藏。</p>}
          {data?.stashes.map(stash => <button key={stash.name} type="button" className={clsx(css.listRow, selected?.hash === stash.hash && css.trActive)} onClick={() => setSelected({ hash: stash.hash })}>
            <code className={css.code}>{stash.name}</code><span className={css.ellipsis}>{stash.subject}</span>
            <span className={css.grow} /><span className={css.muted}>{relativeTime(stash.time)}</span>
          </button>)}
        </div>}
      </main>

      <aside className={css.detail} aria-label="提交详情">
        {!selected && <p className={css.empty}>选择一个提交查看详情。</p>}
        {selected && <>
          <section className={css.card}>
            <h2 className={css.cardTitle}><Icon name="info" size={17} />{selected.hash ? '提交详情' : '工作区变更'}</h2>
            {detailError && <p role="alert" className={css.error}>{detailError}</p>}
            {!detail && !detailError && <p className={css.muted}>正在读取…</p>}
            {detail?.hash && <>
              <p className={css.subject}><Icon name="check" size={15} />{detail.message?.split('\n')[0]}</p>
              <dl className={css.meta}>
                <dt><Icon name="user" size={14} />作者</dt><dd>{detail.author} <span className={css.muted}>&lt;{detail.email}&gt;</span></dd>
                <dt><Icon name="pause" size={14} />提交时间</dt><dd>{absoluteTime(detail.time)} <span className={css.muted}>（{relativeTime(detail.time)}）</span></dd>
                <dt><Icon name="code" size={14} />提交哈希</dt><dd>
                  <code className={css.hash}>{detail.hash.slice(0, 7)}</code>
                  <button type="button" className={css.iconButton} title="复制完整哈希" onClick={() => { void navigator.clipboard.writeText(detail.hash!).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1200) }) }}>
                    <Icon name={copied ? 'check' : 'copy'} size={13} /></button></dd>
                <dt><Icon name="box" size={14} />父提交</dt><dd>{detail.parents?.length ? detail.parents.map(p =>
                  <button key={p} type="button" className={css.hashLink} onClick={() => setSelected({ hash: p })}><code className={css.hash}>{p.slice(0, 7)}</code></button>) : <span className={css.muted}>无（首个提交）</span>}
                  {(detail.parents?.length ?? 0) > 1 && <span className={css.muted}>（合并提交）</span>}</dd>
              </dl>
              {detail.message?.includes('\n') && <><h3 className={css.subTitle}><Icon name="file" size={14} />描述</h3>
                <pre className={css.message}>{detail.message.split('\n').slice(1).join('\n').trim()}</pre></>}
            </>}
          </section>
          {detail && <FileList files={detail.files} openFile={openFile} onOpen={setOpenFile} />}
          {openFile && detail && <section className={css.card}>
            <h3 className={css.subTitle}><Icon name="code" size={14} />代码预览 <span className={clsx(css.muted, css.ellipsis)}>{openFile}</span>
              {patch?.patch && <button type="button" className={css.iconButton} title="复制补丁" onClick={() => { void navigator.clipboard.writeText(patch.patch) }}><Icon name="copy" size={13} /></button>}
              <button type="button" className={css.iconButton} title={detail.hash ? '查看这个版本的完整文件' : '查看完整文件'}
                onClick={() => setCodeFile({ path: openFile, commit: detail.hash })}><Icon name="expand" size={13} /></button></h3>
            {!patch && <p className={css.muted}>正在读取…</p>}
            {patch && <Diff patch={patch} />}
          </section>}
        </>}
      </aside>
    </div>}
    </div>
  </div>
}

function FileList({ files, openFile, onOpen }: { files: GitFile[]; openFile: string | undefined; onOpen: (path: string) => void }) {
  const added = files.reduce((n, f) => n + (f.added ?? 0), 0), deleted = files.reduce((n, f) => n + (f.deleted ?? 0), 0)
  return <section className={css.card}>
    <h3 className={css.subTitle}><Icon name="box" size={14} />变更文件（{files.length}）<span className={css.grow} />
      <span className={css.add}>+{added}</span><span className={css.del}>-{deleted}</span></h3>
    {files.length === 0 && <p className={css.muted}>没有文件变化。</p>}
    <div className={css.files}>
      {files.map(file => <button key={file.path} type="button" className={clsx(css.fileRow, openFile === file.path && css.fileRowActive)} onClick={() => onOpen(file.path)}>
        <Icon name={openFile === file.path ? 'chevronDown' : 'chevron'} size={13} /><Icon name="file" size={14} />
        <span className={css.ellipsis} title={file.orig_path ? `${file.orig_path} → ${file.path}` : file.path}>{file.path}</span>
        <span className={css.grow} />
        {file.untracked ? <span className={css.muted}>新文件</span> : file.added === null ? <span className={css.muted}>二进制</span>
          : <><span className={css.add}>+{file.added}</span><span className={css.del}>-{file.deleted}</span></>}
      </button>)}
    </div>
  </section>
}

function Diff({ patch }: { patch: GitPatch }) {
  const lines = useMemo(() => parsePatch(patch.patch), [patch.patch])
  if (!lines.length) return <p className={css.muted}>{patch.patch ? '二进制文件或仅权限变化。' : '没有可显示的差异。'}</p>
  return <div className={css.diff}>
    {lines.map((line, i) => <div key={i} className={clsx(css.diffLine, css[line.kind])}>
      <span className={css.lineNo}>{line.line ?? ''}</span>
      <span className={css.sign}>{line.kind === 'add' ? '+' : line.kind === 'del' ? '−' : ''}</span>
      <code>{line.text || ' '}</code>
    </div>)}
    {patch.truncated && <p className={css.muted}>补丁太大，只显示了前一部分。</p>}
  </div>
}

function message(reason: unknown) { return String(reason instanceof Error ? reason.message : reason).replace(/^Error:\s*/, '') }

function BranchIcon() {
  return <svg width={15} height={15} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="6.5" cy="5.5" r="2.5" /><circle cx="6.5" cy="18.5" r="2.5" /><circle cx="17.5" cy="7.5" r="2.5" /><path d="M6.5 8v8M17.5 10c0 4-5 4-9.5 6.5" /></svg>
}
function MergeIcon() {
  return <svg width={15} height={15} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="6.5" cy="5.5" r="2.5" /><circle cx="6.5" cy="18.5" r="2.5" /><circle cx="17.5" cy="12" r="2.5" /><path d="M6.5 8v8M8 7.5c1.5 3 4 4.5 7 4.5" /></svg>
}
function CloudIcon() {
  return <svg width={14} height={14} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M7 18.5a4.5 4.5 0 0 1-.6-9 6 6 0 0 1 11.5 1.6A3.8 3.8 0 0 1 17.5 18.5z" /></svg>
}
function ArrowIcon({ down }: { down?: boolean }) {
  return <svg width={15} height={15} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {down ? <path d="M12 4v12m-5-5 5 5 5-5M5 20h14" /> : <path d="M12 20V8m-5 5 5-5 5 5M5 4h14" />}</svg>
}
