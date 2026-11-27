import { useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react'
import { Background, BackgroundVariant, Controls, MiniMap, ReactFlow, applyEdgeChanges, applyNodeChanges, useNodesInitialized, useReactFlow, ReactFlowProvider, type Connection, type EdgeChange, type NodeChange } from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import type { PluginPageProps } from '../../plugins/protocol.ts'
import { ParchmentQuillIcon } from '../../components/ParchmentQuillIcon.tsx'
import { BlueprintContext, blueprintApi, fetchBlueprintProjects } from './api.ts'
import { BlueprintNode, type Card } from './BlueprintNode.tsx'
import { BlueprintEdge, type Wire } from './BlueprintEdge.tsx'
import { Choices, Drawer, EdgeEditor, EntityEditor, Field, ImpactPanel, newEntity } from './InspectorDrawer.tsx'
import { ObserverInboxView } from './ObserverInboxView.tsx'
import { PlaybookForm, PlaybookViewPanel } from './PlaybookView.tsx'
import { buildDistillPlaybookPrompt, filterWorkspaceSessions } from './playbookPrompts.ts'
import { layout } from './layout.ts'
import { entryKinds, entryLabels, kindColors, projectKinds, type BlueprintProjectStatus, type BlueprintState, type Entity, type EntryKind, type Graph, type GraphEdge, type GraphNode, type GraphView, type Impact, type ObserverSubmission, type PlaybookView, type ProjectKind } from './types.ts'
import { appConfirm } from '../../components/AppDialog.tsx'
import type { Task } from '../../api.ts'
import css from './style.module.css'

const nodeTypes = { blueprint: BlueprintNode }
const edgeTypes = { blueprint: BlueprintEdge }
const firstAnalysis = '对当前项目执行全项目架构蓝图分析。'
const syncPrompt = '对当前项目执行增量蓝图同步。'
const idOf = (prefix: string) => prefix + '-' + crypto.randomUUID().slice(0, 8)
const explain = (error: unknown) => error instanceof Error ? error.message : String(error)
const nodesOnly = (nodes: GraphNode[]) => nodes.map(({ entity_id }) => ({ entity_id }))
const baseName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path
function formatBuildTime(iso: string | null | undefined, full = false): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  const pad = (n: number) => String(n).padStart(2, '0')
  const mdhm = `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
  return full ? `${d.getFullYear()}-${mdhm}:${pad(d.getSeconds())}` : mdhm
}
type Inspect =
  | { type: 'create' | 'meta' | 'scope' | 'distill' | 'createPlaybook' | 'editPlaybook' }
  | { type: 'entity'; id: string; port?: string; addPort?: boolean; draft?: Entity }
  | { type: 'edge'; id: string; draft?: GraphEdge }
  | { type: 'impact'; entity: string; port?: string }

function SidebarToggleIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="3" y="4" width="18" height="16" rx="2.5" />
      <path d="M9 4v16" />
    </svg>
  )
}

function RefreshIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M21 12a9 9 0 1 1-2.64-6.36" />
      <path d="M21 3v6h-6" />
    </svg>
  )
}

function SparkleIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="m12 3-1.9 5.8a2 2 0 0 1-1.3 1.3L3 12l5.8 1.9a2 2 0 0 1 1.3 1.3L12 21l1.9-5.8a2 2 0 0 1 1.3-1.3L21 12l-5.8-1.9a2 2 0 0 1-1.3-1.3Z" />
    </svg>
  )
}

function ChevronDownIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="m6 9 6 6 6-6" />
    </svg>
  )
}

export default function BlueprintPage({ workspaceRoot, theme }: PluginPageProps) {
  if (!workspaceRoot) return <p className={css.notice}>请先选择工作区。</p>
  return <ReactFlowProvider key={workspaceRoot}><Workbench root={workspaceRoot} dark={theme === 'dark'} /></ReactFlowProvider>
}

function Workbench({ root, dark }: { root: string; dark: boolean }) {
  const api = useMemo(() => blueprintApi(root), [root])
  const { sendToAi, refreshSignal, projects = [], selectWorkspace, sessions = [], openSession } = useContext(BlueprintContext)
  const [projectStatuses, setProjectStatuses] = useState<Record<string, BlueprintProjectStatus>>({})
  const projectPathsKey = useMemo(() => projects.map(p => p.path).join('\0'), [projects])
  const refreshProjects = useCallback(async () => {
    if (!projects.length) return
    try {
      const list = await fetchBlueprintProjects(projects.map(p => p.path))
      setProjectStatuses(Object.fromEntries(list.map(item => [item.workspace_root, item])))
    } catch { /* Non-fatal; active workspace state still loads independently. */ }
  }, [projectPathsKey])
  useEffect(() => { void refreshProjects() }, [refreshProjects, refreshSignal])
  const flow = useReactFlow<Card, Wire>()
  const nodesInitialized = useNodesInitialized()
  const canvas = useRef<HTMLDivElement>(null)
  const [state, setState] = useState<BlueprintState>()
  const [id, setId] = useState('')
  const currentId = useRef('')
  const [view, setView] = useState<GraphView>()
  const [playbookId, setPlaybookId] = useState('')
  const currentPlaybookId = useRef('')
  const [playbookView, setPlaybookView] = useState<PlaybookView>()
  const [inboxId, setInboxId] = useState('')
  const currentInboxId = useRef('')
  const [cards, setCards] = useState<Card[]>([])
  const [wires, setWires] = useState<Wire[]>([])
  const [drawer, setDrawer] = useState<Inspect>()
  const [impact, setImpact] = useState<Impact>()
  const [impactMode, setImpactMode] = useState(false)
  const [folded, setFolded] = useState(false)
  const [filter, setFilter] = useState('')
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [busy, setBusy] = useState(false)
  const writing = useRef(false)
  const reading = useRef(0)
  const dragging = useRef(false)
  const pending = useRef<{ id: string; nodes: Map<string, GraphNode> }>()
  const timer = useRef<ReturnType<typeof setTimeout>>()
  const queue = useRef<Promise<void>>(Promise.resolve())
  const fitNext = useRef(true)
  const impactRequest = useRef(0)
  const graph = view?.graph.id === id ? view.graph : undefined

  const flushLayout = useCallback(() => {
    clearTimeout(timer.current)
    const batch = pending.current
    if (!batch) return queue.current
    pending.current = undefined
    const saved = queue.current.then(async () => {
      try {
        const nodes = [...batch.nodes.values()]
        await api.layout(batch.id, nodes)
        setView(current => current?.graph.id === batch.id ? { ...current, graph: { ...current.graph, nodes: current.graph.nodes.map(n => batch.nodes.get(n.entity_id) ?? n) } } : current)
        setNotice('布局已保存')
      } catch (error) { setError(explain(error)) }
    })
    queue.current = saved
    return saved
  }, [api])
  useEffect(() => () => { void flushLayout() }, [flushLayout])

  const choose = useCallback((next: string) => {
    const switchingFromOther = Boolean(currentPlaybookId.current || currentInboxId.current)
    if (currentId.current === next && !switchingFromOther) return
    void flushLayout()
    currentPlaybookId.current = ''
    setPlaybookId(''); setPlaybookView(undefined)
    currentInboxId.current = ''
    setInboxId('')
    currentId.current = next
    fitNext.current = true
    setId(next); setView(undefined); setCards([]); setWires([]); setDrawer(undefined); setImpact(undefined)
    impactRequest.current++
  }, [flushLayout])

  const choosePlaybook = useCallback((next: string) => {
    if (currentPlaybookId.current === next && !currentInboxId.current) return
    void flushLayout()
    currentInboxId.current = ''
    setInboxId('')
    currentPlaybookId.current = next
    setPlaybookId(next); setPlaybookView(undefined); setDrawer(undefined)
  }, [flushLayout])

  const chooseInbox = useCallback((next: string) => {
    if (currentInboxId.current === next) return
    void flushLayout()
    currentPlaybookId.current = ''
    setPlaybookId(''); setPlaybookView(undefined)
    currentInboxId.current = next
    setInboxId(next); setDrawer(undefined)
  }, [flushLayout])

  const refresh = useCallback(async (force = false) => {
    if (!force && (writing.current || dragging.current || pending.current)) return
    const sequence = ++reading.current
    try {
      await queue.current
      const next = await api.state()
      if (sequence !== reading.current) return
      const playbooks = next.playbooks ?? []
      const inboxItems = next.inbox ?? []
      const activeInbox = currentInboxId.current && inboxItems.some(i => i.id === currentInboxId.current)
        ? currentInboxId.current
        : ''
      const activePlaybook = !activeInbox && currentPlaybookId.current && playbooks.some(p => p.id === currentPlaybookId.current)
        ? currentPlaybookId.current
        : ''
      const activeGraph = next.graphs.some(g => g.id === currentId.current) ? currentId.current : next.graphs[0]?.id ?? ''
      if (activeInbox) {
        currentId.current = activeGraph
        setId(activeGraph)
        setState(next)
        return
      }
      if (currentInboxId.current) {
        currentInboxId.current = ''
        setInboxId('')
      }
      if (activePlaybook) {
        currentId.current = activeGraph
        setId(activeGraph)
        setState(next)
        const pv = await api.playbook(activePlaybook)
        if (sequence === reading.current && activePlaybook === currentPlaybookId.current) setPlaybookView(pv)
        return
      }
      if (currentPlaybookId.current) {
        currentPlaybookId.current = ''
        setPlaybookId(''); setPlaybookView(undefined)
      }
      if (!activeGraph && playbooks.length > 0) {
        const firstPb = playbooks[0]!.id
        currentPlaybookId.current = firstPb
        setPlaybookId(firstPb)
        setState(next)
        const pv = await api.playbook(firstPb)
        if (sequence === reading.current && firstPb === currentPlaybookId.current) setPlaybookView(pv)
        return
      }
      choose(activeGraph)
      setState(next)
      const graphView = activeGraph ? await api.graph(activeGraph) : undefined
      if (sequence === reading.current && activeGraph === currentId.current && !dragging.current && !pending.current) setView(graphView)
    } catch (error) { if (sequence === reading.current) setError(explain(error)) }
  }, [api, choose])
  useEffect(() => { void refresh() }, [id, playbookId, inboxId, refresh, refreshSignal])
  useEffect(() => {
    const interval = window.setInterval(() => { if (document.visibilityState === 'visible') void refresh() }, 30_000)
    return () => { reading.current++; window.clearInterval(interval) }
  }, [refresh])

  const act = async (change: () => Promise<unknown>, close = true) => {
    if (writing.current) return
    writing.current = true; reading.current++; setBusy(true); setError(''); setNotice('')
    try {
      await flushLayout()
      await change()
      if (close) setDrawer(undefined)
      await refresh(true)
      void refreshProjects()
    } catch (error) { setError(explain(error)) }
    finally { writing.current = false; setBusy(false) }
  }
  const saveGraph = (change: Partial<Graph>, close = true) => graph && void act(() => api.save(graph.id, change), close)
  const addEntity = (entity: string) => graph && !graph.nodes.some(n => n.entity_id === entity) && saveGraph({ nodes: [...nodesOnly(graph.nodes), { entity_id: entity }] })
  const inspectImpact = useCallback((entity: string, port?: string, show = true) => {
    const seq = ++impactRequest.current
    setImpact(undefined)
    if (show) setDrawer({ type: 'impact', entity, ...(port ? { port } : {}) })
    void api.impact(entity, port).then(result => { if (impactRequest.current === seq) setImpact(result) }, error => { if (impactRequest.current === seq) setError(explain(error)) })
  }, [api])
  const inspect = useCallback((entity: string, port?: string) => {
    setDrawer({ type: 'entity', id: entity, ...(port ? { port } : {}) })
    if (impactMode) inspectImpact(entity, port, false)
  }, [impactMode, inspectImpact])
  const addPort = useCallback((entity: string) => setDrawer({ type: 'entity', id: entity, addPort: true }), [])
  const inspectEdge = useCallback((id: string) => setDrawer({ type: 'edge', id }), [])
  const highlight = useMemo(() => {
    if (!impact || !graph || !impactMode) return undefined
    const hits = [...impact.upstream, ...impact.downstream].filter(h => h.graph_id === graph.id)
    const ids = new Set(impact.matches.map(m => m.entity.id))
    for (const { edge } of hits) { ids.add(edge.source_entity); ids.add(edge.target_entity) }
    return { ids, edges: new Set(hits.map(h => h.edge.id)) }
  }, [impact, graph, impactMode])
  const activePort = drawer?.type === 'entity' || drawer?.type === 'impact' ? drawer.port : undefined
  const focusedEntity = drawer?.type === 'entity' ? drawer.id : drawer?.type === 'impact' ? drawer.entity : undefined

  useEffect(() => {
    if (!graph || !view) return
    const entities = new Map(view.entities.map(e => [e.id, e]))
    setCards(current => {
      const old = new Map(current.map(n => [n.id, n]))
      return graph.nodes.flatMap(n => {
        const e = entities.get(n.entity_id)
        if (!e) return []
        const previous = old.get(e.id)
        return [{ ...previous, id: e.id, type: 'blueprint' as const, position: dragging.current || pending.current ? previous?.position ?? { x: n.x ?? 0, y: n.y ?? 0 } : { x: n.x ?? 0, y: n.y ?? 0 },
          style: { opacity: highlight && !highlight.ids.has(e.id) ? .3 : 1 },
          data: { entity: e, status: view.statuses[e.id], references: state?.references[e.id]?.length ?? 0, focusedPort: focusedEntity === e.id ? activePort : undefined, inspect, addPort, impact: inspectImpact } }]
      })
    })
    setWires(current => {
      const old = new Map(current.map(e => [e.id, e]))
      return graph.edges.map(item => ({ ...old.get(item.id), id: item.id, type: 'blueprint', source: item.source_entity, target: item.target_entity, sourceHandle: 'out:' + item.source_port, targetHandle: 'in:' + item.target_port,
        style: { opacity: highlight && !highlight.edges.has(item.id) ? .3 : 1 }, data: { item, inspect: inspectEdge, highlighted: highlight?.edges.has(item.id) ?? false } }))
    })
  }, [graph, view, state?.references, highlight, focusedEntity, activePort, inspect, addPort, inspectImpact, inspectEdge])
  useEffect(() => {
    if (!nodesInitialized || !fitNext.current || !cards.length) return
    const frame = requestAnimationFrame(() => {
      fitNext.current = false
      void flow.fitView({ padding: .2, maxZoom: 1.05 })
    })
    return () => cancelAnimationFrame(frame)
  }, [cards, nodesInitialized, flow])
  useEffect(() => {
    const element = canvas.current
    if (!element) return
    let width = element.clientWidth
    let frame = 0
    const observer = new ResizeObserver(() => {
      const next = element.clientWidth
      if (next === width) return
      width = next
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(() => { if (next > 0) void flow.fitView({ padding: .2, maxZoom: 1.05 }) })
    })
    observer.observe(element)
    return () => { cancelAnimationFrame(frame); observer.disconnect() }
  }, [flow])

  const onNodes = (changes: NodeChange<Card>[]) => setCards(current => applyNodeChanges(changes.filter(c => c.type !== 'remove'), current))
  const onEdges = (changes: EdgeChange<Wire>[]) => setWires(current => applyEdgeChanges(changes.filter(c => c.type !== 'remove'), current))
  const stopDrag = (moved: Card[]) => {
    dragging.current = false
    if (!graph || !moved.length) return
    if (pending.current?.id !== graph.id) pending.current = { id: graph.id, nodes: new Map() }
    for (const { id, position } of moved) pending.current.nodes.set(id, { entity_id: id, ...position })
    clearTimeout(timer.current)
    timer.current = setTimeout(() => void flushLayout(), 250)
  }
  const connect = (link: Connection) => {
    if (!graph || !link.sourceHandle || !link.targetHandle || busy) return
    const draft: GraphEdge = { id: idOf('edge'), source_entity: link.source, source_port: link.sourceHandle.slice(4), target_entity: link.target, target_port: link.targetHandle.slice(3), relation: 'calls', severity: 'info', label: '', invariant: null }
    setDrawer({ type: 'edge', id: draft.id, draft })
  }
  const arrange = () => graph && void act(async () => {
    await api.layout(graph.id, layout(graph, view?.entities ?? []))
    fitNext.current = true
    setNotice('布局已保存')
  }, false)
  const selectedCards = cards.filter(n => n.selected)
  const selectedWires = wires.filter(e => e.selected || (drawer?.type === 'edge' && drawer.id === e.id))
  const discuss = () => {
    if (!graph) return
    const nodes = new Set(selectedCards.map(n => n.id))
    if (focusedEntity && activePort) nodes.add(focusedEntity)
    for (const edge of selectedWires) { nodes.add(edge.source); nodes.add(edge.target) }
    const edgeIds = new Set(selectedWires.map(w => w.id))
    const edges = graph.edges.filter(e => edgeIds.has(e.id) || nodes.has(e.source_entity) && nodes.has(e.target_entity))
    const entityIds = [...nodes]
    const edgeList = edges.map(e => `${e.source_entity}:${e.source_port}->${e.target_entity}:${e.target_port}`)
    const detail = [
      `画布 \`${graph.id}\`（${graph.title}）`,
      entityIds.length ? `实体：${entityIds.join(', ')}` : '',
      activePort && focusedEntity ? `端口：${focusedEntity}:${activePort}` : '',
      edgeList.length ? `连线：${edgeList.join(', ')}` : '',
    ].filter(Boolean).join('；')
    sendToAi?.(`请核对并讨论以下选中蓝图片段：${detail}。`)
  }
  const saveInboxAsPlaybook = (submission: ObserverSubmission) => {
    const newId = idOf('pb')
    const code = submission.verify_commands.length
      ? '// 由观察者战地呈报直接整理的最短路径参考\n' + submission.verify_commands.map(c => `// 验收: ${c}`).join('\n')
      : '// 由观察者战地呈报直接整理，可按需补充编排代码'
    void act(async () => {
      const created = await api.createPlaybook({
        id: newId,
        title: submission.title,
        trigger: submission.trigger,
        summary: submission.summary,
        code,
        anchor_files: submission.anchor_files,
        environment: submission.environment,
        pitfalls: submission.pitfalls,
        verify_commands: submission.verify_commands,
        source_tasks: [submission.source_task_id],
      })
      await api.updateInboxStatus(submission.id, 'merged', created.id)
      choosePlaybook(created.id)
      setNotice('已将观察者呈报直接存为最短路径')
    })
  }
  const available = state?.entities.filter(e => (e.name + ' ' + e.id + ' ' + (e.file_path ?? '')).toLowerCase().includes(filter.toLowerCase())) ?? []
  const playbooks = state?.playbooks ?? []
  const inboxItems = state?.inbox ?? []
  const pendingInboxCount = inboxItems.filter(item => item.status === 'pending').length
  const activeInboxItem = inboxId ? inboxItems.find(item => item.id === inboxId) : undefined
  const activePlaybook = playbookId && playbookView?.playbook.id === playbookId ? playbookView : undefined
  const stale = state?.verify
  const drift = (stale?.drifted_ports ?? 0) + (stale?.missing_ports ?? 0) + (stale?.drifted_playbooks ?? 0) + (stale?.missing_playbooks ?? 0)
  const inspectedEntity = drawer?.type === 'entity' ? drawer.draft ?? state?.entities.find(e => e.id === drawer.id) : undefined
  const inspectedEdge = drawer?.type === 'edge' ? drawer.draft ?? graph?.edges.find(e => e.id === drawer.id) : undefined
  const showCreate = () => setDrawer({ type: 'create' })
  const headerTitle = activeInboxItem?.title ?? activePlaybook?.playbook.title ?? graph?.title ?? baseName(root)
  const showSidebar = !folded && (Boolean(state?.exists) || projects.length > 0 || inboxItems.length > 0)

  return <section className={css.workbench} data-theme={dark ? 'dark' : 'light'} data-testid='blueprint-workbench'>
    <header className={css.toolbar}>
      <button type='button' className={css.iconButton} onClick={() => setFolded(!folded)} aria-label={folded ? '展开蓝图侧栏' : '折叠蓝图侧栏'} title={folded ? '展开蓝图侧栏' : '折叠蓝图侧栏'}>
        <SidebarToggleIcon />
      </button>
      <span className={css.titleMark}><ParchmentQuillIcon size={17} variant='line' /></span>
      <strong className={css.title} title={headerTitle}>{headerTitle}</strong>
      {state?.manifest && <span className={css.badge}>{state.manifest.project_kind}</span>}
      {stale?.exists && <span className={css.fresh} data-stale={stale.stale || undefined}>{drift ? '发现 ' + drift + ' 处漂移' : stale.stale ? '有变更待同步' : '已同步'}</span>}
      {pendingInboxCount > 0 && <button type='button' className={css.inboxAlertPill} onClick={() => { const firstPending = inboxItems.find(i => i.status === 'pending') ?? inboxItems[0]; if (firstPending) chooseInbox(firstPending.id) }} title='查看观察者从实战会话呈报的蓝图线索'>
        观察者呈报 {pendingInboxCount}
      </button>}
      {state?.manifest?.updated_at && <span className={css.buildTime} title={'蓝图构建/同步时间：' + formatBuildTime(state.manifest.updated_at, true)}>同步于 {formatBuildTime(state.manifest.updated_at)}</span>}
      <div className={css.spacer} />
      <div className={css.actions}>
        <button type='button' className={css.secondaryBtn} disabled={!sendToAi} onClick={() => setDrawer({ type: 'distill' })} title='从历史会话的编排代码中提炼最短路径蓝图'>
          <SparkleIcon /><span>从会话提炼</span>
        </button>
        {!playbookId && !inboxId && graph && <>
          <div className={css.splitBtn}>
            <button type='button' className={css.secondaryBtn} disabled={!sendToAi} onClick={() => sendToAi?.(firstAnalysis)}>启动分析</button>
            <button type='button' className={css.splitArrow} aria-label='启动分析范围' title='选择分析范围' disabled={!sendToAi} onClick={() => setDrawer({ type: 'scope' })}><ChevronDownIcon /></button>
          </div>
          <button type='button' className={css.secondaryBtn} onClick={() => { void api.verify().then(verify => { setState(current => current ? { ...current, verify } : current); sendToAi?.(syncPrompt + '\n\n当前画布：' + graph.id + '；待检查文件：' + verify.changed_files.length + ' 个。') }, err => setError(explain(err))) }} disabled={!sendToAi}>增量同步</button>
          <button type='button' className={css.secondaryBtn} onClick={arrange} disabled={busy || !graph.nodes.length}>自动排版</button>
          <button type='button' className={css.secondaryBtn} aria-pressed={impactMode} onClick={() => { setImpactMode(!impactMode); if (!impactMode && focusedEntity) inspectImpact(focusedEntity, activePort, false) }}>影响面</button>
          <button type='button' className={css.secondaryBtn} disabled={!sendToAi || !selectedCards.length && !selectedWires.length && !activePort} onClick={discuss}>发给 AI</button>
          <button type='button' className={css.primary} disabled={busy} onClick={() => { const draft = newEntity(); setDrawer({ type: 'entity', id: draft.id, draft }) }}>+ 新建节点</button>
        </>}
        <button type='button' className={css.iconButton} title='刷新蓝图' aria-label='刷新蓝图' disabled={busy} onClick={() => { setError(''); void flushLayout().then(() => Promise.all([refresh(true), refreshProjects()])) }}>
          <RefreshIcon />
        </button>
      </div>
    </header>
    {error && <div role='alert' className={css.error}><span>{error}</span><button type='button' onClick={() => setError('')} aria-label='关闭错误'>×</button></div>}
    <div className={css.main}>
      {showSidebar && <nav aria-label='蓝图画布与共享实体' className={css.sidebar}>
        {projects.length > 0 && <section className={css.projects}>
          <div className={css.sectionHead}><h2>已打开项目</h2><span className={css.countPill}>{projects.length}</span></div>
          <div className={css.projectList}>
            {projects.map(project => {
              const info = projectStatuses[project.path]
              const active = project.path === root
              const status = info?.status ?? (active && state ? (state.exists ? (state.verify.stale ? 'stale' : 'synced') : 'unbuilt') : undefined)
              const builtAt = info?.updated_at ?? (active ? state?.manifest?.updated_at : null)
              const graphsCount = info?.graph_count ?? (active ? state?.graphs.length ?? 0 : 0)
              const playbooksCount = info?.playbook_count ?? (active ? playbooks.length : 0)
              const inboxCount = info?.pending_inbox ?? (active ? pendingInboxCount : 0)
              const driftCount = info ? info.drifted_ports + info.missing_ports + (info.drifted_playbooks ?? 0) + (info.missing_playbooks ?? 0) : (active ? drift : 0)
              const changedCount = info?.changed_files ?? (active ? state?.verify.changed_files.length ?? 0 : 0)
              const statusLabel = status === 'synced' ? '已同步' : status === 'stale' ? '待同步' : status === 'unbuilt' ? '未构建' : '检测中'
              const countText = `${graphsCount} 图${playbooksCount ? ` · ${playbooksCount} 路径` : ''}${inboxCount ? ` · ${inboxCount} 待审` : ''}`
              const metaText = status === 'unbuilt'
                ? (inboxCount ? `${inboxCount} 条观察者呈报待处理` : '未构建蓝图')
                : builtAt
                  ? `${formatBuildTime(builtAt)} · ${countText}${driftCount ? ` · ${driftCount} 漂移` : changedCount ? ` · ${changedCount} 变更` : ''}`
                  : status ? countText : '读取状态…'
              const tooltip = `${project.path}${builtAt ? `\n构建时间：${formatBuildTime(builtAt, true)}` : '\n尚未构建蓝图'}`
              return <button key={project.path} type='button' className={css.projectItem} aria-current={active ? 'location' : undefined} title={tooltip} onClick={() => selectWorkspace?.(project.path)}>
                <span className={css.projectRow}>
                  <span className={css.projectName}>{project.title || baseName(project.path)}</span>
                  <span className={css.projectBadge} data-status={status ?? 'loading'}>{statusLabel}</span>
                </span>
                <span className={css.projectMeta}>{metaText}</span>
              </button>
            })}
          </div>
        </section>}

        {/* Observer battlefield submissions inbox */}
        <section className={projects.length > 0 ? css.sectionDivider : css.inboxSection}>
          <div className={css.sectionHead}>
            <h2>观察者呈报</h2>
            {pendingInboxCount > 0 ? (
              <span className={css.inboxPendingCount}>{pendingInboxCount} 待审</span>
            ) : (
              <span className={css.countPill}>{inboxItems.length}</span>
            )}
          </div>
          {inboxItems.length > 0 ? (
            <div className={css.navStack}>
              {inboxItems.map(item => (
                <button
                  key={item.id}
                  type='button'
                  className={css.inboxNavItem}
                  aria-current={inboxId === item.id ? 'page' : undefined}
                  title={`${item.title}\n来源：${item.source_task_title || item.source_task_id}`}
                  onClick={() => chooseInbox(item.id)}
                >
                  <i className={css.inboxDot} data-status={item.status} />
                  <span className={css.playbookNavTitle}>{item.title}</span>
                </button>
              ))}
            </div>
          ) : (
            <p className={css.sideHint}>观察者巡检或在主会话点击「呈报蓝图」后，战地线索会自动汇总到这里</p>
          )}
        </section>

        {state?.exists && <>
          <div className={`${css.sectionHead} ${css.sectionDivider}`}><h2>画布</h2><button type='button' className={css.miniAction} onClick={showCreate}>+ 新建</button></div>
          {entryKinds.map(kind => {
            const group = state.graphs.filter(g => g.entry_kind === kind)
            return group.length > 0 && <section key={kind} className={css.group}><h3>{entryLabels[kind]}</h3><div className={css.navStack}>{group.map(g => <button type='button' key={g.id} aria-current={!playbookId && !inboxId && g.id === id ? 'page' : undefined} title={g.entry_summary || g.title} onClick={() => choose(g.id)}>{g.title}</button>)}</div></section>
          })}
          <section className={css.playbookSectionNav}>
            <div className={css.sectionHead}>
              <h2>最短路径 ({playbooks.length})</h2>
              <div className={css.playbookNavActions}>
                <button type='button' className={css.miniAction} title='从历史会话提炼最短路径' disabled={!sendToAi} onClick={() => setDrawer({ type: 'distill' })}>提炼</button>
                <button type='button' className={css.miniAction} title='手动新建最短路径' onClick={() => setDrawer({ type: 'createPlaybook' })}>+ 新建</button>
              </div>
            </div>
            {playbooks.length > 0 ? <div className={css.navStack}>{playbooks.map(pb => {
              const st = state.verify.playbooks?.[pb.id]?.status ?? 'ok'
              return <button key={pb.id} type='button' className={css.playbookNavItem} aria-current={playbookId === pb.id && !inboxId ? 'page' : undefined} title={pb.trigger || pb.summary || pb.title} onClick={() => choosePlaybook(pb.id)}>
                <span className={css.playbookNavTitle}>{pb.title}</span>
                <i className={css.statusDot} data-status={st} title={st === 'ok' ? '锚点有效' : st === 'drifted' ? '锚点已变更' : '锚点缺失'} />
              </button>
            })}</div> : <p className={css.sideHint}>暂无最短路径，可从历史会话或观察者呈报一键提炼</p>}
          </section>
          {!playbookId && !inboxId && graph && view?.chains && view.chains.length > 0 && <section className={css.rules}>
            <div className={css.sectionHead}><h2>方法调用链</h2><span className={css.countPill}>{view.chains.length}</span></div>
            <div className={css.chainList}>
              {view.chains.slice(0, 8).map((chain, idx) => (
                <button
                  key={chain.entry + ':' + idx}
                  type='button'
                  className={css.chainItem}
                  title={`覆盖 ${chain.files.length} 个锚点文件：\n${chain.files.join('\n')}`}
                  onClick={() => sendToAi?.(`请按以下蓝图方法调用链核对或分析业务流程：\`${chain.expression}\`（画布 \`${graph.id}\`）。`)}
                  disabled={!sendToAi}
                >
                  <code className={css.chainExpr}>{chain.expression}</code>
                  <span className={css.chainMeta}>{chain.steps.length} 步 · {chain.files.length} 文件</span>
                </button>
              ))}
            </div>
          </section>}
          {!playbookId && !inboxId && graph && <section className={css.rules}>
            <div className={css.sectionHead}><h2>画布红线</h2><button type='button' className={css.miniAction} onClick={() => setDrawer({ type: 'meta' })} aria-label='编辑画布和红线'>编辑</button></div>
            {graph.invariants.length ? <ul>{graph.invariants.map((text, i) => <li key={i}>{text}</li>)}</ul> : <p className={css.sideHint}>暂无红线</p>}
          </section>}
          <section className={css.library}>
            <div className={css.sectionHead}><h2>共享实体</h2><span className={css.countPill}>{state.entities.length}</span></div>
            <input className={css.searchInput} placeholder='搜索名称或路径' aria-label='搜索共享实体' value={filter} onChange={e => setFilter(e.target.value)} />
            <div className={css.navStack}>
              {available.map(e => <div key={e.id} className={css.libraryItem}>
                <button type='button' title={e.name} draggable onDragStart={event => { event.dataTransfer.setData('application/blueprint-entity', e.id); event.dataTransfer.effectAllowed = 'copy' }} onClick={() => !playbookId && !inboxId && graph && !graph.nodes.some(n => n.entity_id === e.id) ? addEntity(e.id) : inspect(e.id)}><i style={{ background: kindColors[e.kind] }} /><span>{e.name}</span></button>
                <button type='button' className={css.inspectIconBtn} title='检查共享实体' aria-label={'检查 ' + e.name} onClick={() => inspect(e.id)}>•••</button>
              </div>)}
            </div>
            {!available.length && <p className={css.sideHint}>{filter ? '无匹配实体' : '暂无实体'}</p>}
          </section>
        </>}
      </nav>}
      <div ref={canvas} className={css.canvas} data-testid='blueprint-canvas' onDragOver={e => { if (!playbookId && !inboxId && e.dataTransfer.types.includes('application/blueprint-entity')) e.preventDefault() }} onDrop={e => { if (playbookId || inboxId) return; const entity = e.dataTransfer.getData('application/blueprint-entity'); if (entity) { e.preventDefault(); addEntity(entity) } }}>
        {inboxId && activeInboxItem ? (
          <ObserverInboxView
            submission={activeInboxItem}
            existingPlaybooks={playbooks}
            existingGraphs={state?.graphs ?? []}
            busy={busy}
            onMergeWithAi={sendToAi ? (prompt => {
              // Do not optimistically mark merged here; the Blueprint Agent calls `blueprint.resolve_inbox` after saving.
              sendToAi(prompt)
            }) : undefined}
            onSaveAsPlaybook={() => saveInboxAsPlaybook(activeInboxItem)}
            onSetStatus={status => void act(() => api.updateInboxStatus(activeInboxItem.id, status), false)}
            onDelete={() => {
              const target = activeInboxItem
              void appConfirm({ title: '删除观察者呈报', message: '删除战地呈报「' + target.title + '」？', confirmText: '删除', danger: true }).then(ok => {
                if (ok) void act(async () => {
                  await api.deleteInbox(target.id)
                  currentInboxId.current = ''
                  setInboxId('')
                })
              })
            }}
            onOpenSession={openSession}
            onJumpPlaybook={choosePlaybook}
          />
        ) : playbookId ? (activePlaybook ? (
          <PlaybookViewPanel
            playbook={activePlaybook.playbook}
            status={activePlaybook.status}
            graphs={state?.graphs ?? []}
            busy={busy}
            onRunAi={sendToAi}
            onSyncAi={sendToAi}
            onJumpGraph={choose}
            onOpenSession={openSession}
            onEdit={() => setDrawer({ type: 'editPlaybook' })}
            onDelete={() => {
              const target = activePlaybook.playbook
              void appConfirm({ title: '删除最短路径', message: '删除最短路径「' + target.title + '」？', confirmText: '删除', danger: true }).then(ok => {
                if (ok) void act(async () => {
                  await api.deletePlaybook(target.id)
                  currentPlaybookId.current = ''
                  setPlaybookId('')
                  setPlaybookView(undefined)
                })
              })
            }}
          />
        ) : <div className={css.empty}><p>正在加载最短路径…</p></div>) : graph ? <>
          <ReactFlow<Card, Wire> key={id} nodes={cards} edges={wires} nodeTypes={nodeTypes} edgeTypes={edgeTypes} onNodesChange={onNodes} onEdgesChange={onEdges}
            onNodeClick={(_, n) => inspect(n.id)} onEdgeClick={(_, e) => inspectEdge(e.id)} onConnect={connect}
            onNodeDragStart={() => { dragging.current = true }} onNodeDragStop={(_, n, nodes) => stopDrag(nodes.length ? nodes : [n])}
            onSelectionDragStart={() => { dragging.current = true }} onSelectionDragStop={(_, nodes) => stopDrag(nodes)}
            selectionOnDrag panOnDrag={[1, 2]} deleteKeyCode={null} colorMode={dark ? 'dark' : 'light'} minZoom={.15} maxZoom={2.2}>
            <Background variant={BackgroundVariant.Dots} color='var(--bp-grid)' gap={20} size={1.2} />
            <Controls showInteractive={false} />
            {cards.length > 0 && <MiniMap<Card> className={css.minimap} nodeColor={n => kindColors[n.data.entity.kind]} pannable zoomable />}
          </ReactFlow>
          {!cards.length && <div className={css.canvasEmpty}>空白画布，可从左侧拖入共享实体或点击右上角「+ 新建节点」</div>}
        </> : <div className={css.empty}>
          <div className={css.emptyHeroCard}>
            <div className={css.emptyIconHalo} aria-hidden='true'>
              <ParchmentQuillIcon size={58} variant='emblem' />
            </div>
            <h1>{state ? state.exists ? '选择或新建画布 / 最短路径' : '此项目尚未建立架构蓝图' : error ? '无法加载蓝图' : '正在加载架构蓝图'}</h1>
            <p className={css.emptySub}>
              {projects.length > 0 ? `当前项目：${baseName(root)} · ` : ''}
              沉淀架构画布、可执行最短路径与观察者战地呈报，让每次会话少走弯路
            </p>
            {state && <div className={css.emptyActions}>
              <button className={css.primary} type='button' onClick={() => sendToAi?.(firstAnalysis)} disabled={!sendToAi}>启动全项目蓝图分析</button>
              {pendingInboxCount > 0 && (
                <button type='button' className={css.inboxCtaBtn} onClick={() => { const first = inboxItems.find(i => i.status === 'pending') ?? inboxItems[0]; if (first) chooseInbox(first.id) }}>
                  审阅观察者战地呈报 ({pendingInboxCount})
                </button>
              )}
              <button type='button' className={css.secondaryBtn} onClick={() => setDrawer({ type: 'distill' })} disabled={!sendToAi}><SparkleIcon /><span>从历史会话提炼</span></button>
              <button type='button' className={css.secondaryBtn} onClick={showCreate}>{state.exists ? '新建画布' : '手动新建空白蓝图'}</button>
            </div>}
          </div>
        </div>}
        {notice && <div role='status' className={css.notice}>{notice}</div>}
      </div>
      {drawer && <Drawer title={
        drawer.type === 'create' ? '新建画布'
          : drawer.type === 'meta' ? '画布与红线'
            : drawer.type === 'edge' ? '连线检查器'
              : drawer.type === 'impact' ? '影响面'
                : drawer.type === 'scope' ? '分析范围'
                  : drawer.type === 'distill' ? '从历史会话提炼最短路径'
                    : drawer.type === 'createPlaybook' ? '新建最短路径'
                      : drawer.type === 'editPlaybook' ? '编辑最短路径'
                        : '实体检查器'
      } close={() => setDrawer(undefined)}>
        {drawer.type === 'create' && <GraphForm project={state?.manifest?.project_kind} busy={busy} save={(title, entry_kind, entry_summary, project_kind) => void act(async () => { const created = await api.create({ id: idOf('canvas'), title, entry_kind, entry_summary, ...(state?.exists ? {} : { project_kind }) }); choose(created.id) })} />}
        {drawer.type === 'meta' && graph && <GraphForm key={graph.id} graph={graph} busy={busy} save={(title, entry_kind, entry_summary, _, invariants) => saveGraph({ title, entry_kind, entry_summary, invariants })} remove={() => { void appConfirm({ title: '删除画布', message: '删除画布「' + graph.title + '」？共享实体不会删除。', confirmText: '删除', danger: true }).then(ok => { if (ok) void act(() => api.deleteGraph(graph.id)) }) }} />}
        {drawer.type === 'scope' && <AnalysisScope run={scope => { sendToAi?.(scope ? `分析模块或页面「${scope}」的架构蓝图。` : firstAnalysis); setDrawer(undefined) }} />}
        {drawer.type === 'distill' && <DistillSessionsForm
          root={root}
          sessions={sessions}
          existingPlaybooks={playbooks}
          run={prompt => { sendToAi?.(prompt); setDrawer(undefined) }}
        />}
        {drawer.type === 'createPlaybook' && <PlaybookForm
          project={state?.manifest?.project_kind}
          busy={busy}
          save={(pbId, payload) => void act(async () => {
            const created = await api.createPlaybook({ ...payload, id: pbId })
            choosePlaybook(created.id)
          })}
        />}
        {drawer.type === 'editPlaybook' && activePlaybook && <PlaybookForm
          key={activePlaybook.playbook.id}
          playbook={activePlaybook.playbook}
          busy={busy}
          save={(pbId, payload) => void act(() => api.savePlaybook(pbId, payload))}
          remove={() => {
            const target = activePlaybook.playbook
            void appConfirm({ title: '删除最短路径', message: '删除最短路径「' + target.title + '」？', confirmText: '删除', danger: true }).then(ok => {
              if (ok) void act(async () => {
                await api.deletePlaybook(target.id)
                currentPlaybookId.current = ''
                setPlaybookId('')
                setPlaybookView(undefined)
              })
            })
          }}
        />}
        {drawer.type === 'entity' && inspectedEntity && <EntityEditor key={drawer.id + '/' + (drawer.port ?? '') + '/' + !!drawer.addPort} initial={inspectedEntity} isNew={!!drawer.draft} initialPort={drawer.port} addingPort={drawer.addPort} api={api} busy={busy}
          anchor={drawer.port ? view?.statuses[drawer.id]?.ports[drawer.port] : undefined}
          onImpact={port => { setImpactMode(true); inspectImpact(drawer.id, port) }}
          save={entity => void act(async () => {
            if (drawer.draft && state?.entities.some(e => e.id === entity.id)) throw new Error('该实体 ID 已存在')
            await api.putEntity(entity)
            if (drawer.draft && graph) await api.save(graph.id, { nodes: [...nodesOnly(graph.nodes), { entity_id: entity.id }] })
          })}
          remove={() => { void appConfirm({ title: '删除共享实体', message: '删除此共享实体及其所有画布引用？', confirmText: '删除', danger: true }).then(ok => { if (ok) void act(() => api.deleteEntity(drawer.id)) }) }}
          removeNode={graph?.nodes.some(n => n.entity_id === drawer.id) ? () => saveGraph({ nodes: nodesOnly(graph.nodes.filter(n => n.entity_id !== drawer.id)) }) : undefined} />}
        {drawer.type === 'edge' && inspectedEdge && graph && <EdgeEditor key={inspectedEdge.id} initial={inspectedEdge} busy={busy}
          save={edge => saveGraph({ edges: [...graph.edges.filter(e => e.id !== edge.id), edge] })}
          remove={drawer.draft ? undefined : () => saveGraph({ edges: graph.edges.filter(e => e.id !== drawer.id) })} />}
        {drawer.type === 'impact' && (impact ? <ImpactPanel report={impact} jump={choose} graphTitle={key => state?.graphs.find(g => g.id === key)?.title ?? key} /> : <p>正在计算影响面…</p>)}
      </Drawer>}
    </div>
  </section>
}

function GraphForm({ graph, project, busy, save, remove }: { graph?: Graph; project?: ProjectKind | undefined; busy: boolean; save: (title: string, kind: EntryKind, summary: string, project: ProjectKind, invariants: string[]) => void; remove?: () => void }) {
  const [title, setTitle] = useState(graph?.title ?? '')
  const [kind, setKind] = useState<EntryKind>(graph?.entry_kind ?? 'ui_page')
  const [projectKind, setProjectKind] = useState<ProjectKind>(project ?? 'fullstack')
  const [summary, setSummary] = useState(graph?.entry_summary ?? '')
  const [rules, setRules] = useState(graph?.invariants.join('\n') ?? '')
  return <form className={css.form} onSubmit={e => { e.preventDefault(); save(title.trim(), kind, summary, projectKind, rules.split('\n').map(r => r.trim()).filter(Boolean)) }}>
    <Field label='画布名称'><input autoFocus required value={title} onChange={e => setTitle(e.target.value)} /></Field>
    {!graph && !project && <Field label='项目形态'><Choices value={projectKind} values={projectKinds} onChange={setProjectKind} /></Field>}
    <Field label='入口类型'><Choices value={kind} values={entryKinds} labels={entryLabels} onChange={setKind} /></Field>
    <Field label='入口摘要'><textarea rows={2} value={summary} onChange={e => setSummary(e.target.value)} /></Field>
    {graph && <Field label='画布红线（每行一条）'><textarea rows={6} value={rules} onChange={e => setRules(e.target.value)} /></Field>}
    <button type='submit' className={css.primary} disabled={busy}>{graph ? '保存画布' : '创建画布'}</button>
    {remove && <button type='button' className={css.warning} disabled={busy} onClick={remove}>删除画布</button>}
  </form>
}
function AnalysisScope({ run }: { run: (scope: string) => void }) {
  const [scope, setScope] = useState('')
  return <form className={css.form} onSubmit={e => { e.preventDefault(); run(scope.trim()) }}>
    <button type='button' className={css.secondaryBtn} onClick={() => run('')}>全项目分析</button>
    <Field label='模块或页面'><input required value={scope} onChange={e => setScope(e.target.value)} /></Field>
    <button type='submit' className={css.primary}>分析指定模块或页面</button>
  </form>
}

function DistillSessionsForm({
  root,
  sessions,
  existingPlaybooks,
  run,
}: {
  root: string
  sessions: readonly Task[]
  existingPlaybooks: readonly { id: string; title: string }[]
  run: (prompt: string) => void
}) {
  const [query, setQuery] = useState('')
  const [showAll, setShowAll] = useState(false)
  const [focus, setFocus] = useState('')
  const [selected, setSelected] = useState<string[]>([])
  const filtered = useMemo(() => filterWorkspaceSessions(sessions, root, query, showAll), [sessions, root, query, showAll])
  const toggle = (taskId: string) => {
    setSelected(cur => cur.includes(taskId) ? cur.filter(x => x !== taskId) : [...cur, taskId])
  }
  const chosenTasks = sessions.filter(t => selected.includes(t.task_id))
  return <form className={css.form} onSubmit={e => {
    e.preventDefault()
    if (!chosenTasks.length) return
    run(buildDistillPlaybookPrompt(chosenTasks, existingPlaybooks, focus))
  }}>
    <p className={css.playbookEmptyHint}>勾选 1 个或多个历史会话，AI 会分析当时的编排代码、剔除弯路并对照当前最新源码，提炼为可复用的最短路径蓝图。</p>
    <Field label='搜索会话'><input value={query} onChange={e => setQuery(e.target.value)} placeholder='按标题或 task_id 筛选' /></Field>
    <label className={css.distillToggle}>
      <input type='checkbox' checked={showAll} onChange={e => setShowAll(e.target.checked)} />
      <span>显示全部工作区的历史会话</span>
    </label>
    <div className={css.distillList}>
      {filtered.slice(0, 40).map(task => {
        const checked = selected.includes(task.task_id)
        return <label key={task.task_id} className={css.distillRow}>
          <input type='checkbox' checked={checked} onChange={() => toggle(task.task_id)} />
          <span className={css.distillRowBody}>
            <span className={css.distillRowTitle} title={task.title || task.task_id}>{task.title?.trim() || '未命名会话'}</span>
            <span className={css.distillRowMeta}>{baseName(task.workspace || '')} · {task.status} · {task.task_id.slice(0, 8)}</span>
          </span>
        </label>
      })}
      {!filtered.length && <p className={css.playbookEmptyHint}>当前筛选下没有历史会话</p>}
    </div>
    <Field label='补充提炼目标（可选）'>
      <textarea rows={2} value={focus} onChange={e => setFocus(e.target.value)} placeholder='例如：只提炼发版构建与验收最短路径，忽略排查日志的过程' />
    </Field>
    <button type='submit' className={css.primary} disabled={!chosenTasks.length}>
      让 AI 提炼为最短路径（已选 {chosenTasks.length} 个会话）
    </button>
  </form>
}
