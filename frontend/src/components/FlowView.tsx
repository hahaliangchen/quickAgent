import { useCallback, useEffect, useMemo, useState } from 'react'
import { ReactFlow, Controls, type Edge, type Node, type NodeChange } from '@xyflow/react'
import '@xyflow/react/dist/style.css'

import type { FrameDetail, FrameSummary } from '../api.ts'
import { useAgentApi } from '../cordis/react.tsx'
import { chatItems } from '../model.ts'
import { layoutFrameTree } from '../frameLayout.ts'
import { FRAME_STATES, FrameNode, frameActivity, frameDuration, frameState, type FrameNodeData } from './FrameNode.tsx'
import { ProcessList } from './ProcessList.tsx'
import { RequestContextPanel } from './RequestContextPanel.tsx'
import styles from './FlowView.module.css'

const nodeTypes = { frame: FrameNode }
const ACTIVE = new Set(['pending', 'running', 'suspended', 'paused'])
const POLL_MS = 1500
/** The frames a program opened: the calls the graph draws below a request. */
const CALL_KINDS = new Set(['virtual', 'function', 'module', 'paused_program'])

/** The frames under one request, the request first. */
function subtree(frames: readonly FrameSummary[], root: string): FrameSummary[] {
  const calls = new Map<string, FrameSummary[]>()
  for (const frame of frames) if (frame.caller_id !== null) calls.set(frame.caller_id, [...calls.get(frame.caller_id) ?? [], frame])
  const found: FrameSummary[] = []
  const seen = new Set<string>()
  const visit = (frame: FrameSummary) => {
    if (seen.has(frame.id)) return
    seen.add(frame.id)
    found.push(frame)
    for (const call of calls.get(frame.id) ?? []) visit(call)
  }
  const top = frames.find(frame => frame.id === root)
  if (top) visit(top)
  return found
}

/**
 * The conversation's function frames, read from the agent by this view
 * itself: one request at a time, each call below the frame whose program made it.
 */
export function FlowView({ taskId, running }: { taskId?: string | undefined; running: boolean }) {
  const api = useAgentApi()
  const [frames, setFrames] = useState<readonly FrameSummary[]>()
  const [loadError, setLoadError] = useState('')
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    setFrames(undefined)
    setLoadError('')
    if (taskId === undefined) return
    let alive = true
    const load = () => {
      api.frames(taskId).then(
        (result) => { if (alive) { setFrames(result.frames); setLoadError(''); setNow(Date.now()) } },
        (reason: unknown) => { if (alive) setLoadError(String(reason)) },
      )
    }
    load()
    // Running frames change while the agent works; a settled conversation is read once.
    const timer = running ? window.setInterval(load, POLL_MS) : undefined
    return () => { alive = false; if (timer !== undefined) window.clearInterval(timer) }
  }, [api, taskId, running])

  const requests = useMemo(() => (frames ?? []).filter(frame => frame.kind === 'request'), [frames])
  // Null follows the latest request.
  const [chosen, setChosen] = useState<string | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  useEffect(() => { setChosen(null); setSelectedId(null) }, [taskId])
  const request = requests.find(frame => frame.id === chosen) ?? requests.at(-1)
  const shown = useMemo(() => {
    if (frames === undefined) return []
    if (request !== undefined) return subtree(frames, request.id)
    // Records from before function frames: show whatever frames there are.
    return frames.filter(frame => frame.kind !== 'conversation')
  }, [frames, request])

  // A turn whose AI worked without a program draws one frame: no call sits below
  // the request, so the tools it used are named there instead of drawn.
  const calls = useMemo(() => shown.filter(frame => CALL_KINDS.has(frame.kind)), [shown])
  const toolCalls = useMemo(() => shown.reduce((sum, frame) => sum + (frame.tool_calls ?? 0), 0), [shown])

  const [measured, setMeasured] = useState<Record<string, { width: number; height: number }>>({})
  const onNodesChange = useCallback((changes: NodeChange[]) => {
    setMeasured(previous => {
      let next = previous
      for (const change of changes) {
        if (change.type !== 'dimensions' || !change.dimensions) continue
        const old = previous[change.id]
        if (old?.width === change.dimensions.width && old.height === change.dimensions.height) continue
        if (next === previous) next = { ...previous }
        next[change.id] = change.dimensions
      }
      return next
    })
  }, [])

  const { nodes, edges } = useMemo(() => {
    const placements = layoutFrameTree(shown.map(frame => ({ id: frame.id, caller: frame.caller_id })))
    const ids = new Set(shown.map(frame => frame.id))
    const nodes: Node<FrameNodeData>[] = shown.map(frame => ({
      id: frame.id,
      type: 'frame',
      position: placements.get(frame.id) ?? { x: 0, y: 0 },
      data: { frame, now },
      selected: frame.id === selectedId,
      ...(measured[frame.id] ? { measured: measured[frame.id] } : {}),
    }))
    const edges: Edge[] = shown.filter(frame => frame.caller_id !== null && ids.has(frame.caller_id)).map(frame => ({
      id: `${frame.caller_id}->${frame.id}`,
      source: frame.caller_id!,
      target: frame.id,
      sourceHandle: 'out',
      targetHandle: 'in',
      type: 'smoothstep',
      animated: frame.status === 'running',
      style: { stroke: frame.status === 'failed' ? '#e8a3a3' : '#94a3b8', strokeWidth: 1.5 },
    }))
    return { nodes, edges }
  }, [shown, selectedId, measured, now])

  const selected = shown.find(frame => frame.id === selectedId)
  const [debug, setDebug] = useState(false)
  const [debugError, setDebugError] = useState('')
  const [allRequests, setAllRequests] = useState(false)
  useEffect(() => {
    let alive = true
    setDebug(false); setAllRequests(false); setDebugError('')
    if (taskId === undefined) return
    api.contextDebug(taskId).then(result => { if (alive) setDebug(result.enabled) }, (reason: unknown) => { if (alive) setDebugError(String(reason)) })
    return () => { alive = false }
  }, [api, taskId])
  const toggleDebug = () => {
    if (taskId === undefined) return
    setDebugError('')
    api.setContextDebug(taskId, !debug).then(result => {
      setDebug(result.enabled)
      if (!result.enabled) setAllRequests(false)
    }, (reason: unknown) => { setDebugError(String(reason)) })
  }
  useEffect(() => {
    if (selectedId === null && !allRequests) return
    const close = (event: KeyboardEvent) => { if (event.key === 'Escape') { setSelectedId(null); setAllRequests(false) } }
    window.addEventListener('keydown', close)
    return () => window.removeEventListener('keydown', close)
  }, [selectedId, allRequests])
  // New model requests show up as the frames move on.
  const version = useMemo(() => Math.max(-1, ...(frames ?? []).map(frame => frame.last_seq ?? -1)), [frames])

  if (taskId === undefined) return <div className={styles.empty}>选择一个对话后显示它的流程。</div>
  return (
    <div className={styles.container}>
      <div className={styles.topBar}>
        <strong className={styles.heading}>流程图</strong>
        {requests.length > 0 && (
          <select className={styles.turnPicker} aria-label="选择要查看的请求" value={request?.id ?? ''}
            onChange={event => { setChosen(event.target.value === requests.at(-1)?.id ? null : event.target.value); setSelectedId(null) }}>
            {requests.map(frame => <option key={frame.id} value={frame.id}>第 {frame.turn ?? '?'} 轮 · {frame.title.slice(0, 40)}</option>)}
          </select>
        )}
        {request && <span className={styles.requestState}>{FRAME_STATES[request.status] ?? request.status}{frameDuration(request, now) && ` · ${frameDuration(request, now)}`}</span>}
        <span className={styles.spacer} />
        {loadError && <span role="alert" className={styles.alert} title={loadError}>读取流程失败</span>}
        <button type="button" className={styles.pill} aria-pressed={debug} onClick={toggleDebug}
          title="显示或隐藏每个函数实际发出的模型请求">{debug ? '上下文：显示' : '上下文：隐藏'}</button>
        {debug && <button type="button" className={styles.pill} onClick={() => { setAllRequests(true); setSelectedId(null) }}>全部请求</button>}
        {debugError && <span role="alert" className={styles.alert} title={debugError}>上下文设置失败</span>}
      </div>
      <div className={styles.canvas}>
        {frames !== undefined && shown.length === 0
          ? <div className={styles.empty}>这个对话还没有开始执行。</div>
          : (
            <ReactFlow
              // Each request is its own picture: fit it when it is shown.
              key={request?.id ?? 'all'}
              nodes={nodes}
              edges={edges}
              nodeTypes={nodeTypes}
              onNodesChange={onNodesChange}
              onNodeClick={(_, node) => { setSelectedId(node.id); setAllRequests(false) }}
              onPaneClick={() => { setSelectedId(null); setAllRequests(false) }}
              fitView
              fitViewOptions={{ padding: 0.2 }}
              minZoom={0.2}
              maxZoom={1.5}
              proOptions={{ hideAttribution: true }}
            >
              <Controls />
            </ReactFlow>
          )}
        {request !== undefined && calls.length === 0 && (
          <button type="button" className={styles.note} title="点开请求节点，查看它的执行过程"
            onClick={() => { setSelectedId(request.id); setAllRequests(false) }}>
            {toolCalls === 0
              ? '本轮没有函数调用，也还没有工具调用：点开节点查看执行过程。'
              : `本轮没有函数调用：直接调用了 ${toolCalls} 次工具，点开节点查看执行过程。`}
          </button>
        )}
        {allRequests && debug && (
          <aside className={styles.drawer}>
            <div className={styles.drawerHeader}>
              <strong>全部模型请求</strong>
              <button type="button" className={styles.close} onClick={() => setAllRequests(false)} title="关闭">✕</button>
            </div>
            <div className={styles.drawerBody}><RequestContextPanel taskId={taskId} version={version} /></div>
          </aside>
        )}
        {selected && !allRequests && (
          <FrameDrawer taskId={taskId} frame={selected} now={now} debug={debug} version={version} onClose={() => setSelectedId(null)} />
        )}
      </div>
    </div>
  )
}

function pretty(value: unknown): string {
  if (typeof value === 'string') {
    try { return JSON.stringify(JSON.parse(value), null, 2) } catch { return value }
  }
  return JSON.stringify(value, null, 2) ?? ''
}

/** The caller's source around the call, the call's line marked; all of it on request. */
function CallerSource({ program }: { program: NonNullable<FrameDetail['caller_program']> }) {
  const [whole, setWhole] = useState(false)
  const lines = program.source.replace(/\r\n/g, '\n').split('\n')
  const at = program.call_site.line
  const from = whole ? 1 : Math.max(1, at - 6)
  const to = whole ? lines.length : Math.min(lines.length, at + 6)
  return (
    <div className={styles.source}>
      <pre>{lines.slice(from - 1, to).map((text, index) => {
        const number = from + index
        return <div key={number} className={number === at ? styles.callLine : undefined}><span className={styles.lineNumber}>{number}</span>{text}</div>
      })}</pre>
      {lines.length > to - from + 1 || whole
        ? <button type="button" className={styles.link} onClick={() => setWhole(value => !value)}>{whole ? '只看调用处' : `显示整个程序（${lines.length} 行）`}</button>
        : null}
    </div>
  )
}

/** One frame in detail: read from the agent when opened, and again whenever the frame moves on. */
function FrameDrawer({ taskId, frame, now, debug, version, onClose }: {
  taskId: string
  frame: FrameSummary
  now: number
  debug: boolean
  version: number
  onClose: () => void
}) {
  const api = useAgentApi()
  const [detail, setDetail] = useState<FrameDetail>()
  const [error, setError] = useState('')
  useEffect(() => { setDetail(undefined); setError('') }, [frame.id])
  useEffect(() => {
    let alive = true
    api.frame(taskId, frame.id).then(
      (result) => { if (alive) { setDetail(result); setError('') } },
      (reason: unknown) => { if (alive) setError(String(reason)) },
    )
    return () => { alive = false }
  }, [api, taskId, frame.id, frame.last_seq, frame.status])
  const active = ACTIVE.has(frame.status)
  const items = useMemo(() => detail ? chatItems(detail.events) : [], [detail])
  const parameters = Array.isArray(detail?.frame.parameters) ? detail.frame.parameters as unknown[] : []
  const args = Array.isArray(detail?.frame.arguments) ? detail.frame.arguments as unknown[] : []
  const captured = detail?.frame.captured !== null && typeof detail?.frame.captured === 'object' ? Object.entries(detail.frame.captured as Record<string, unknown>) : []
  const returned = detail?.frame.result !== null && typeof detail?.frame.result === 'object' ? (detail.frame.result as Record<string, unknown>).result : undefined
  const name = frame.kind === 'request' ? frame.title : frame.function ?? frame.title
  const live = frame.status === 'running' ? frameActivity(frame) : ''
  return (
    <aside className={styles.drawer} aria-label="函数帧详情">
      <div className={styles.drawerHeader}>
        <strong title={name}>{name}</strong>
        <button type="button" className={styles.close} onClick={onClose} title="关闭">✕</button>
      </div>
      <div className={styles.drawerBody}>
        <p className={styles.meta}>
          {[frameState(frame), frameDuration(frame, now), frame.turn === null ? '' : `第 ${frame.turn} 轮`,
            frame.call_site ? `第 ${frame.call_site.line} 行调用` : '', frame.concurrent && frame.kind !== 'request' ? '并发调用' : ''].filter(Boolean).join(' · ')}
        </p>
        {live !== '' && <p className={styles.live}>正在：{live}</p>}
        {error && <p role="alert" className={styles.alert}>{error}</p>}
        <section>
          <h4>{frame.kind === 'request' ? '用户的请求' : '契约'}</h4>
          <p className={styles.text}>{frame.kind === 'request' ? detail?.frame.goal ?? frame.title : frame.description || '（没有契约注释）'}</p>
        </section>
        {parameters.length > 0 && (
          <section>
            <h4>参数</h4>
            {parameters.map((parameter, index) => (
              <details key={index} className={styles.value}>
                <summary>{String(parameter)}</summary>
                <pre>{pretty(args[index])}</pre>
              </details>
            ))}
          </section>
        )}
        {captured.length > 0 && (
          <section>
            <h4>@uses 读取的变量</h4>
            {captured.map(([variable, value]) => (
              <details key={variable} className={styles.value}><summary>{variable}</summary><pre>{pretty(value)}</pre></details>
            ))}
          </section>
        )}
        {frame.error && <section><h4>错误</h4><p className={`${styles.text} ${styles.errorText}`}>{frame.error}</p></section>}
        {frame.status === 'done' && returned !== undefined && returned !== '' && (
          <section>
            <h4>{frame.kind === 'request' ? '结局' : '返回值'}</h4>
            <pre className={styles.returned}>{pretty(returned)}</pre>
          </section>
        )}
        {detail?.caller_program && (
          <section>
            <h4>调用处</h4>
            <CallerSource program={detail.caller_program} />
          </section>
        )}
        <section>
          <h4>执行过程</h4>
          {detail === undefined ? <p className={styles.hint}>正在读取…</p>
            : items.length === 0 ? <p className={styles.hint}>还没有记录。</p>
              : <div className={styles.steps}><ProcessList items={items} closed={!active} onOpenTask={() => {}} renderItem={() => null} /></div>}
        </section>
        {debug && (
          <section>
            <h4>这个函数发出的模型请求</h4>
            <RequestContextPanel key={frame.id} taskId={taskId} frameId={frame.id} version={version} />
          </section>
        )}
      </div>
    </aside>
  )
}
