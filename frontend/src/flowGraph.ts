/**
 * The picture one turn makes: a node per step, the tools a step called drawn
 * under it, and the calls a program made drawn under the step that ran it.
 *
 * The graph is built from the same chat items the transcript renders, so every
 * conversation has one. The flow tab used to read the agent's frames table,
 * which the runtime stopped writing, so the tab stood empty.
 *
 * Kept free of runtime imports so it can be unit tested on its own.
 */
import type { AssistantItem, ChatItem } from './model.ts'

export type FlowStatus = 'pending' | 'running' | 'done' | 'failed' | 'interrupted'

/** What a node stands for: the request, one step, its calls, and how the turn ended. */
export type FlowKind = 'request' | 'step' | 'tool' | 'call' | 'program' | 'delegate' | 'note' | 'reply' | 'end'

export interface FlowNode {
  readonly id: string
  readonly parent: string | null
  readonly kind: FlowKind
  readonly title: string
  readonly summary: string
  readonly meta: string
  readonly status: FlowStatus
  readonly turn: number
  /** The chat items behind the node: one, or several when a run of tools is one node. */
  readonly items: readonly ChatItem[]
}

export interface FlowEdge {
  readonly id: string
  readonly source: string
  readonly target: string
  /** A call the step above made, rather than the next step of the turn. */
  readonly branch: boolean
  readonly status: FlowStatus
}

export interface FlowTurn {
  readonly turn: number
  readonly title: string
  readonly status: FlowStatus
  readonly steps: number
  readonly tools: number
  readonly durationMs: number | undefined
}

export interface FlowGraph {
  readonly nodes: readonly FlowNode[]
  readonly edges: readonly FlowEdge[]
}

export interface FlowPlacement {
  readonly x: number
  readonly y: number
  readonly width: number
  readonly height: number
}

export const FLOW_WIDTH = 300
/** The gap between two nodes of the same column. */
export const FLOW_GAP = 20
/** How far a branch sits to the right of the step that opened it. */
export const FLOW_INDENT = 34
/** Deeper chains stop indenting, so a long call chain stays one narrow column. */
const DEEPEST_INDENT = 4

/**
 * The height each kind of node draws at. The node draws itself at exactly this
 * height, so the room the layout leaves is the room the node takes: an edge
 * starts at the bottom of a box and ends on the top of the next one.
 */
export const FLOW_HEIGHT: Readonly<Record<FlowKind, number>> = {
  request: 88, reply: 88, step: 88, call: 88, tool: 78, program: 80, delegate: 88, note: 68, end: 62,
}

const heads = (value: string, max = 68): string => {
  const line = value.replace(/\s+/g, ' ').trim()
  return line.length > max ? line.slice(0, max - 1) + '…' : line
}

/** The latest thought of a reasoning summary, as one line. */
export function thoughtHeadline(reasoning: string): string {
  const paragraphs = reasoning.replace(/\*\*\*\*/g, '**\n\n**').split(/\r?\n[\t ]*\r?\n/).map(part => part.trim()).filter(part => part !== '')
  const latest = paragraphs.findLast(part => part.startsWith('**')) ?? paragraphs.at(-1) ?? ''
  return heads((latest.split(/\r?\n/)[0] ?? '').replaceAll('**', '').trim(), 90)
}

const ENDINGS: Readonly<Record<string, string>> = { completed: '完成', aborted: '已取消', interrupted: '已中断', error: '失败' }

function endStatus(reason: { readonly kind: string } | undefined): FlowStatus {
  if (reason === undefined) return 'running'
  return reason.kind === 'completed' ? 'done' : reason.kind === 'error' ? 'failed' : 'interrupted'
}

/** Where a tool call stands: it waits for its result, and a failed one keeps its failure. */
function toolStatus(item: ChatItem): FlowStatus {
  if (item.kind !== 'tool') return 'done'
  if (item.result === undefined) return 'running'
  return item.result.isError ? 'failed' : 'done'
}

const RUNNING_CALL = new Set(['pending', 'running', 'suspended', 'paused'])

function callStatus(item: ChatItem): FlowStatus {
  if (item.kind !== 'call') return 'done'
  if (item.error !== undefined) return 'failed'
  if (RUNNING_CALL.has(item.status)) return 'running'
  return item.status === 'done' ? 'done' : 'interrupted'
}

const PROGRAM_TITLES: Readonly<Record<string, string>> = {
  started: '运行程序', invalid: '程序无效', failed: '程序失败', question: '程序提问', paused: '程序已暂停',
  result_invalid: '返回值无效', module_loaded: '加载模块', module_fix: '修复模块', visual_completed: '视觉核对',
}

const FAILED_PROGRAMS = new Set(['failed', 'invalid', 'result_invalid'])

/** The items of each turn, in the order the turn ran them. */
function turnGroups(items: readonly ChatItem[]): { turn: number; items: ChatItem[] }[] {
  const groups: { turn: number; items: ChatItem[] }[] = []
  let current = 1
  let open = false
  for (const item of items) {
    if (item.kind === 'turn') {
      current = item.turn
      open = false
      continue
    }
    if (!open) {
      groups.push({ turn: current, items: [] })
      open = true
    }
    const group = groups.at(-1)
    if (group === undefined) continue
    group.items.push(item)
  }
  return groups
}

/** One entry per turn: what was asked, how it went, and how much it did. */
export function flowTurns(items: readonly ChatItem[]): FlowTurn[] {
  return turnGroups(items).map(group => {
    const asked = group.items.find(item => item.kind === 'user')
    const ended = group.items.findLast(item => item.kind === 'turn-end')
    return {
      turn: group.turn,
      title: heads(asked?.kind === 'user' ? asked.text : '', 46) || '第 ' + String(group.turn) + ' 轮',
      status: endStatus(ended?.kind === 'turn-end' ? ended.reason : undefined),
      steps: group.items.filter(item => item.kind === 'assistant').length,
      tools: group.items.filter(item => item.kind === 'tool').length,
      durationMs: ended?.kind === 'turn-end' ? ended.durationMs : undefined,
    }
  })
}

/** A duration a reader can compare at a glance. */
export function formatSpan(ms: number): string {
  if (ms < 1000) return String(Math.max(1, Math.round(ms))) + ' ms'
  const seconds = ms / 1000
  if (seconds < 60) return String(Number(seconds.toFixed(1))) + ' s'
  const minutes = Math.floor(seconds / 60)
  return String(minutes) + ' 分 ' + String(Math.round(seconds - minutes * 60)) + ' s'
}

/**
 * One turn as a flow: the request, every step in order, the tools each step
 * called below it, and a program's calls nested under the step that ran it.
 */
export function buildFlow(items: readonly ChatItem[], turn: number): FlowGraph {
  const group = turnGroups(items).find(entry => entry.turn === turn)
  const nodes: FlowNode[] = []
  const edges: FlowEdge[] = []
  if (group === undefined) return { nodes, edges }
  const answered = group.items.findLast(item => item.kind === 'turn-end')
  const asked = group.items.find(item => item.kind === 'user')
  let counter = 0
  const nextId = (): string => String(turn) + ':' + String(counter++)

  const link = (source: FlowNode, target: FlowNode): void => {
    // The turn's own chain hangs off the request; everything else is a branch.
    edges.push({ id: source.id + '->' + target.id, source: source.id, target: target.id, branch: target.parent !== requestId, status: target.status })
  }

  const add = (node: FlowNode): FlowNode => { nodes.push(node); return node }

  const request = add({
    id: nextId(), parent: null, kind: 'request', turn,
    title: heads(asked?.kind === 'user' ? asked.text : '', 60) || '第 ' + String(turn) + ' 轮请求',
    summary: asked?.kind === 'user' && (asked.attachments?.length ?? 0) > 0 ? String(asked.attachments?.length) + ' 个附件' : '',
    meta: '第 ' + String(turn) + ' 轮',
    status: endStatus(answered?.kind === 'turn-end' ? answered.reason : undefined),
    items: asked === undefined ? [] : [asked],
  })
  const requestId = request.id
  /** The last node on the turn's own chain: the ending follows it. */
  let spine: FlowNode = request

  /** One branch's items: its own chain, and the tools each step called below it. */
  const walk = (branch: readonly ChatItem[], parent: FlowNode): void => {
    let previous: FlowNode | undefined
    let owner: { node: FlowNode; item: AssistantItem } | undefined
    let run: { at: number; items: ChatItem[] } | undefined
    const place = (node: FlowNode): void => {
      link(previous ?? parent, node)
      previous = node
      if (node.parent === requestId) spine = node
    }
    for (const item of branch) {
      if (item.kind === 'user' || item.kind === 'turn-end' || item.kind === 'observer' || item.kind === 'retrospective') continue
      if (item.kind === 'tool') {
        // A tool the step's own AI called sits under that step; a program's call
        // carries a line number and belongs to the call it was made in.
        const owned = owner !== undefined && item.line === undefined && item.frame === owner.item.frame
          && (item.step === undefined || item.step === owner.item.step)
        if (owned && owner !== undefined) {
          const node = add({
            id: nextId(), parent: owner.node.id, kind: 'tool', turn,
            title: item.name, summary: '', meta: '', status: toolStatus(item), items: [item],
          })
          // Siblings stack in one column under the step: the second call follows
          // the first rather than crossing back over it to the step above.
          link(previous ?? owner.node, node)
          previous = node
          continue
        }
        owner = undefined
        if (run !== undefined) {
          run.items.push(item)
          const grouped = nodes[run.at]
          if (grouped !== undefined) {
            nodes[run.at] = {
              ...grouped,
              items: [...run.items],
              title: '工具调用 ×' + String(run.items.length),
              status: run.items.some(one => toolStatus(one) === 'running') ? 'running' : run.items.some(one => toolStatus(one) === 'failed') ? 'failed' : 'done',
            }
          }
          continue
        }
        const node = add({
          id: nextId(), parent: parent.id, kind: 'tool', turn,
          title: item.name, summary: '', meta: '', status: toolStatus(item), items: [item],
        })
        run = { at: nodes.length - 1, items: [item] }
        place(node)
        continue
      }
      run = undefined
      if (item.kind === 'assistant') {
        const thought = item.reasoning.trim() === '' ? '' : thoughtHeadline(item.reasoning)
        const node = add({
          id: nextId(), parent: parent.id, kind: 'step', turn,
          title: thought || heads(item.text, 60) || '第 ' + String(item.step + 1) + ' 步',
          summary: thought === '' ? '' : heads(item.text, 60),
          meta: '第 ' + String(item.step + 1) + ' 步',
          status: item.streaming ? 'running' : 'done',
          items: [item],
        })
        owner = { node, item }
        place(node)
        continue
      }
      owner = undefined
      if (item.kind === 'call') {
        const node = add({
          id: nextId(), parent: parent.id, kind: 'call', turn,
          // The runtime names a program's call with its brackets already: don't double them.
          title: item.name.endsWith(')') ? item.name : item.name + '()',
          summary: heads(item.description, 60),
          meta: [item.module ? '模块' : '虚函数', item.line === undefined ? '' : '第 ' + String(item.line) + ' 行'].filter(Boolean).join(' · '),
          status: callStatus(item),
          items: [item],
        })
        place(node)
        walk(item.steps, node)
        continue
      }
      if (item.kind === 'program') {
        const node = add({
          id: nextId(), parent: parent.id, kind: 'program', turn,
          title: PROGRAM_TITLES[item.event] ?? '程序',
          summary: heads(item.text, 70),
          meta: '',
          status: FAILED_PROGRAMS.has(item.event) ? 'failed' : item.event === 'paused' ? 'interrupted' : 'done',
          items: [item],
        })
        place(node)
        continue
      }
      if (item.kind === 'subagent') {
        const node = add({
          id: nextId(), parent: parent.id, kind: 'delegate', turn,
          title: item.name,
          summary: heads(item.report, 60),
          meta: '子代理',
          status: item.status === 'running' ? 'running' : item.status === 'done' ? 'done' : 'failed',
          items: [item],
        })
        place(node)
        continue
      }
      if (item.kind === 'interjection') {
        const node = add({
          id: nextId(), parent: parent.id, kind: 'note', turn,
          title: heads(item.text, 60) || '插话',
          summary: '', meta: item.delivered ? '已送达' : '待送达', status: 'done', items: [item],
        })
        place(node)
      }
    }
  }

  walk(group.items, request)

  // The closing reply is the turn's answer, not one more step of the work: the
  // last step holding it is relabelled rather than drawn a second time.
  const reply = group.items.findLast(item => item.kind === 'assistant' && item.program === undefined && item.text.trim() !== '')
  if (reply !== undefined) {
    const at = nodes.findLastIndex(node => node.kind === 'step' && node.items[0] === reply)
    const node = at < 0 ? undefined : nodes[at]
    if (node !== undefined) {
      nodes[at] = { ...node, kind: 'reply', title: '回复', summary: heads(reply.kind === 'assistant' ? reply.text : '', 74), meta: '' }
      if (spine === node) spine = nodes[at]!
    }
  }
  if (answered?.kind === 'turn-end') {
    const node = add({
      id: nextId(), parent: requestId, kind: 'end', turn,
      title: ENDINGS[answered.reason.kind] ?? answered.reason.kind,
      summary: answered.reason.kind === 'error' ? heads(answered.reason.message, 74) : '',
      meta: answered.durationMs === undefined ? '' : formatSpan(answered.durationMs),
      status: endStatus(answered.reason), items: [answered],
    })
    link(spine, node)
  }
  return { nodes, edges }
}

/** Where the graph draws each node: one column, branches indented under their step. */
export function layoutFlow(nodes: readonly FlowNode[]): Map<string, FlowPlacement> {
  const byId = new Map(nodes.map(node => [node.id, node]))
  const depth = new Map<string, number>()
  const depthOf = (node: FlowNode): number => {
    const known = depth.get(node.id)
    if (known !== undefined) return known
    let value = 0
    const seen = new Set<string>([node.id])
    let cursor = node.parent === null ? undefined : byId.get(node.parent)
    while (cursor !== undefined && !seen.has(cursor.id)) {
      seen.add(cursor.id)
      value += 1
      cursor = cursor.parent === null ? undefined : byId.get(cursor.parent)
    }
    depth.set(node.id, value)
    return value
  }
  const placed = new Map<string, FlowPlacement>()
  let y = 0
  for (const node of nodes) {
    const height = FLOW_HEIGHT[node.kind]
    // The request is the spine: everything directly under it sits at the left edge.
    const indent = node.parent === null ? 0 : Math.max(0, depthOf(node) - 1)
    placed.set(node.id, { x: Math.min(indent, DEEPEST_INDENT) * FLOW_INDENT, y, width: FLOW_WIDTH, height })
    y += height + FLOW_GAP
  }
  return placed
}
