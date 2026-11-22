import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { ReactFlow, Controls, MarkerType, type Edge, type Node, type NodeChange, type ReactFlowInstance } from '@xyflow/react'
import '@xyflow/react/dist/style.css'

import type { AgentEvent, Task } from '../api.ts'
import { chatItems } from '../model.ts'
import { buildFlow, flowTurns, formatSpan, layoutFlow, FLOW_WIDTH, type FlowNode as FlowGraphNode } from '../flowGraph.ts'
import { FlowNodeView, type FlowNodeData } from './FlowNode.tsx'
import styles from './FlowView.module.css'

const nodeTypes = { flow: FlowNodeView }

/** What each kind of node is called where the graph says it in one word. */
const KINDS: Readonly<Record<FlowGraphNode['kind'], string>> = {
  request: '用户请求', step: 'AI 步骤', tool: '工具调用', call: '函数调用', program: '程序',
  delegate: '子代理', note: '插话', reply: '回复', end: '结束',
}

const STATE_TEXT: Readonly<Record<string, string>> = {
  pending: '等待开始', running: '进行中', done: '完成', failed: '失败', interrupted: '已中断',
}

function pretty(value: unknown): string {
  if (typeof value === 'string') {
    try { return JSON.stringify(JSON.parse(value), null, 2) } catch { return value }
  }
  return JSON.stringify(value, null, 2) ?? ''
}

/**
 * The turn as a flow: every step the AI took, the tools it called under the
 * step that called them, and the ending. Built from the conversation's own
 * events, so it is there for every turn the transcript can show.
 */
export function FlowView({ taskId, events, task, running }: {
  taskId?: string | undefined
  events: readonly AgentEvent[]
  task?: Task | undefined
  running: boolean
}) {
  const items = useMemo(() => chatItems(events, task), [events, task])
  const turns = useMemo(() => flowTurns(items), [items])
  // Null follows the latest turn, as the transcript follows the live one.
  const [chosen, setChosen] = useState<number | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [canvas, setCanvas] = useState<ReactFlowInstance | null>(null)
  const wrap = useRef<HTMLDivElement | null>(null)
  useEffect(() => { setChosen(null); setSelectedId(null) }, [taskId])
  const shown = turns.find(entry => entry.turn === chosen) ?? turns.at(-1)
  const graph = useMemo(() => shown === undefined ? { nodes: [], edges: [] } : buildFlow(items, shown.turn), [items, shown])

  /** Picking a node on the canvas picks it here too, so the two never disagree. */
  const onNodesChange = useCallback((changes: NodeChange[]) => {
    setSelectedId(current => {
      let next = current
      for (const change of changes) {
        if (change.type !== 'select') continue
        if (change.selected) next = change.id
        else if (current === change.id) next = null
      }
      return next
    })
  }, [])

  const { nodes, edges, width } = useMemo(() => {
    // The nodes draw at the heights the layout reserved for them, so an edge
    // leaves one box and lands on the next one instead of crossing it.
    const placements = layoutFlow(graph.nodes)
    const nodes: Node<FlowNodeData>[] = graph.nodes.map(node => ({
      id: node.id,
      type: 'flow',
      position: { x: placements.get(node.id)?.x ?? 0, y: placements.get(node.id)?.y ?? 0 },
      data: { node },
      selected: node.id === selectedId,
    }))
    const edges: Edge[] = graph.edges.map(edge => ({
      id: edge.id,
      source: edge.source,
      target: edge.target,
      sourceHandle: 'out',
      targetHandle: 'in',
      type: 'default',
      animated: edge.status === 'running',
      markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: edge.status === 'failed' ? '#e8a3a3' : edge.branch ? '#c2ccd9' : '#9aa7b8' },
      style: {
        // A call reads as a branch of the step that made it, not as the next step.
        stroke: edge.status === 'failed' ? '#e8a3a3' : edge.branch ? '#c2ccd9' : '#9aa7b8',
        strokeWidth: edge.branch ? 1.4 : 1.6,
        strokeDasharray: edge.branch ? '4 3' : undefined,
      },
    }))
    const width = Math.max(FLOW_WIDTH, ...[...placements.values()].map(place => place.x + place.width))
    return { nodes, edges, width }
  }, [graph, selectedId])

  /** A turn is tall, not wide: show it at reading size from its first step. */
  const showTop = useCallback(() => {
    if (canvas === null) return
    const room = wrap.current?.clientWidth ?? 0
    if (room === 0) return
    const zoom = Math.min(1, (room - 48) / width)
    canvas.setViewport({ x: Math.max(20, (room - width * zoom) / 2), y: 16, zoom }, { duration: 0 })
  }, [canvas, width])
  useEffect(() => { showTop() }, [showTop, shown?.turn])

  // The drawer takes the right of the canvas: keep the column where it can be read.
  useEffect(() => {
    if (canvas === null) return
    const room = wrap.current?.clientWidth ?? 0
    if (room === 0) return
    const viewport = canvas.getViewport()
    canvas.setViewport({ ...viewport, x: Math.max(20, (room - width * viewport.zoom) / 2) }, { duration: 160 })
  }, [canvas, width, selectedId])

  const selected = graph.nodes.find(node => node.id === selectedId)
  useEffect(() => {
    if (selectedId === null) return
    const close = (event: KeyboardEvent) => { if (event.key === 'Escape') setSelectedId(null) }
    window.addEventListener('keydown', close)
    return () => window.removeEventListener('keydown', close)
  }, [selectedId])

  if (taskId === undefined) return <div className={styles.empty}>选择一个对话后显示它的流程。</div>
  return (
    <div className={styles.container}>
      <div className={styles.topBar}>
        <strong className={styles.heading}>流程图</strong>
        {turns.length > 0 && (
          <select
            className={styles.turnPicker}
            aria-label="选择要查看的轮次"
            value={shown?.turn ?? ''}
            onChange={event => { setChosen(Number(event.target.value)); setSelectedId(null) }}
          >
            {turns.map(turn => (
              <option key={turn.turn} value={turn.turn}>第 {turn.turn} 轮 · {turn.title}</option>
            ))}
          </select>
        )}
        {shown !== undefined && (
          <span className={styles.summaryText}>
            {[
              STATE_TEXT[shown.status] ?? shown.status,
              String(shown.steps) + ' 步',
              shown.tools === 0 ? '' : String(shown.tools) + ' 次工具',
              shown.durationMs === undefined ? '' : formatSpan(shown.durationMs),
            ].filter(Boolean).join(' · ')}
          </span>
        )}
        <span className={styles.spacer} />
        {running && <span className={styles.live}>运行中</span>}
        <button type="button" className={styles.pill} onClick={() => { void canvas?.fitView({ padding: 0.16, duration: 220 }) }}>适应画布</button>
        <button type="button" className={styles.pill} onClick={showTop}>回到开头</button>
      </div>
      <div className={styles.canvas}>
        {/* The drawer takes the right of the canvas; the graph keeps to what is left. */}
        <div className={styles.graph} ref={wrap} data-narrow={selected !== undefined || undefined}>
          {shown === undefined
            ? <div className={styles.empty}>这个对话还没有开始执行。</div>
            : (
              <ReactFlow
              // Each turn is its own picture: it opens at its own first step.
              key={shown.turn}
              nodes={nodes}
              edges={edges}
              nodeTypes={nodeTypes}
              onInit={setCanvas}
              onNodesChange={onNodesChange}
              onNodeClick={(_, node) => setSelectedId(node.id)}
              onPaneClick={() => setSelectedId(null)}
              minZoom={0.15}
              maxZoom={2}
              proOptions={{ hideAttribution: true }}
            >
                <Controls showInteractive={false} />
              </ReactFlow>
            )}
        </div>
        {selected !== undefined && <NodeDetail node={selected} onClose={() => setSelectedId(null)} />}
      </div>
    </div>
  )
}

/** One node in full: what it was asked, what it answered, what it returned. */
function NodeDetail({ node, onClose }: { node: FlowGraphNode; onClose: () => void }) {
  return (
    <aside className={styles.drawer} aria-label="步骤详情">
      <div className={styles.drawerHeader}>
        <strong title={node.title}>{node.title}</strong>
        <button type="button" className={styles.close} onClick={onClose} title="关闭">✕</button>
      </div>
      <div className={styles.drawerBody}>
        <p className={styles.meta}>
          {[KINDS[node.kind], STATE_TEXT[node.status] ?? node.status, node.meta, node.items.length > 1 ? String(node.items.length) + ' 个调用' : ''].filter(Boolean).join(' · ')}
        </p>
        {node.summary !== '' && <p className={styles.text}>{node.summary}</p>}
        {node.items.map((item, index) => (
          <section key={item.key + ':' + String(index)}>
            {item.kind === 'tool' && (
              <ToolDetail name={item.name} arguments={item.arguments} result={item.result} />
            )}
            {item.kind === 'assistant' && (
              <AssistantDetail reasoning={item.reasoning} text={item.text} />
            )}
            {item.kind === 'call' && (
              <CallDetail description={item.description} result={item.result} error={item.error} steps={item.steps.length} />
            )}
            {item.kind === 'program' && (<><h4>程序输出</h4><pre className={styles.value}>{item.text}</pre></>)}
            {item.kind === 'interjection' && (<><h4>插话</h4><p className={styles.text}>{item.text}</p></>)}
            {item.kind === 'user' && (<><h4>用户的请求</h4><p className={styles.text}>{item.text}</p></>)}
            {item.kind === 'subagent' && (<><h4>子代理的报告</h4><p className={styles.text}>{item.report || '（还没有报告）'}</p></>)}
            {item.kind === 'turn-end' && (
              <EndDetail reason={item.reason} durationMs={item.durationMs} />
            )}
          </section>
        ))}
      </div>
    </aside>
  )
}

function ToolDetail({ name, arguments: raw, result }: { name: string; arguments: string; result: { readonly text: string; readonly isError: boolean } | undefined }) {
  const state = result === undefined ? '还在运行' : result.isError ? '失败' : '完成'
  return (
    <>
      <h4>{name} · {state}</h4>
      <pre className={styles.value}>{pretty(raw)}</pre>
      {result !== undefined && <pre className={result.isError ? styles.failedValue : styles.returned}>{result.text}</pre>}
    </>
  )
}

function AssistantDetail({ reasoning, text }: { reasoning: string; text: string }) {
  return (
    <>
      {reasoning.trim() !== '' && <><h4>思考</h4><pre className={styles.value}>{reasoning.trim()}</pre></>}
      {text.trim() !== '' && <><h4>回复</h4><pre className={styles.value}>{text.trim()}</pre></>}
      {reasoning.trim() === '' && text.trim() === '' && <p className={styles.meta}>这一步没有留下文字。</p>}
    </>
  )
}

function CallDetail({ description, result, error, steps }: { description: string; result: string | undefined; error: string | undefined; steps: number }) {
  return (
    <>
      <h4>契约</h4>
      <p className={styles.text}>{description || '（没有契约注释）'}</p>
      {result !== undefined && result !== '' && <><h4>返回</h4><pre className={styles.returned}>{pretty(result)}</pre></>}
      {error !== undefined && <><h4>错误</h4><p className={styles.text + ' ' + styles.errorText}>{error}</p></>}
      {steps > 0 && <p className={styles.meta}>内部有 {steps} 个步骤，画在它的下方。</p>}
    </>
  )
}

function EndDetail({ reason, durationMs }: { reason: { readonly kind: string; readonly message?: string | undefined }; durationMs: number | undefined }) {
  return (
    <>
      <h4>结局</h4>
      <p className={styles.text}>
        {reason.kind === 'completed' ? '目标完成'
          : reason.kind === 'aborted' ? '你取消了这一轮'
          : reason.kind === 'interrupted' ? '这一轮被中断'
          : '这一轮出错：' + (reason.message ?? '')}
      </p>
      {durationMs !== undefined && <p className={styles.meta}>耗时 {formatSpan(durationMs)}</p>}
    </>
  )
}
