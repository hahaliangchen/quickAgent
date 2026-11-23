import { useContext, useEffect, useRef, useState } from 'react'
import type { PluginPageProps } from '../../plugins/protocol.ts'
import { type Access, type AiActivity, type RemoteTool, type Server, type Usage, confirmAccess, message, ssh, SshContext, SshError } from './api.ts'
import TerminalPane from './TerminalPane.tsx'
import AiTerminalPane from './AiTerminalPane.tsx'
import SftpExplorer from './SftpExplorer.tsx'
import ServerModal from './ServerModal.tsx'
import css from './style.module.css'

type Tab = { kind: 'human'; id: number; profile: Server; access: Access } | { kind: 'ai'; id: string; taskId: string; profile: Server }
const aiTabId = (task: string, server: string) => 'ai:' + encodeURIComponent(task) + ':' + server
const directory = (tool: RemoteTool, path: string) => {
  if (tool === 'ssh__exec' || tool === 'ssh__list_dir') return path
  const slash = path.lastIndexOf('/')
  return slash < 0 ? '.' : slash === 0 ? '/' : path.slice(0, slash)
}

export default function Workbench({ workspaceRoot, theme = 'light' }: PluginPageProps) {
  const context = useContext(SshContext)
  const access = { ...(workspaceRoot ? { workspace_root: workspaceRoot } : {}), ...context.access }
  const [servers, setServers] = useState<Server[]>([])
  const [selected, setSelected] = useState<string>()
  const [tabs, setTabs] = useState<Tab[]>([])
  const [tab, setTab] = useState<Tab['id']>()
  const [followTarget, setFollowTarget] = useState<{ serverId: string; path: string; seq: number }>()
  const [refreshTarget, setRefreshTarget] = useState<{ serverId: string; seq: number }>()
  const [visited, setVisited] = useState<Set<string>>(() => new Set())
  const [history, setHistory] = useState<Usage[]>([])
  const [revision, setRevision] = useState(0)
  const [form, setForm] = useState<'new' | 'edit'>()
  const [mismatch, setMismatch] = useState<SshError>()
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [ready, setReady] = useState(false)
  const [testing, setTesting] = useState(false)
  const next = useRef(0)
  const followSeq = useRef(0)
  const serverList = useRef(servers)
  serverList.current = servers
  const focus = context.followAi ? context.aiFocus : undefined
  const server = servers.find(item => item.id === selected)
  const activity = () => setRevision(value => value + 1)
  const report = (cause: unknown, id?: string) => {
    setNotice(''); setError(message(cause))
    if (cause instanceof SshError && cause.keyChanged && id) { setSelected(id); setMismatch(cause); setForm('edit') }
  }
  useEffect(() => {
    let live = true
    ssh.list().then(items => {
      if (live) { setServers(items); setSelected(items[0]?.id); setReady(true) }
    }, error => { if (live) { report(error); setReady(true) } })
    return () => { live = false }
  }, [])
  useEffect(() => {
    const abort = new AbortController()
    setHistory([])
    if (selected) ssh.history(selected, abort.signal).then(result => setHistory(result.history), error => { if (!abort.signal.aborted) report(error, selected) })
    return () => abort.abort()
  }, [selected, revision])

  useEffect(() => {
    if (selected) setVisited(current => current.has(selected) ? current : new Set([...current, selected]))
  }, [selected])

  useEffect(() => {
    if (!ready || !focus) return
    let stale = false
    async function apply() {
      if (!focus) return
      let profile = serverList.current.find(item => item.id === focus.serverId)
      if (!profile) {
        const items = await ssh.list()
        if (stale) return
        setServers(items)
        profile = items.find(item => item.id === focus.serverId)
      }
      if (!profile) return
      const id = aiTabId(focus.taskId, profile.id)
      const item: Tab = { kind: 'ai', id, taskId: focus.taskId, profile }
      setSelected(profile.id)
      setTabs(items => items.some(item => item.id === id) ? items : [...items, item])
      setTab(id)
      const path = focus.remotePath ?? profile.default_dir ?? '.'
      setFollowTarget({ serverId: profile.id, path: focus.remotePath ? directory(focus.tool, path) : path, seq: ++followSeq.current })
    }
    void apply().catch(error => { if (!stale) report(error, focus.serverId) })
    return () => { stale = true }
  }, [ready, focus])

  function liveActivity(item: Extract<Tab, { kind: 'ai' }>, event: AiActivity) {
    activity()
    if (focus?.serverId !== item.profile.id || focus.taskId !== item.taskId) return
    // Completion refreshes the visible directory without taking manual navigation back.
    setRefreshTarget({ serverId: item.profile.id, seq: ++followSeq.current })
    if (!focus.remotePath && event.remote_path && tab === item.id && selected === item.profile.id) {
      const tool = ('ssh__' + event.action) as RemoteTool
      setFollowTarget({ serverId: item.profile.id, path: directory(tool, event.remote_path), seq: ++followSeq.current })
    }
  }

  function saved(profile: Server) {
    setServers(items => items.some(item => item.id === profile.id) ? items.map(item => item.id === profile.id ? profile : item) : [...items, profile])
    setSelected(profile.id); setForm(undefined); setMismatch(undefined); setError('')
  }
  function openTerminal(profile: Server) {
    if (!confirmAccess(profile, '打开交互式终端；输入会直接发送到远程服务器。')) return
    const id = ++next.current
    setTabs(items => [...items, { kind: 'human', id, profile, access: { ...access, approved: profile.require_approval } }]); setTab(id); setError('')
  }
  function closeTerminal(id: Tab['id']) {
    const remaining = tabs.filter(item => item.id !== id)
    setTabs(remaining)
    if (id === tab) { const last = remaining.at(-1); setTab(last?.id); if (last) setSelected(last.profile.id) }
  }
  async function remove(profile: Server) {
    if (!window.confirm('删除档案 ' + profile.name + ' 及其已保存凭据和使用记录？')) return
    try {
      await ssh.delete(profile.id)
      const remaining = servers.filter(item => item.id !== profile.id)
      const remainingTabs = tabs.filter(item => item.profile.id !== profile.id)
      setServers(remaining); setTabs(remainingTabs); setTab(remainingTabs.at(-1)?.id); setSelected(remainingTabs.at(-1)?.profile.id ?? remaining[0]?.id); setError('')
    } catch (cause) { report(cause, profile.id) }
  }
  async function test(profile: Server) {
    setTesting(true); setNotice(''); setError('')
    try { const updated = await ssh.test(profile.id); saved(updated); setNotice('连接与认证通过') }
    catch (cause) { report(cause, profile.id) }
    finally { setTesting(false) }
  }

  return <div className={css.root} data-theme={theme} data-testid="ssh-workbench">
    <header className={css.topbar}>
      <h1>远程终端</h1><span>SFTP</span>
      <label className={css.followAi}><input type="checkbox" checked={context.followAi} onChange={event => context.onToggleFollowAi?.(event.target.checked)} />跟随 AI 操作</label>
      {server && <p title={server.username + '@' + server.host + ':' + server.port}>{server.username}@{server.host}:{server.port}</p>}
      {server?.require_approval && <span className={css.badge} data-tier="prod">需确认</span>}
    </header>
    {(error || notice) && <div className={error ? css.alert : css.notice} role={error ? 'alert' : 'status'}><span>{error || notice}</span><button aria-label="关闭提示" onClick={() => { setError(''); setNotice('') }}>×</button></div>}
    <div className={css.columns}>
      <aside className={css.sidebar} aria-label="服务器与使用记录">
        <header className={css.panelHeader}><h2>服务器</h2><button className={css.primary} onClick={() => { setMismatch(undefined); setForm('new') }}>＋ 添加服务器</button></header>
        <ul className={css.servers}>{servers.map(profile => <li key={profile.id}><button aria-pressed={profile.id === selected} onClick={() => { setSelected(profile.id); setError('') }}>
          <span><strong>{profile.name}</strong><small title={profile.host}>{profile.host}</small></span><span className={css.badge} data-tier={profile.env_tier}>{profile.env_tier}</span>
        </button></li>)}</ul>
        {ready && servers.length === 0 && <p className={css.muted}>没有服务器档案</p>}
        {server && <>
          <div className={css.serverActions}>
            <button onClick={() => void test(server)} disabled={testing}>{testing ? '测试中…' : '测试连接'}</button>
            <button onClick={() => { setMismatch(undefined); setForm('edit') }}>设置</button>
            <button title="删除服务器档案" aria-label="删除服务器档案" onClick={() => void remove(server)}>×</button>
          </div>
          <h3>最近活动</h3>
          <ul className={css.history}>{history.length === 0 ? <li className={css.muted}>暂无使用记录</li> : history.slice(0, 8).map(item => <li key={item.id}>
            <time dateTime={new Date(item.created_at * 1000).toISOString()}>{new Date(item.created_at * 1000).toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' })}</time>
            <span>{{ terminal:'终端会话', exec:'执行命令', upload:'上传文件', download:'下载文件', read_file:'预览文件', write_file:'修改目录' }[item.action] ?? item.summary}</span>
            {item.task_id && context.openTask && <button title={item.task_id} onClick={() => context.openTask?.(item.task_id!)}>{context.titleFor?.(item.task_id) || '打开关联会话'}</button>}
          </li>)}</ul>
        </>}
      </aside>
      <main className={css.terminalColumn} aria-label="终端区">
        <div className={css.tabbar}><div className={css.tabs} role="tablist" aria-label="终端标签">{tabs.map(item => <div key={item.id} className={css.tab} data-active={item.id === tab}>
          <button role="tab" aria-selected={item.id === tab} onClick={() => { setTab(item.id); setSelected(item.profile.id) }}>{item.profile.name} · {item.kind === 'ai' ? 'AI 实时' : item.id}</button>
          <button aria-label={item.kind === 'ai' ? '关闭 AI 实时终端 ' + item.profile.name : '关闭终端 ' + item.id} onClick={() => closeTerminal(item.id)}>×</button>
        </div>)}</div><button className={css.newTerminal} disabled={!server} onClick={() => { if (server) openTerminal(server) }}>＋ 新建终端</button></div>
        {tabs.length === 0 && <div className={css.empty}><span className={css.promptIcon} aria-hidden="true">&gt;_</span><p>没有打开的终端</p></div>}
        {tabs.map(item => item.kind === 'ai' ? <AiTerminalPane key={item.id} serverId={item.profile.id} taskId={item.taskId} active={tab === item.id} theme={theme}
          onOpenHuman={() => openTerminal(serverList.current.find(profile => profile.id === item.profile.id) ?? item.profile)} onActivity={event => liveActivity(item, event)} diagnose={context.diagnose} /> : <TerminalPane key={item.id} server={item.profile} access={item.access} active={tab === item.id} theme={theme} onError={cause => report(cause, item.profile.id)} onConnection={activity} diagnose={context.diagnose} />)}
      </main>
      {servers.filter(profile => profile.id === selected || visited.has(profile.id)).map(profile => <SftpExplorer key={profile.id} server={profile} access={access}
        hidden={profile.id !== selected} theme={theme} followTarget={followTarget?.serverId === profile.id ? followTarget : undefined}
        refreshSignal={refreshTarget?.serverId === profile.id ? refreshTarget.seq : undefined}
        onError={cause => { if (profile.id === selected) report(cause, profile.id) }} onActivity={activity} />)}
      {!server && <section className={css.explorer}><header className={css.panelHeader}><h2>SFTP 文件</h2></header><p className={css.muted}>未选择服务器</p></section>}
    </div>
    {form && <ServerModal key={form === 'new' ? 'new' : server?.id} server={form === 'edit' ? server : undefined} keyError={mismatch} onSaved={saved} onClose={() => { setForm(undefined); setMismatch(undefined) }} />}
  </div>
}
