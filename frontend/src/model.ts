import type { AgentEvent, Task } from './api.ts'

export interface ToolResult {
  readonly text: string
  readonly isError: boolean
  readonly durationMs: number | undefined
  readonly childTaskId: string | undefined
  readonly time: number
}

export type TurnEndReason =
  | { readonly kind: 'completed'; readonly goalAchieved?: boolean | undefined; readonly unresolved?: readonly string[] | undefined }
  | { readonly kind: 'aborted' }
  | { readonly kind: 'error'; readonly message: string; readonly code: string | undefined }

export type ChatItem =
  | { readonly kind: 'subagent'; readonly key: string; readonly agentId: string; readonly name: string; readonly status: string; readonly events: readonly AgentEvent[]; readonly report: string; readonly review: string; readonly time: number }
  | { readonly kind: 'turn'; readonly key: string; readonly turn: number }
  | { readonly kind: 'interjection'; readonly key: string; readonly text: string; readonly time: number; readonly delivered: boolean }
  | { readonly kind: 'user'; readonly key: string; readonly text: string; readonly time: number; readonly attachments?:readonly {name:string;url:string;image:boolean}[] }
  | AssistantItem
  | { readonly kind: 'observer'; readonly key: string; readonly summary: string; readonly suggestions: readonly string[]; readonly time: number }
  | { readonly kind: 'retrospective'; readonly key: string; readonly text: string; readonly time: number }
  | ToolItem
  | CallItem
  | ProgramItem
  | { readonly kind: 'turn-end'; readonly key: string; readonly reason: TurnEndReason; readonly durationMs?: number | undefined; readonly time?: number | undefined }

/** One model reply: its text and the reasoning summary the model shared. */
export interface AssistantItem {
  readonly kind: 'assistant'
  readonly key: string
  readonly text: string
  readonly reasoning: string
  readonly time: number
  readonly streaming: boolean
  readonly turn: number
  readonly step: number
  /** The function frame whose AI wrote it. */
  readonly frame?: string | undefined
  /** What the host did with the ```js program this reply holds. */
  readonly program?: { readonly status: 'started' | 'invalid'; readonly error?: string | undefined; readonly line?: number | undefined } | undefined
}

export interface ToolItem {
  readonly kind: 'tool'
  readonly key: string
  readonly callId: string
  readonly name: string
  readonly arguments: string
  readonly time: number
  readonly result: ToolResult | undefined
  readonly frame?: string | undefined
  /** The model step that asked for it; a call the program made has none of its own. */
  readonly step?: number | undefined
  /** The program line that made the call. */
  readonly line?: number | undefined
}

/** A @virtual or @module call the program made. It runs in a frame of its own. */
export interface CallItem {
  readonly kind: 'call'
  readonly key: string
  readonly callId: string
  /** The frame whose program made the call. */
  readonly frame?: string | undefined
  /** The frame the call opened. */
  readonly child?: string | undefined
  readonly name: string
  /** The function's contract comment. */
  readonly description: string
  readonly module: boolean
  readonly line?: number | undefined
  readonly status: string
  readonly result?: string | undefined
  readonly error?: string | undefined
  readonly time: number
  /** What happened inside the call, in order. */
  readonly steps: readonly ChatItem[]
}

/** What the host recorded about a program run besides its tool calls. */
export interface ProgramItem {
  readonly kind: 'program'
  readonly key: string
  readonly frame?: string | undefined
  readonly event: 'started' | 'invalid' | 'failed' | 'question' | 'paused' | 'result_invalid' | 'module_loaded' | 'module_fix'
  readonly text: string
  readonly line?: number | undefined
  readonly time: number
}

/** A reply whose ```js block the host runs as a program. */
export const PROGRAM_FENCE = /^```js[ \t]*$/m

/** Keep progress and actions in event order; only a final reply with text is a conclusion. */
export function splitTurnItems(items: readonly ChatItem[]): {
  process: readonly ChatItem[]
  conclusion: AssistantItem | undefined
} {
  const last = items.at(-1)
  return last?.kind === 'assistant' && last.text.trim() !== '' && !PROGRAM_FENCE.test(last.text)
    ? { process: items.slice(0, -1), conclusion: last }
    : { process: items, conclusion: undefined }
}

/** A step of the process: one model reply with the tools it called, or calls nobody's reply made. */
export type ProcessEntry =
  | { readonly kind: 'step'; readonly key: string; readonly reply: AssistantItem; readonly tools: readonly ToolItem[] }
  | { readonly kind: 'tools'; readonly key: string; readonly tools: readonly ToolItem[] }
  | { readonly kind: 'item'; readonly key: string; readonly item: ChatItem }

export function processEntries(items: readonly ChatItem[]): ProcessEntry[] {
  const entries: ProcessEntry[] = []
  let step: { kind: 'step'; key: string; reply: AssistantItem; tools: ToolItem[] } | undefined
  for (const item of items) {
    if (item.kind === 'assistant') {
      step = { kind: 'step', key: item.key, reply: item, tools: [] }
      entries.push(step)
    } else if (item.kind === 'tool') {
      if (step !== undefined && item.line === undefined && item.frame === step.reply.frame && item.step === step.reply.step) {
        step.tools.push(item)
        continue
      }
      step = undefined
      const last = entries.at(-1)
      if (last?.kind === 'tools') (last.tools as ToolItem[]).push(item)
      else entries.push({ kind: 'tools', key: item.key, tools: [item] })
    } else {
      step = undefined
      entries.push({ kind: 'item', key: item.key, item })
    }
  }
  return entries
}

/** Summary parts that a transport delivers back to back, "**A****B**", are paragraphs. */
export function summaryParagraphs(reasoning: string): string {
  return reasoning.replace(/\*\*\*\*/g, '**\n\n**')
}

/** The latest thought of a reasoning summary, as one line. */
export function reasoningHeadline(reasoning: string): string {
  const paragraphs = summaryParagraphs(reasoning).split(/\r?\n[\t ]*\r?\n/).map(part => part.trim()).filter(part => part !== '')
  const latest = paragraphs.findLast(part => part.startsWith('**')) ?? paragraphs.at(-1) ?? ''
  const line = (latest.split(/\r?\n/)[0] ?? '').replaceAll('**', '').trim()
  return line.length > 120 ? `${line.slice(0, 119)}…` : line
}

/** What a function's contract says it is for: its first line of prose, not a JSDoc tag. */
export function contractSummary(description: string): string {
  return description.split(/\r?\n/).map(line => line.trim()).find(line => line !== '' && !line.startsWith('@')) ?? ''
}

/** Every item, with the steps inside calls. */
export function flattenItems(items: readonly ChatItem[]): ChatItem[] {
  return items.flatMap(item => item.kind === 'call' ? [item, ...flattenItems(item.steps)] : [item])
}

type Block = { readonly type?: unknown; readonly text?: unknown; readonly content?: unknown }

/** Rust nests the SessionEvent message under `data.message`; DSH puts it at `data`. */
function messageOf(event: AgentEvent): Record<string, unknown> {
  const nested = event.data.message
  return typeof nested === 'object' && nested !== null ? nested as Record<string, unknown> : event.data
}

function textOf(content: unknown, type: 'text' | 'reasoning' = 'text'): string {
  if (typeof content === 'string') return type === 'text' ? content : ''
  if (!Array.isArray(content)) return ''
  const parts: string[] = []
  for (const block of content as Block[]) {
    if (block?.type === type && typeof block.text === 'string') parts.push(block.text)
    else if (type === 'text' && block?.type === 'tool-result') parts.push(textOf(block.content))
  }
  return parts.join('\n')
}

/** Some model providers put a thinking block in text instead of the reasoning channel. */
function assistantDisplay(text: string, reasoning: string): { text: string; reasoning: string } {
  const leading = text.trimStart()
  const opening = /^<thinking>/i.exec(leading)
  if (!opening) {
    // A streaming chunk may stop halfway through the opening tag.
    if (leading.startsWith('<') && '<thinking>'.startsWith(leading.toLowerCase())) {
      return { text: '', reasoning }
    }
    return { text, reasoning }
  }
  const closing = /<\/thinking>/i.exec(leading.slice(opening[0].length))
  if (!closing) return { text: '', reasoning }
  const innerStart = opening[0].length
  const innerEnd = innerStart + closing.index
  const thought = leading.slice(innerStart, innerEnd).trim()
  const body = leading.slice(innerEnd + closing[0].length).trimStart()
  return { text: body, reasoning: [reasoning, thought].filter(Boolean).join('\n\n') }
}

function stringOf(value: unknown): string | undefined {
  return typeof value === 'string' && value !== '' ? value : undefined
}

function numberOf(value: unknown): number | undefined {
  return typeof value === 'number' ? value : undefined
}

function argumentsOf(value: unknown): string {
  if (typeof value === 'string') return value
  return value === undefined ? '' : JSON.stringify(value)
}

function reasonOf(value: unknown): TurnEndReason {
  const reason = (value ?? {}) as { kind?: unknown; goal_achieved?: unknown; unresolved?: unknown; error?: { message?: unknown; code?: unknown } }
  if (reason.kind === 'completed') return { kind: 'completed',
    goalAchieved: typeof reason.goal_achieved === 'boolean' ? reason.goal_achieved : undefined,
    unresolved: Array.isArray(reason.unresolved) ? reason.unresolved.filter((item): item is string => typeof item === 'string') : undefined }
  if (reason.kind === 'error') {
    return { kind: 'error', message: stringOf(reason.error?.message) ?? '', code: stringOf(reason.error?.code) }
  }
  return { kind: 'aborted' }
}

function resultOf(event: AgentEvent): ToolResult {
  const message = messageOf(event)
  const meta = (event.data.meta ?? {}) as { durationMs?: unknown; result?: { child_task_id?: unknown } }
  return {
    text: textOf(message.content),
    isError: message.isError === true,
    durationMs: numberOf(meta.durationMs),
    childTaskId: stringOf(meta.result?.child_task_id),
    time: event.time,
  }
}

export function callIdOfResult(event: AgentEvent): string | undefined {
  const message = messageOf(event)
  const source = message.source as { callId?: unknown } | undefined
  return stringOf(source?.callId) ?? stringOf(message.toolCallId)
}

function observerStringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string' && item.trim() !== '') : []
}

function recordOf(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null ? value as Record<string, unknown> : {}
}

function parsed(text: string): unknown {
  try { return JSON.parse(text) } catch { return undefined }
}

/** What each function frame last recorded about itself: its role, the call that opened it, its status and result. */
function frameRecords(events: readonly AgentEvent[]): Map<string, { role: string | undefined; callId: string | undefined; status: string; result: unknown }> {
  const frames = new Map<string, { role: string | undefined; callId: string | undefined; status: string; result: unknown }>()
  for (const event of events) {
    const id = event.type.startsWith('task/node_') ? stringOf(event.data.node_id) : undefined
    if (id === undefined) continue
    const metadata = recordOf(event.data.metadata)
    frames.set(id, { role: stringOf(metadata.role), callId: stringOf(recordOf(metadata.caller).call_id), status: stringOf(event.data.status) ?? 'pending', result: event.data.result })
  }
  return frames
}

/** A program's call of a @virtual or @module function, from the arguments the host recorded for it. */
function functionCall(raw: string): { name: string; description: string; module: boolean; line: number | undefined } | undefined {
  const args = recordOf(parsed(raw))
  const task = args.program_task
  if (typeof task !== 'object' || task === null) return undefined
  const contract = task as Record<string, unknown>
  return {
    name: stringOf(contract.name) ?? stringOf(args.goal) ?? '函数',
    description: stringOf(contract.description) ?? stringOf(args.briefing) ?? '',
    module: contract.module === true,
    line: numberOf(recordOf(args.call_site).line),
  }
}

/** A function that gave up returns {"$task_error": …}; anything else is its value. */
function callOutcome(text: string | undefined): { result?: string; error?: string } {
  if (text === undefined || text === '') return {}
  const value = parsed(text)
  if (typeof value === 'object' && value !== null && '$task_error' in value) {
    const error = (value as Record<string, unknown>).$task_error
    const message = recordOf(error).message
    return { error: typeof message === 'string' ? message : typeof error === 'string' ? error : JSON.stringify(error) }
  }
  return { result: text }
}

/** Steps inside a call belong to it; the frame they ran in names the call. */
function nestCalls(items: readonly ChatItem[]): ChatItem[] {
  const built = items.map(item => item.kind === 'call' ? { ...item, steps: [] as ChatItem[] } : item)
  const inside = new Map<string, ChatItem[]>()
  for (const item of built) if (item.kind === 'call' && item.child !== undefined) inside.set(item.child, item.steps as ChatItem[])
  const top: ChatItem[] = []
  for (const item of built) {
    const frame = 'frame' in item ? item.frame : undefined
    const owner = frame === undefined ? undefined : inside.get(frame)
    if (owner !== undefined) owner.push(item)
    else top.push(item)
  }
  return top
}

/** Project an ordered event log into transcript rows; tool results attach to their call. */
export function chatItems(events: readonly AgentEvent[], task?: Task): ChatItem[] {
  events = [...events].sort((a, b) => a.seq - b.seq)
  const delegates = new Map<string, { status: string; events: AgentEvent[]; report: string; review: string }>()
  for (const event of events) {
    const id = stringOf(event.data.agent_id)
    if (!id || !event.type.startsWith('subagent/')) continue
    const child = delegates.get(id) ?? { status: 'running', events: [], report: '', review: '' }
    if (event.type === 'subagent/event' && typeof event.data.event_type === 'string') {
      child.events.push({ ...event, type: event.data.event_type, data: event.data.event as Record<string, unknown> })
    } else if (event.type === 'subagent/end') {
      child.status = stringOf(event.data.status) ?? 'interrupted'
      const result = event.data.result as Record<string, unknown> | undefined
      child.report = [textOf(result?.report), stringOf(result?.error)].filter(Boolean).join('\n\n')
    } else if (event.type === 'subagent/review') {
      const review = event.data.review as Record<string, unknown> | undefined
      child.review = `${stringOf(review?.decision) ?? ''}：${stringOf(review?.evidence) ?? ''}`
    }
    delegates.set(id, child)
  }
  const deliveredInterjections = new Set(events.filter(event => event.type === 'user/interjection_delivered').map(event => Number(event.data.interjection_seq)))
  const results = new Map<string, ToolResult>()
  for (const event of events) {
    if (event.type !== 'tool/result') continue
    const id = callIdOfResult(event)
    if (id !== undefined) results.set(id, resultOf(event))
  }
  const frames = frameRecords(events)
  const childByCall = new Map<string, string>()
  for (const [id, frame] of frames) if (frame.role === 'temp' && frame.callId !== undefined) childByCall.set(frame.callId, id)
  const items: ChatItem[] = []
  // The reply each frame is streaming: concurrent functions stream at once.
  const streaming = new Map<string, number>()
  let turnStartTime: number | undefined
  /** The latest reply in a frame that holds a program the host has not answered yet. */
  const programReply = (frame: string | undefined): number => items.findLastIndex(item =>
    item.kind === 'assistant' && item.frame === frame && item.program === undefined && PROGRAM_FENCE.test(item.text))
  const markProgram = (frame: string | undefined, program: NonNullable<AssistantItem['program']>): boolean => {
    const index = programReply(frame)
    const reply = items[index]
    if (reply?.kind !== 'assistant') return false
    items[index] = { ...reply, program }
    return true
  }

  for (const event of events) {
    const key = `${event.type}:${event.seq}`
    const frame = stringOf(event.data.task_node_id)
    switch (event.type) {
      case 'subagent/start': {
        const agentId = stringOf(event.data.agent_id)
        if (!agentId) break
        const child = delegates.get(agentId)
        items.push({ kind: 'subagent', key: `delegate:${agentId}`, agentId, name: stringOf(event.data.name) ?? '子任务', status: child?.status ?? 'running', events: child?.events ?? [], report: child?.report ?? '', review: child?.review ?? '', time: event.time })
        break
      }
      case 'turn/start': {
        turnStartTime = event.time
        const turn = numberOf(event.data.turn) ?? 1
        if (turn > 1) items.push({ kind: 'turn', key, turn })
        break
      }
      case 'user/interjection':
        items.push({ kind: 'interjection', key, text: stringOf(event.data.text) ?? '', time: event.time, delivered: deliveredInterjections.has(event.seq) })
        break
      case 'user/message': {
        if (turnStartTime === undefined) {
          turnStartTime = event.time
        }
        const text = textOf(messageOf(event).content)
        const content=messageOf(event).content
        const attachments=(Array.isArray(content)?content:[]).filter(p=>p&&typeof p==='object'&&('attachment' in p)).map(p=>{
          const part=p as {type:string;attachment:{attachmentId:string;name:string}}
          return {name:part.attachment.name,url:`/agent/tasks/${encodeURIComponent(String(event.data.task_id))}/attachments/${encodeURIComponent(part.attachment.attachmentId)}`,image:part.type==='image'}
        })
        if (text !== ''||attachments.length) items.push({ kind: 'user', key, text, attachments, time: event.time })
        break
      }
      case 'assistant/delta': {
        const turn = numberOf(event.data.turn) ?? 0
        const step = numberOf(event.data.step) ?? 0
        const piece = stringOf(event.data.text) ?? ''
        const reasoningPiece = stringOf(event.data.reasoning) ?? ''
        const slot = `${frame ?? ''}:${turn}:${step}`
        const index = streaming.get(slot)
        const open = index === undefined ? undefined : items[index]
        if (index !== undefined && open?.kind === 'assistant') {
          items[index] = { ...open, text: open.text + piece, reasoning: open.reasoning + reasoningPiece, time: event.time }
        } else if (piece !== '' || reasoningPiece !== '') {
          streaming.set(slot, items.length)
          items.push({ kind: 'assistant', key, text: piece, reasoning: reasoningPiece, time: event.time, streaming: true, turn, step, frame })
        }
        break
      }
      case 'assistant/draft':
      case 'assistant/message': {
        // A draft is a function's closing reply; in the main frame it is provisional.
        if (event.type === 'assistant/draft' && (frame === undefined || frames.get(frame)?.role !== 'temp')) break
        const message = messageOf(event)
        // A function's closing reply is its return value, which its call shows.
        const text = event.type === 'assistant/draft' ? ''
          : (event.data.acceptance_pending === true ? '本轮执行结果（提交 Observer 验收）\n\n' : '') + textOf(message.content)
        // The summary the provider shared with the reply, kept after the stream is gone.
        const reasoning = [textOf(message.content, 'reasoning'), stringOf(message.reasoning_content)].filter(Boolean).join('\n\n')
        const turn = numberOf(event.data.turn) ?? 0
        const step = numberOf(event.data.step) ?? 0
        // Older hosts streamed without naming the frame.
        const slot = [`${frame ?? ''}:${turn}:${step}`, `:${turn}:${step}`].find(candidate => streaming.has(candidate))
        const index = slot === undefined ? undefined : streaming.get(slot)
        if (slot !== undefined) streaming.delete(slot)
        const open = index === undefined ? undefined : items[index]
        if (text === '' && reasoning === '') {
          if (index !== undefined && open?.kind === 'assistant') items[index] = { ...open, streaming: false }
          break
        }
        const reply: AssistantItem = { kind: 'assistant', key, text, reasoning, time: event.time, streaming: false, turn, step, frame }
        if (index !== undefined) items[index] = reply
        else items.push(reply)
        break
      }
      case 'observer/plan_review':
      case 'observer/progress_review': {
        const suggestions = Array.isArray(event.data.suggestions)
          ? event.data.suggestions.filter((value): value is string => typeof value === 'string' && value.trim() !== '')
          : []
        if (suggestions.length > 0) {
          items.push({ kind: 'observer', key, summary: stringOf(event.data.summary) ?? '', suggestions, time: event.time })
        }
        break
      }
      case 'observer/retrospective': {
        const original = event.data.observer_return
        const report = original && typeof original === 'object' ? original as Record<string, unknown> : event.data
        const acceptance = report.acceptance && typeof report.acceptance === 'object' ? report.acceptance as Record<string, unknown> : undefined
        const labels: Record<string, string> = { passed: '要求已满足', needs_work: '需要补齐', blocked: '受限，存在未完成项', uncertain: '尚未确认' }
        const requirements = Array.isArray(acceptance?.requirements) ? acceptance.requirements.flatMap(value => {
          if (!value || typeof value !== 'object') return []
          const item = value as Record<string, unknown>
          return [`- ${String(item.requirement ?? '')}：${String(item.result ?? '')}${item.explanation ? `；${String(item.explanation)}` : ''}`]
        }) : []
        const text = [acceptance ? `验收：${labels[String(acceptance.assessment)] ?? String(acceptance.assessment ?? '尚未确认')}` : undefined,
          ...requirements, report.action === 'continue' ? 'Observer 请求将具体缺口交回 Organizer，继续补齐。' : undefined,
          stringOf(report.summary) ?? stringOf(event.data.summary),
          stringOf(report.path_review) ?? stringOf(event.data.pathReview),
          ...observerStringList(report.shortening_opportunities ?? event.data.shorteningOpportunities).map(item => `- 下次可改进：${item}`),
          event.data.status === 'unavailable' ? `复盘未完成：${stringOf(event.data.message) ?? '未返回结果'}` : undefined]
          .filter(Boolean).join('\n\n')
        if (text) items.push({ kind: 'retrospective', key, text, time: event.time })
        break
      }
      case 'tool/call': {
        const callId = stringOf(event.data.callId) ?? key
        const name = stringOf(event.data.name) ?? 'tool'
        // A handed-over program shows as its program item, not as a tool card with its source.
        if (name === 'write_program') break
        const raw = argumentsOf(event.data.arguments)
        const result = results.get(callId)
        const call = name === 'create_task' ? functionCall(raw) : undefined
        if (call !== undefined) {
          // The caller records what the function returned; the frame keeps it too.
          const answer = result === undefined ? {} : recordOf(parsed(result.text))
          const child = childByCall.get(callId) ?? stringOf(answer.node_id)
          const record = child === undefined ? undefined : frames.get(child)
          const outcome = result === undefined
            ? callOutcome(stringOf(recordOf(record?.result).result) ?? stringOf(recordOf(record?.result).message))
            : result.isError || answer.error !== undefined ? { error: stringOf(answer.error) ?? result.text } : callOutcome(stringOf(answer.result))
          const status = record?.status ?? (result === undefined ? 'running' : outcome.error !== undefined ? 'failed' : 'done')
          items.push({ kind: 'call', key, callId, frame, child, ...call, status, ...outcome, time: event.time, steps: [] })
          break
        }
        const location = recordOf(event.data.program_location)
        items.push({ kind: 'tool', key, callId, name, arguments: raw, time: event.time, result, frame, step: numberOf(event.data.step), line: numberOf(location.line) })
        break
      }
      case 'program/start':
        // The host compiled the program the AI handed over and started it.
        if (!markProgram(frame, { status: 'started' })) {
          items.push({ kind: 'program', key, frame, event: 'started', text: event.data.fixed === true ? '修复后重新运行' : stringOf(event.data.function) ?? '', time: event.time })
        }
        break
      case 'program/source_invalid': {
        const diagnostic = recordOf(event.data.diagnostic)
        const first = recordOf(Array.isArray(diagnostic.diagnostics) ? diagnostic.diagnostics[0] : undefined)
        const error = stringOf(first.message) ?? stringOf(diagnostic.error) ?? ''
        const line = numberOf(first.line)
        if (!markProgram(frame, { status: 'invalid', error, line })) items.push({ kind: 'program', key, frame, event: 'invalid', text: error, line, time: event.time })
        break
      }
      case 'program/error': {
        const error = recordOf(event.data.error)
        const question = error.code === 'WAITING_FOR_USER'
        items.push({ kind: 'program', key, frame, event: question ? 'question' : 'failed', text: stringOf(error.message) ?? stringOf(event.data.error) ?? '', line: numberOf(recordOf(error.location).line), time: event.time })
        break
      }
      case 'program/paused':
        items.push({ kind: 'program', key, frame, event: 'paused', text: stringOf(event.data.stopped_at) ?? '', time: event.time })
        break
      case 'program/result_invalid':
        items.push({ kind: 'program', key, frame, event: 'result_invalid', text: stringOf(recordOf(event.data.diagnostic).error) ?? '', time: event.time })
        break
      case 'module/loaded':
        items.push({ kind: 'program', key, frame, event: 'module_loaded', text: `第 ${String(event.data.version ?? '?')} 版`, time: event.time })
        break
      case 'module/fix_started':
        items.push({ kind: 'program', key, frame, event: 'module_fix', text: stringOf(recordOf(event.data.error).message) ?? '', time: event.time })
        break
      case 'turn/end': {
        const durationMs = turnStartTime !== undefined ? Math.max(0, event.time - turnStartTime) : undefined
        const reason = reasonOf(event.data.reason)
        items.push({
          kind: 'turn-end',
          key,
          reason,
          durationMs,
          time: event.time,
        })
        turnStartTime = undefined
        break
      }
    }
  }

  // If task is completed/stopped but no turn/end event was recorded:
  if (
    items.length > 0 &&
    items.findLastIndex(item => item.kind === 'turn-end') < items.findLastIndex(item => item.kind === 'user' || item.kind === 'turn') &&
    task &&
    (task.status === 'completed' || task.status === 'failed' || task.status === 'interrupted' || task.status === 'cancelled')
  ) {
    const durationMs = turnStartTime !== undefined
      ? Math.max(0, (task.updated_at || Date.now()) - turnStartTime)
      : (task.updated_at && task.created_at ? Math.max(0, task.updated_at - task.created_at) : undefined)
    items.push({
      kind: 'turn-end',
      key: `synthetic:turn-end:${task.task_id}`,
      reason: task.status === 'completed' ? { kind: 'completed' } : { kind: 'aborted' },
      durationMs,
      time: task.updated_at,
    })
  }

  return nestCalls(items.map(item => item.kind === 'assistant'
    ? { ...item, ...assistantDisplay(item.text, summaryParagraphs(item.reasoning)) }
    : item))
}

/** Formats a millisecond duration into a human-friendly Chinese string (e.g. "28秒", "1分15秒", "1小时2分"). */
export function formatDuration(ms: number | undefined): string {
  if (ms === undefined || ms < 0 || isNaN(ms)) return ''
  const totalSeconds = Math.round(ms / 1000)
  if (totalSeconds < 1) return '< 1秒'
  if (totalSeconds < 60) return `${totalSeconds}秒`
  const minutes = Math.floor(totalSeconds / 60)
  const remainingSeconds = totalSeconds % 60
  if (minutes < 60) {
    return remainingSeconds > 0 ? `${minutes}分${remainingSeconds}秒` : `${minutes}分钟`
  }
  const hours = Math.floor(minutes / 60)
  const remainingMinutes = minutes % 60
  return remainingMinutes > 0 ? `${hours}小时${remainingMinutes}分` : `${hours}小时`
}

/** Formats milliseconds into mm:ss timer format (e.g. "00:23", "01:15"). */
export function formatTimer(ms: number | undefined): string {
  if (ms === undefined || ms < 0 || isNaN(ms)) return '00:00'
  const totalSeconds = Math.floor(ms / 1000)
  const mins = Math.floor(totalSeconds / 60)
  const secs = totalSeconds % 60
  return `${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`
}

export interface TaskDurationInfo {
  readonly currentTurnDurationMs?: number | undefined
  readonly isRunning: boolean
  readonly currentTurnStartTime?: number | undefined
  readonly currentTurnEndTime?: number | undefined
  readonly totalTaskDurationMs?: number | undefined
}

export function getTaskDurationInfo(
  events: readonly AgentEvent[],
  task?: Task,
  running?: boolean,
  now: number = Date.now()
): TaskDurationInfo {
  let firstStartTime: number | undefined
  let lastTurnStartTime: number | undefined
  let lastTurnEndTime: number | undefined
  let hasTurnEndAfterLastStart = false

  for (const event of events) {
    if (event.type === 'turn/start' || event.type === 'user/message') {
      if (firstStartTime === undefined) {
        firstStartTime = event.time
      }
      lastTurnStartTime = event.time
      hasTurnEndAfterLastStart = false
    } else if (event.type === 'turn/end') {
      lastTurnEndTime = event.time
      hasTurnEndAfterLastStart = true
    }
  }

  if (firstStartTime === undefined && task?.created_at) {
    firstStartTime = task.created_at
  }
  if (lastTurnStartTime === undefined && task?.created_at) {
    lastTurnStartTime = task.created_at
  }

  const isRunning = Boolean(
    running ||
    task?.status === 'running' ||
    task?.status === 'cancelling'
  )

  let currentTurnDurationMs: number | undefined
  let totalTaskDurationMs: number | undefined

  if (isRunning) {
    if (lastTurnStartTime !== undefined) {
      currentTurnDurationMs = Math.max(0, now - lastTurnStartTime)
    }
    if (firstStartTime !== undefined) {
      totalTaskDurationMs = Math.max(0, now - firstStartTime)
    }
  } else {
    if (lastTurnStartTime !== undefined) {
      const endTime = (hasTurnEndAfterLastStart && lastTurnEndTime !== undefined)
        ? lastTurnEndTime
        : (task?.updated_at && task.updated_at >= lastTurnStartTime ? task.updated_at : lastTurnStartTime)
      currentTurnDurationMs = Math.max(0, endTime - lastTurnStartTime)
      lastTurnEndTime = endTime
    }
    if (firstStartTime !== undefined) {
      const overallEndTime = lastTurnEndTime ?? task?.updated_at ?? firstStartTime
      totalTaskDurationMs = Math.max(0, overallEndTime - firstStartTime)
    }
  }

  return {
    currentTurnDurationMs,
    isRunning,
    currentTurnStartTime: lastTurnStartTime,
    currentTurnEndTime: isRunning ? undefined : lastTurnEndTime,
    totalTaskDurationMs,
  }
}

/** Whether the log is waiting on the model: a step opened with no reply yet. */
export function awaitingModel(events: readonly AgentEvent[]): boolean {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const type = events[index]?.type
    if (type === 'step/start') return true
    if (type === 'assistant/delta' || type === 'assistant/message' || type === 'tool/call' || type === 'tool/result' || type === 'turn/end') return false
  }
  return false
}
