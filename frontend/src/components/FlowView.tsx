import { ConversationTaskProvider } from './ConversationTask.tsx'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { ReactFlow, Controls, MarkerType, type Edge, type Node, type NodeChange, type ReactFlowInstance } from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import { extractMarkdownPlainText, MarkdownText } from '@deepseek-ai/dsh-client-ui-primitives'

import type { AgentEvent, Task } from '../api.ts'
import { chatItems, type ChatItem, type TurnEndReason } from '../model.ts'
import {
  buildFlow, flowTurns, formatSpan, layoutFlow, FLOW_WIDTH,
  type FlowKind, type FlowNode as FlowGraphNode, type FlowStatus, type FlowTurn,
} from '../flowGraph.ts'
import { FlowNodeView, type FlowNodeData } from './FlowNode.tsx'
import { ToolRow } from './ToolRow.tsx'
import { markdownLabels } from '../dsh/markdown-labels.ts'
import { t } from '../i18n.ts'
import styles from './FlowView.module.css'

const nodeTypes = { flow: FlowNodeView }

/** What each kind of node is called where the drawer names it. */
const KINDS: Readonly<Record<FlowKind, string>> = {
  request: '用户请求', step: 'AI 步骤', call: '函数调用', reply: '回复', end: '结束',
}

const STATE_TEXT: Readonly<Record<string, string>> = {
  pending: '等待开始', running: '进行中', done: '完成', failed: '失败', interrupted: '已中断',
}

/** The title a program event is known by, in the drawer. */
const PROGRAM_LABELS: Readonly<Record<string, string>> = {
  started: '运行程序', invalid: '程序无效', failed: '程序失败', question: '程序提问', paused: '程序已暂停',
  result_invalid: '返回值无效', module_loaded: '加载模块', module_fix: '修复模块', visual_completed: '视觉核对',
}

function pretty(value: unknown): string {
  if (typeof value === 'string') {
    try { return JSON.stringify(JSON.parse(value), null, 2) } catch { return value }
  }
  return JSON.stringify(value, null, 2) ?? ''
}

/** The state word a card and the drawer both print for one call. */
function stateOf(status: FlowStatus | string): string {
  return STATE_TEXT[status] ?? String(status)
}

/**
 * The words of a message without the markup a card cannot draw: a reply that
 * opens a section reads as its own sentence instead of `### 已完成`, and a link
 * keeps its label. Cards and headers preview with this; the drawer draws the
 * document itself.
 */
function preview(markdown: string): string {
  return extractMarkdownPlainText(markdown, { mode: 'first-paragraph' }).replace(/\s+/gu, ' ').trim()
}

/**
 * The turn as a flow: every step the AI took with the work it did folded into
 * the same card, a program's calls as branches off it, and the ending. Built
 * from the conversation's own events, so it is there for every turn the
 * transcript can show.
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
      // A card draws words, not markup: show the message, never its source.
      data: { node: { ...node, title: preview(node.title), summary: preview(node.summary) } },
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
          <TurnPicker
            turns={turns}
            shown={shown}
            onPick={turn => { setChosen(turn); setSelectedId(null) }}
          />
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
        {selected !== undefined && (
          <ConversationTaskProvider taskId={taskId}>
            <NodeDetail node={selected} onClose={() => setSelectedId(null)} />
          </ConversationTaskProvider>
        )}
      </div>
    </div>
  )
}

/**
 * Which turn is on show. A list rather than a bare select: every turn is one
 * row with the state it ended in, how much work it took and how long it ran,
 * and the row that is drawn now is the row that reads as chosen.
 */
function TurnPicker({ turns, shown, onPick }: {
  turns: readonly FlowTurn[]
  shown: FlowTurn | undefined
  onPick: (turn: number) => void
}) {
  const [open, setOpen] = useState(false)
  const here = Math.max(0, turns.findIndex(turn => turn.turn === shown?.turn))
  const [active, setActive] = useState(here)
  const box = useRef<HTMLDivElement | null>(null)

  // A list that stays open while the pointer is away is a list in the way.
  useEffect(() => {
    if (!open) return
    const away = (event: MouseEvent) => {
      if (box.current !== null && event.target instanceof Element && !box.current.contains(event.target)) setOpen(false)
    }
    const key = (event: KeyboardEvent) => { if (event.key === 'Escape') setOpen(false) }
    window.addEventListener('mousedown', away)
    window.addEventListener('keydown', key)
    return () => {
      window.removeEventListener('mousedown', away)
      window.removeEventListener('keydown', key)
    }
  }, [open])

  const openAt = (index: number): void => { setActive(index); setOpen(true) }
  const onKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>): void => {
    if (event.key === 'Escape') { setOpen(false); return }
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      if (!open) { openAt(here); return }
      const step = event.key === 'ArrowDown' ? 1 : -1
      setActive(current => Math.min(turns.length - 1, Math.max(0, current + step)))
      return
    }
    if ((event.key === 'Enter' || event.key === ' ') && open) {
      event.preventDefault()
      const turn = turns[active]
      if (turn === undefined) return
      onPick(turn.turn)
      setOpen(false)
    }
  }

  return (
    <div className={styles.picker} ref={box}>
      <button
        type="button"
        className={styles.pickerTrigger}
        data-open={open || undefined}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="选择要查看的轮次"
        onClick={() => { if (open) setOpen(false); else openAt(here) }}
        onKeyDown={onKeyDown}
      >
        <span className={styles.pickerTurn}>{shown === undefined ? '轮次' : '第 ' + String(shown.turn) + ' 轮'}</span>
        {shown !== undefined && <span className={styles.pickerTitle}>{shown.title}</span>}
        <span className={styles.pickerCaret} aria-hidden="true">▾</span>
      </button>
      {open && (
        <div className={styles.pickerMenu} role="listbox" aria-label="轮次">
          {turns.map((turn, index) => (
            <button
              key={turn.turn}
              type="button"
              role="option"
              aria-selected={turn.turn === shown?.turn}
              className={styles.pickerRow}
              data-active={index === active || undefined}
              data-current={turn.turn === shown?.turn || undefined}
              onMouseEnter={() => setActive(index)}
              onClick={() => { onPick(turn.turn); setOpen(false) }}
            >
              <span className={styles.pickerDot} data-status={turn.status} />
              <span className={styles.pickerRowMain}>
                <span className={styles.pickerRowTitle}>
                  <span className={styles.pickerRowTurn}>第 {turn.turn} 轮</span>
                  {turn.title}
                </span>
                <span className={styles.pickerRowMeta}>
                  {[
                    STATE_TEXT[turn.status] ?? turn.status,
                    String(turn.steps) + ' 步',
                    turn.tools === 0 ? '' : String(turn.tools) + ' 次工具',
                    turn.durationMs === undefined ? '' : formatSpan(turn.durationMs),
                  ].filter(Boolean).join(' · ')}
                </span>
              </span>
              <span className={styles.pickerCheck} aria-hidden="true">{turn.turn === shown?.turn ? '✓' : ''}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

/** One node in full: what it was asked, what it did, what it returned. */
function NodeDetail({ node, onClose }: { node: FlowGraphNode; onClose: () => void }) {
  const lead = node.items.find(item => item.kind === 'assistant')
  const asked = node.items.find(item => item.kind === 'user')
  // The request and the answer are drawn on their own; everything else is work.
  const work = node.items.filter(item => item.kind !== 'assistant' && item.kind !== 'user')
  // A step that did one thing shows it open: hiding a single row helps nobody.
  const alone = work.length <= 1
  const head = preview(node.title)
  return (
    <aside className={styles.drawer} aria-label="节点详情">
      <header className={styles.drawerHeader}>
        <div className={styles.headText}>
          <span className={styles.headKind} data-kind={node.kind}>{KINDS[node.kind]}</span>
          <strong className={styles.headTitle} title={head}>{head}</strong>
        </div>
        <button type="button" className={styles.close} onClick={onClose} title="关闭（Esc）" aria-label="关闭">✕</button>
      </header>
      <div className={styles.facts}>
        <span className={styles.state} data-status={node.status}>{stateOf(node.status)}</span>
        {node.meta !== '' && <span className={styles.fact}>{node.meta}</span>}
        {node.work.tools > 0 && <span className={styles.fact}>{node.work.tools} 次工具</span>}
        {node.work.programs > 0 && <span className={styles.fact}>{node.work.programs} 次程序</span>}
        {node.work.failed > 0 && <span className={styles.factBad}>{node.work.failed} 次失败</span>}
      </div>
      {/* The body is keyed by the node: opening another node starts at its top. */}
      <div className={styles.drawerBody} key={node.id}>
        {asked !== undefined && (
          <section className={styles.group}>
            <h4 className={styles.groupTitle}>
              提出的要求
              {node.summary !== '' && <span className={styles.groupNote}>{preview(node.summary)}</span>}
            </h4>
            <p className={styles.lead}>{asked.text.trim() === '' ? '（这条消息只有附件）' : asked.text}</p>
          </section>
        )}
        {/* A step's own words say what it did: a preview above them would only repeat it. */}
        {lead === undefined && asked === undefined && node.summary !== '' && <p className={styles.lead}>{preview(node.summary)}</p>}
        {lead !== undefined && <AssistantDetail reasoning={lead.reasoning} text={lead.text} />}
        {work.length > 0 && (
          <section className={styles.group}>
            <h4 className={styles.groupTitle}>
              这一步做的事
              {node.work.label !== '' && <span className={styles.groupNote}>{node.work.label}</span>}
            </h4>
            {work.map((item, index) => <WorkCard key={'item-' + String(index)} item={item} open={alone} />)}
          </section>
        )}
        {lead === undefined && asked === undefined && work.length === 0 && <p className={styles.meta}>这个节点没有留下记录。</p>}
      </div>
    </aside>
  )
}

/** One thing the step did, as a row that opens into its arguments and its result. */
function WorkCard({ item, open }: { item: ChatItem; open: boolean }) {
  if (item.kind === 'tool') return <ToolCard item={item} open={open || item.result?.isError === true} />
  if (item.kind === 'program') return <ProgramCard item={item} open={open} />
  if (item.kind === 'subagent') return <SubagentCard item={item} open={open} />
  if (item.kind === 'call') return <CallCard item={item} open={open} />
  if (item.kind === 'interjection') return <NoteCard item={item} />
  if (item.kind === 'turn-end') return <EndDetail reason={item.reason} durationMs={item.durationMs} />
  return null
}

function CardHead({ tone, name, note, state }: { tone?: string | undefined; name: string; note: string; state?: string | undefined }) {
  return (
    <summary className={styles.cardSum}>
      {tone !== undefined && <span className={styles.dot} data-tone={tone} />}
      <span className={styles.cardName}>{name}</span>
      {note !== '' && <span className={styles.cardNote} title={note}>{note}</span>}
      {state !== undefined && <span className={styles.cardState} data-status={state}>{stateOf(state)}</span>}
    </summary>
  )
}

function ToolCard({ item, open }: { item: Extract<ChatItem, { kind: 'tool' }>; open: boolean }) {
  // The transcript's own row, not a copy of it: a file change opens as the
  // patch it was, a command as its terminal, a read as the lines it read.
  return (
    <div className={styles.toolRow}>
      <ToolRow
        name={item.name}
        rawArguments={item.arguments}
        result={item.result}
        stopped={false}
        onOpenChild={() => {}}
        startOpen={open}
      />
    </div>
  )
}

function ProgramCard({ item, open }: { item: Extract<ChatItem, { kind: 'program' }>; open: boolean }) {
  const failed = item.event === 'failed' || item.event === 'invalid' || item.event === 'result_invalid'
  return (
    <details className={styles.card} open={open} data-state={failed ? 'failed' : 'done'}>
      <CardHead
        tone="run"
        name={PROGRAM_LABELS[item.event] ?? '程序'}
        note={item.text.replace(/\s+/g, ' ').trim()}
        state={failed ? 'failed' : 'done'}
      />
      <div className={styles.cardBody}>
        {item.source !== undefined && item.source.trim() !== '' && (
          <>
            <h5 className={styles.label}>程序</h5>
            <pre className={styles.code}>{item.source}</pre>
          </>
        )}
        <h5 className={styles.label}>输出</h5>
        <pre className={styles.code}>{item.text === '' ? '（没有输出）' : item.text}</pre>
      </div>
    </details>
  )
}

function SubagentCard({ item, open }: { item: Extract<ChatItem, { kind: 'subagent' }>; open: boolean }) {
  return (
    <details className={styles.card} open={open} data-state={item.status === 'running' ? 'running' : 'done'}>
      <CardHead tone="agent" name={item.name === '' ? '子代理' : item.name} note={item.review} state={item.status === 'running' ? 'running' : 'done'} />
      <div className={styles.cardBody}>
        <h5 className={styles.label}>报告</h5>
        <p className={styles.lead}>{item.report === '' ? '（还没有报告）' : item.report}</p>
      </div>
    </details>
  )
}

function NoteCard({ item }: { item: Extract<ChatItem, { kind: 'interjection' }> }) {
  return (
    <details className={styles.card} data-state="done">
      <CardHead tone="context" name="插话" note={item.text.replace(/\s+/g, ' ').trim()} state="done" />
      <div className={styles.cardBody}>
        <h5 className={styles.label}>{item.delivered ? '已送达' : '待送达'}</h5>
        <p className={styles.lead}>{item.text}</p>
      </div>
    </details>
  )
}

function CallCard({ item, open }: { item: Extract<ChatItem, { kind: 'call' }>; open: boolean }) {
  const state = item.error !== undefined ? 'failed' : ['pending', 'running', 'suspended', 'paused'].includes(item.status) ? 'running' : 'done'
  return (
    <details className={styles.card} open={open} data-state={state}>
      <CardHead tone="task" name={item.name.endsWith(')') ? item.name : item.name + '()'} note={item.description} state={state} />
      <div className={styles.cardBody}>
        <h5 className={styles.label}>契约</h5>
        <p className={styles.lead}>{item.description === '' ? '（没有契约注释）' : item.description}</p>
        {item.result !== undefined && item.result !== '' && (
          <>
            <h5 className={styles.label}>返回</h5>
            <pre className={styles.code}>{pretty(item.result)}</pre>
          </>
        )}
        {item.error !== undefined && (
          <>
            <h5 className={styles.label}>错误</h5>
            <p className={styles.errorText}>{item.error}</p>
          </>
        )}
      </div>
    </details>
  )
}

/**
 * The model's own words. The reasoning reads as a quiet aside and the answer
 * as the document it is: both are Markdown, and both are drawn — a reader who
 * opens one step should never be shown `###` or `**` as literal characters.
 */
function AssistantDetail({ reasoning, text }: { reasoning: string; text: string }) {
  const labels = useMemo(() => markdownLabels(t), [])
  const thought = reasoning.trim()
  const said = text.trim()
  if (thought === '' && said === '') return <p className={styles.meta}>这一步没有留下文字。</p>
  return (
    <>
      {thought !== '' && (
        <section className={styles.thoughtPanel}>
          <h4 className={styles.groupTitle}>思考</h4>
          <div className={styles.thought}>
            <MarkdownText text={thought} labels={labels} variant="compact" />
          </div>
        </section>
      )}
      {said !== '' && (
        <section className={styles.group}>
          <h4 className={styles.groupTitle}>写下的话</h4>
          <div className={styles.reply}>
            <MarkdownText text={said} labels={labels} variant="compact" />
          </div>
        </section>
      )}
    </>
  )
}

function EndDetail({ reason, durationMs }: { reason: TurnEndReason; durationMs: number | undefined }) {
  const [title, note] = reason.kind === 'completed' ? ['目标完成', '这一轮按计划收尾。']
    : reason.kind === 'aborted' ? ['你取消了这一轮', '已完成的步骤仍然留在图上。']
    : reason.kind === 'interrupted' ? ['这一轮被中断', '流程停在最后一个完成的节点。']
    : ['这一轮出错', reason.message ?? '']
  return (
    <section className={styles.group}>
      <h4 className={styles.groupTitle}>结局<span className={styles.groupNote}>{title}</span></h4>
      {note !== '' && <p className={styles.lead}>{note}</p>}
      {durationMs !== undefined && <p className={styles.meta}>耗时 {formatSpan(durationMs)}</p>}
    </section>
  )
}
