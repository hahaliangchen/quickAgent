/**
 * The picture one turn makes: the request, one card per step, a program's own
 * calls drawn as their own branch, the closing reply and the ending.
 *
 * A step is one card, not four. The tools it called, the program runs it
 * started and the subagents it spawned fold into the card of the step that
 * started them, so a long turn reads as the steps it took instead of as every
 * leaf it touched. Nothing is lost: every folded call is one click away in the
 * drawer, which shows the arguments, the result and the failure of each.
 *
 * The graph is built from the same chat items the transcript renders, so every
 * conversation has one. The flow tab used to read the agent's frames table,
 * which the runtime stopped writing, so the tab stood empty.
 *
 * Kept free of runtime imports so it can be unit tested on its own.
 */
import type { AssistantItem, ChatItem } from './model.ts'

export type FlowStatus = 'pending' | 'running' | 'done' | 'failed' | 'interrupted'

/** What a card stands for: the request, one step of the AI, a program's call, the reply, the ending. */
export type FlowKind = 'request' | 'step' | 'call' | 'reply' | 'end'

/** The family a call belongs to: a step's card colours what it did with it. */
export type FlowTone = 'read' | 'edit' | 'run' | 'search' | 'task' | 'agent' | 'context' | 'web' | 'neutral'

/** What a step did besides thinking, counted once so the card and the drawer agree. */
export interface FlowWork {
  /** How many tool calls the step made. */
  readonly tools: number
  /** How many program events it produced. */
  readonly programs: number
  /** How many of those failed, so the card can say so without being opened. */
  readonly failed: number
  /** The work by family, in the order it happened. */
  readonly groups: readonly { readonly tone: FlowTone; readonly count: number }[]
  /** What the step was pointed at, deduplicated, the first few: what a card names. */
  readonly names: readonly { readonly name: string; readonly tone: FlowTone }[]
  /** One line for the card: "修改 ×2 · 运行". */
  readonly label: string
}

const NO_WORK: FlowWork = { tools: 0, programs: 0, failed: 0, groups: [], names: [], label: '' }

export interface FlowNode {
  readonly id: string
  readonly parent: string | null
  readonly kind: FlowKind
  readonly title: string
  readonly summary: string
  readonly meta: string
  readonly status: FlowStatus
  readonly turn: number
  /** The chat items behind the node: its step, and everything that step did. */
  readonly items: readonly ChatItem[]
  /** What the step did besides thinking; empty for a request, a reply a call or the ending. */
  readonly work: FlowWork
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

export const FLOW_WIDTH = 340
/** The gap between two nodes of the same column. */
export const FLOW_GAP = 18
/** How far a branch sits to the right of the step that opened it. */
export const FLOW_INDENT = 36
/** Deeper chains stop indenting, so a long call chain stays one narrow column. */
const DEEPEST_INDENT = 4

/**
 * The height each kind of node draws at. The node draws itself at exactly this
 * height, so the room the layout leaves is the room the node takes: an edge
 * starts at the bottom of a box and ends on the top of the next one.
 */
export const FLOW_HEIGHT: Readonly<Record<FlowKind, number>> = {
  request: 108, step: 104, call: 96, reply: 104, end: 64,
}

/**
 * The words of a message with the markup a card cannot draw removed: heading
 * and list markers, emphasis, code ticks, and link destinations. A card has a
 * line of room, so without this `### 已完成` would be drawn as its own source.
 */
function plain(value: string): string {
  const lines = value.split(/\r?\n/u).map(line => line.trim()).filter(line => line !== '')
  // A sentence rather than a section title, a quote or a table row: a bullet
  // keeps its text, because the bullet is often the point of the message.
  const body = lines.find(line => !/^(?:#{1,6}\s|>|\|)/u.test(line)) ?? lines[0] ?? ''
  return body
    .replace(/^#{1,6}\s+/u, '')
    .replace(/^\s*(?:[-*+]|\d+[.)])\s+/u, '')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/gu, '$1')
    .replace(/\*\*(.+?)\*\*/gu, '$1')
    .replace(/__(.+?)__/gu, '$1')
    .replace(/~~(.+?)~~/gu, '$1')
    .replace(/`([^`]+)`/gu, '$1')
}

const heads = (value: string, max = 68): string => {
  const line = plain(value).replace(/\s+/g, ' ').trim()
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

/** Where anything a step did stands, so one bundle has one answer. */
function workStatus(item: ChatItem): FlowStatus {
  if (item.kind === 'tool') return toolStatus(item)
  if (item.kind === 'call') return callStatus(item)
  if (item.kind === 'program') return FAILED_PROGRAMS.has(item.event) ? 'failed' : item.event === 'paused' ? 'interrupted' : 'done'
  if (item.kind === 'subagent') return item.status === 'running' ? 'running' : item.status === 'done' ? 'done' : 'failed'
  return 'done'
}

/**
 * The family a tool belongs to. Longest-standing conventions sit on top: a
 * command is a run even when it is called something that reads, and an edit is
 * an edit even when the tool also writes down where it went.
 */
const TONES: readonly { readonly tone: FlowTone; readonly match: RegExp }[] = [
  { tone: 'run', match: /^(exec_command|write_stdin|run_program|run_project_script|install_dependencies|run_readonly_command|process_)/ },
  { tone: 'edit', match: /write|edit|replace|apply|patch|create|rename|mkdir|delete/ },
  { tone: 'search', match: /search|grep|glob|find|list_/ },
  { tone: 'agent', match: /agent|delegate|handoff/ },
  { tone: 'task', match: /task|plan|step/ },
  { tone: 'context', match: /skill|context|memory|remember|history|asset|compact/ },
  { tone: 'web', match: /web|http|browser|fetch|url/ },
  { tone: 'read', match: /read|cat|open|attachment|inspect/ },
]

const TONE_WORDS: Readonly<Record<FlowTone, string>> = {
  read: '读取', edit: '修改', run: '运行', search: '搜索', task: '任务', agent: '子代理', context: '上下文', web: '网络', neutral: '工具',
}

/** What family a call belongs to, by its name. */
export function toneOf(name: string): FlowTone {
  const lower = name.toLowerCase()
  return TONES.find(entry => entry.match.test(lower))?.tone ?? 'neutral'
}

/** What a step did besides thinking, counted from the items it folded in. */
function workOf(items: readonly ChatItem[]): FlowWork {
  const counts = new Map<FlowTone, number>()
  const names: { name: string; tone: FlowTone }[] = []
  let tools = 0
  let programs = 0
  let failed = 0
  const note = (tone: FlowTone, name: string): void => {
    counts.set(tone, (counts.get(tone) ?? 0) + 1)
    if (name !== '' && names.length < 8 && !names.some(one => one.name === name)) names.push({ name, tone })
  }
  for (const item of items) {
    if (item.kind === 'tool') {
      tools += 1
      note(toneOf(item.name), item.name)
    } else if (item.kind === 'program') {
      programs += 1
      note('run', PROGRAM_TITLES[item.event] ?? '程序')
    } else if (item.kind === 'subagent') {
      note('agent', item.name === '' ? '子代理' : item.name)
    } else if (item.kind === 'interjection') {
      note('context', '插话')
    } else continue
    if (workStatus(item) === 'failed') failed += 1
  }
  const groups = [...counts.entries()].map(([tone, count]) => ({ tone, count }))
  return {
    tools, programs, failed, groups, names,
    label: groups.map(group => TONE_WORDS[group.tone] + (group.count > 1 ? ' ×' + String(group.count) : '')).join(' · '),
  }
}

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

/** One step, and everything it did: the unit the graph draws a card for. */
interface Bundle {
  readonly lead: AssistantItem | undefined
  readonly items: ChatItem[]
}

/**
 * Split a branch into one bundle per step. Work that arrives before any step —
 * a program's own call, a tool the runtime ran on its own — is a bundle too, so
 * it still gets one card instead of a card per call.
 */
function bundles(list: readonly ChatItem[]): Bundle[] {
  const out: Bundle[] = []
  for (const item of list) {
    if (item.kind === 'user' || item.kind === 'turn' || item.kind === 'turn-end' || item.kind === 'observer' || item.kind === 'retrospective') continue
    if (item.kind === 'assistant') {
      out.push({ lead: item, items: [item] })
      continue
    }
    const open = out.at(-1)
    if (open === undefined || open.lead === undefined && open.items.length === 0) out.push({ lead: undefined, items: [item] })
    else open.items.push(item)
  }
  return out
}

/** Where a whole step stands: one running call is enough to keep it running. */
function bundleStatus(bundle: Bundle): FlowStatus {
  if (bundle.lead?.streaming === true) return 'running'
  const work = bundle.items.filter(item => item.kind !== 'assistant')
  const states = work.map(workStatus)
  if (states.includes('failed')) return 'failed'
  if (states.includes('running')) return 'running'
  if (states.includes('interrupted')) return 'interrupted'
  return work.length === 0 && bundle.lead === undefined ? 'pending' : 'done'
}

/** What a card calls a bundle nobody's step claims. */
function titleOfWork(items: readonly ChatItem[]): string {
  if (items.length > 1) return '工具调用 ×' + String(items.length)
  const item = items[0]
  if (item === undefined) return '步骤'
  if (item.kind === 'tool') return item.name
  if (item.kind === 'program') return PROGRAM_TITLES[item.event] ?? '程序'
  if (item.kind === 'subagent') return item.name === '' ? '子代理' : item.name
  if (item.kind === 'interjection') return heads(item.text, 60) || '插话'
  if (item.kind === 'call') return item.name
  return '步骤'
}

/**
 * One turn as a flow: the request, one card per step carrying everything that
 * step did, a program's own calls as branches off it, and the ending.
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

  const add = (node: FlowNode): FlowNode => { nodes.push(node); return node }

  /** An edge leaves one card and lands on the next; anything off the turn's chain is a branch. */
  const link = (source: FlowNode, target: FlowNode): void => {
    edges.push({ id: source.id + '->' + target.id, source: source.id, target: target.id, branch: target.parent !== requestId, status: target.status })
  }

  const request = add({
    id: nextId(), parent: null, kind: 'request', turn,
    title: heads(asked?.kind === 'user' ? asked.text : '', 60) || '第 ' + String(turn) + ' 轮请求',
    summary: asked?.kind === 'user' && (asked.attachments?.length ?? 0) > 0 ? String(asked.attachments?.length) + ' 个附件' : '',
    meta: '第 ' + String(turn) + ' 轮',
    status: endStatus(answered?.kind === 'turn-end' ? answered.reason : undefined),
    items: asked === undefined ? [] : [asked],
    work: NO_WORK,
  })
  const requestId = request.id
  /** The last node on the turn's own chain: the ending follows it. */
  let spine: FlowNode = request

  /**
   * One branch of the graph. Every step becomes a single card; a call the
   * program made keeps a card of its own, since what happened inside it is a
   * flow of its own, drawn below it.
   */
  const walk = (branch: readonly ChatItem[], parent: FlowNode): void => {
    let previous: FlowNode | undefined
    for (const bundle of bundles(branch)) {
      const lead = bundle.lead
      const thought = lead === undefined || lead.reasoning.trim() === '' ? '' : thoughtHeadline(lead.reasoning)
      const node = add({
        id: nextId(), parent: parent.id, kind: 'step', turn,
        title: lead === undefined
          ? titleOfWork(bundle.items)
          : thought || heads(lead.text, 60) || '第 ' + String(lead.step + 1) + ' 步',
        summary: lead === undefined ? '' : thought === '' ? '' : heads(lead.text, 60),
        meta: lead === undefined ? '' : '第 ' + String(lead.step + 1) + ' 步',
        status: bundleStatus(bundle),
        // A call gets a card of its own below: the step does not carry it twice.
        items: bundle.items.filter(item => item.kind !== 'call'),
        work: workOf(bundle.items),
      })
      link(previous ?? parent, node)
      previous = node
      if (node.parent === requestId) spine = node
      for (const item of bundle.items) {
        if (item.kind !== 'call') continue
        // The runtime names a program's call with its brackets already: don't double them.
        const call = add({
          id: nextId(), parent: node.id, kind: 'call', turn,
          title: item.name.endsWith(')') ? item.name : item.name + '()',
          summary: heads(item.description, 60),
          meta: [item.module ? '模块' : '虚函数', item.line === undefined ? '' : '第 ' + String(item.line) + ' 行'].filter(Boolean).join(' · '),
          status: callStatus(item),
          items: [item],
          work: NO_WORK,
        })
        link(node, call)
        walk(item.steps, call)
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
      // The card names the answer rather than itself: its kind already says 回复.
      nodes[at] = { ...node, kind: 'reply', title: heads(reply.kind === 'assistant' ? reply.text : '', 74), summary: '' }
      if (spine === node) spine = nodes[at]!
    }
  }
  if (answered?.kind === 'turn-end') {
    const node = add({
      id: nextId(), parent: requestId, kind: 'end', turn,
      title: ENDINGS[answered.reason.kind] ?? answered.reason.kind,
      summary: answered.reason.kind === 'error' ? heads(answered.reason.message, 74) : '',
      meta: answered.durationMs === undefined ? '' : formatSpan(answered.durationMs),
      status: endStatus(answered.reason), items: [answered], work: NO_WORK,
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
