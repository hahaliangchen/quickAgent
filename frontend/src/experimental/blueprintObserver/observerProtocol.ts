/**
 * Pure, React-free protocol & trigger helpers for the experimental Blueprint Observer.
 *
 * Designed to stay completely isolated from the core agent pipeline so the entire
 * `frontend/src/experimental/blueprintObserver/` directory can be added or removed cleanly.
 */

import type { AgentEvent, Task } from '../../api.ts'

export type ObserverSeverity = 'tip' | 'warn' | 'blocker'
export type ObserverAction = 'interject' | 'stop' | 'stop_and_continue'

export interface ObserverSuggestion {
  readonly id: string
  readonly shouldInterject: boolean
  readonly action: ObserverAction
  readonly severity: ObserverSeverity
  readonly title: string
  readonly reason: string
  readonly interject: string
  readonly sourceSeq?: number | undefined
}

export interface ObserverBlueprintNote {
  readonly title: string
  readonly trigger: string
  readonly summary: string
  readonly anchor_files: string[]
  readonly environment: string[]
  readonly pitfalls: string[]
  readonly verify_commands: string[]
  readonly sourceSeq?: number | undefined
}

export interface ObserverTrigger {
  readonly kind: 'manual' | 'error' | 'turn_failed' | 'plan_phase' | 'turn_completed' | 'stalled_tool'
  readonly label: string
  readonly latestSeq: number
  readonly detail?: string | undefined
}

export interface StalledInFlightTool {
  readonly callId: string
  readonly seq: number
  readonly toolName: string
  readonly startedAt: number
  readonly lastActivityAt: number
  readonly idleSec: number
}

export interface ObserverPrefs {
  readonly enabled: boolean
  readonly autoInspect: boolean
  readonly width: number
  readonly model?: { readonly providerId: string; readonly modelId: string } | undefined
  readonly reasoningEffort?: string | null | undefined
  readonly fastMode?: boolean | undefined
}

export const OBSERVER_TITLE_PREFIX = '[观察者] '
export const OBSERVER_COOLDOWN_MS = 45_000
export const OBSERVER_STALL_THRESHOLD_MS = 180_000

const PREFS_KEY = 'blueprint_observer_prefs_v1'
// v2 sessions are created with backend `kind: "observer"` + dedicated pinned system prompt.
const SESSION_MAP_KEY = 'blueprint_observer_sessions_v2'
const DISMISSED_KEY = 'blueprint_observer_dismissed_v1'

const DEFAULT_PREFS: ObserverPrefs = {
  enabled: false,
  autoInspect: true,
  width: 400,
}

export function loadObserverPrefs(storage?: Pick<Storage, 'getItem'>): ObserverPrefs {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    const raw = store?.getItem(PREFS_KEY)
    if (!raw) return DEFAULT_PREFS
    const parsed = JSON.parse(raw) as Record<string, unknown>
    const width = typeof parsed.width === 'number' && Number.isFinite(parsed.width)
      ? Math.min(720, Math.max(320, parsed.width))
      : DEFAULT_PREFS.width
    const modelRaw = parsed.model as Record<string, unknown> | undefined
    const model =
      modelRaw && typeof modelRaw.providerId === 'string' && typeof modelRaw.modelId === 'string' && modelRaw.modelId
        ? { providerId: modelRaw.providerId, modelId: modelRaw.modelId }
        : undefined
    const reasoningEffort =
      typeof parsed.reasoningEffort === 'string' && parsed.reasoningEffort.trim()
        ? parsed.reasoningEffort.trim()
        : parsed.reasoningEffort === null
          ? null
          : undefined
    const fastMode = typeof parsed.fastMode === 'boolean' ? parsed.fastMode : undefined
    return {
      enabled: parsed.enabled === true,
      autoInspect: parsed.autoInspect !== false,
      width,
      ...(model ? { model } : {}),
      ...(reasoningEffort !== undefined ? { reasoningEffort } : {}),
      ...(fastMode !== undefined ? { fastMode } : {}),
    }
  } catch {
    return DEFAULT_PREFS
  }
}

export function saveObserverPrefs(prefs: ObserverPrefs, storage?: Pick<Storage, 'setItem'>): void {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    store?.setItem(PREFS_KEY, JSON.stringify(prefs))
  } catch {
    // Ignore storage write failures.
  }
}

function loadSessionMap(storage?: Pick<Storage, 'getItem'>): Record<string, string> {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    const raw = store?.getItem(SESSION_MAP_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as unknown
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
    const out: Record<string, string> = {}
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === 'string' && v.trim()) out[k] = v
    }
    return out
  } catch {
    return {}
  }
}

export function getObserverSessionId(watchedTaskId: string, storage?: Pick<Storage, 'getItem'>): string | undefined {
  return loadSessionMap(storage)[watchedTaskId]
}

export function setObserverSessionId(
  watchedTaskId: string,
  observerTaskId: string | undefined,
  storage?: Pick<Storage, 'getItem' | 'setItem'>,
): void {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    const map = loadSessionMap(store)
    if (observerTaskId) {
      map[watchedTaskId] = observerTaskId
    } else {
      delete map[watchedTaskId]
    }
    // Keep the most recent 60 mappings to bound localStorage usage.
    const entries = Object.entries(map).slice(-60)
    store?.setItem(SESSION_MAP_KEY, JSON.stringify(Object.fromEntries(entries)))
  } catch {
    // Ignore storage errors.
  }
}

/** Prevent an observer session from observing itself if the user ever opens it directly. */
export function isObserverOwnTask(task: Pick<Task, 'task_id' | 'title'> | undefined, storage?: Pick<Storage, 'getItem'>): boolean {
  if (!task) return false
  if (task.title?.startsWith(OBSERVER_TITLE_PREFIX)) return true
  const map = loadSessionMap(storage)
  return Object.values(map).includes(task.task_id)
}

export function loadDismissedSuggestionIds(storage?: Pick<Storage, 'getItem'>): Set<string> {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    const raw = store?.getItem(DISMISSED_KEY)
    if (!raw) return new Set()
    const parsed = JSON.parse(raw) as unknown
    if (!Array.isArray(parsed)) return new Set()
    return new Set(parsed.filter((v): v is string => typeof v === 'string'))
  } catch {
    return new Set()
  }
}

export function saveDismissedSuggestionIds(ids: ReadonlySet<string>, storage?: Pick<Storage, 'setItem'>): void {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    store?.setItem(DISMISSED_KEY, JSON.stringify([...ids].slice(-120)))
  } catch {
    // Ignore storage errors.
  }
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined
}

function clipText(raw: string, maxLen: number): string {
  const clean = raw.replace(/\s+/gu, ' ').trim()
  return clean.length > maxLen ? `${clean.slice(0, maxLen)}…` : clean
}

/**
 * Decide whether newly arrived main-session events warrant waking the Observer Agent.
 *
 * Rules:
 * - Never trigger on user-initiated or observer-initiated cancellation (`aborted` / `cancelled`).
 * - Do NOT trigger on routine single tool/command errors (the main agent routinely fixes typos on its own).
 * - Wake only on real lifecycle signals: turn failure (`error`), plan phase transition (`plan/updated`),
 *   or natural turn completion (`completed`). (Stalled tools are handled separately by the single-shot watchdog.)
 */
export function detectObserverTrigger(
  events: readonly AgentEvent[],
  lastInspectedSeq: number,
  running: boolean,
): ObserverTrigger | null {
  if (events.length === 0) return null
  const fresh = events.filter(ev => ev.seq > lastInspectedSeq)
  if (fresh.length === 0) return null
  const latestSeq = fresh[fresh.length - 1]!.seq

  // Priority 1: inspect turn/end first — ignore user/observer cancellations, wake on genuine failure.
  for (let i = fresh.length - 1; i >= 0; i--) {
    const ev = fresh[i]!
    if (ev.type === 'turn/end') {
      const reason = asRecord(ev.data.reason)
      const reasonKind = typeof reason?.kind === 'string' ? reason.kind : ''
      if (reasonKind === 'aborted' || reasonKind === 'cancelled') {
        return null
      }
      if (reasonKind === 'error') {
        const msg = typeof reason?.message === 'string' ? reason.message : '主会话本轮运行失败'
        return {
          kind: 'turn_failed',
          label: '主会话本轮报错中断',
          latestSeq,
          detail: clipText(msg, 180),
        }
      }
      if (!running && reasonKind === 'completed') {
        return {
          kind: 'turn_completed',
          label: '主会话本轮结束复核',
          latestSeq,
        }
      }
    }
  }

  // Priority 2: semantic plan phase boundary (`plan/updated`).
  for (let i = fresh.length - 1; i >= 0; i--) {
    const ev = fresh[i]!
    if (ev.type === 'plan/updated') {
      const steps = Array.isArray(ev.data.steps) ? ev.data.steps : []
      const active = steps
        .map(asRecord)
        .find(step => step?.status === 'in_progress' && typeof step.title === 'string')
      const detail = typeof active?.title === 'string' ? clipText(active.title, 120) : undefined
      return {
        kind: 'plan_phase',
        label: '主会话计划阶段变更',
        latestSeq,
        ...(detail ? { detail } : {}),
      }
    }
  }

  return null
}

/**
 * Pure zero-cost stalled-tool detector for the Observer single-shot watchdog.
 *
 * Scans the main session's event stream to locate the latest unclosed `tool/call`
 * (without a matching `tool/result` in the active turn) and checks whether the session
 * has been completely silent (`nowMs - lastActivityAt >= thresholdMs`, default 3 min).
 */
export function findStalledInFlightTool(
  events: readonly AgentEvent[],
  nowMs: number,
  thresholdMs: number = OBSERVER_STALL_THRESHOLD_MS,
): StalledInFlightTool | null {
  if (events.length === 0 || !Number.isFinite(nowMs) || thresholdMs <= 0) return null

  const inFlight = new Map<string, { callId: string; seq: number; toolName: string; startedAt: number }>()
  for (const ev of events) {
    if (ev.type === 'turn/end') {
      inFlight.clear()
      continue
    }
    if (ev.type === 'tool/call') {
      const rawId = typeof ev.data.callId === 'string' && ev.data.callId
        ? ev.data.callId
        : (typeof ev.data.call_id === 'string' && ev.data.call_id ? ev.data.call_id : `call_${ev.seq}`)
      const toolName = typeof ev.data.name === 'string' && ev.data.name ? ev.data.name : 'unknown'
      inFlight.set(rawId, {
        callId: rawId,
        seq: ev.seq,
        toolName,
        startedAt: typeof ev.time === 'number' && Number.isFinite(ev.time) ? ev.time : 0,
      })
      continue
    }
    if (ev.type === 'tool/result') {
      const msg = asRecord(ev.data.message)
      const cid = typeof msg?.toolCallId === 'string' && msg.toolCallId
        ? msg.toolCallId
        : (typeof ev.data.callId === 'string' && ev.data.callId
          ? ev.data.callId
          : (typeof ev.data.call_id === 'string' ? ev.data.call_id : ''))
      if (cid) inFlight.delete(cid)
    }
  }

  if (inFlight.size === 0) return null

  let latestCall: { callId: string; seq: number; toolName: string; startedAt: number } | null = null
  for (const call of inFlight.values()) {
    if (!latestCall || call.seq > latestCall.seq) latestCall = call
  }
  if (!latestCall) return null

  let lastActivityAt = latestCall.startedAt
  for (const ev of events) {
    if (ev.seq >= latestCall.seq && typeof ev.time === 'number' && Number.isFinite(ev.time) && ev.time > lastActivityAt) {
      lastActivityAt = ev.time
    }
  }
  if (lastActivityAt <= 0) return null

  const idleMs = nowMs - lastActivityAt
  if (idleMs < thresholdMs) return null

  return {
    callId: latestCall.callId,
    seq: latestCall.seq,
    toolName: latestCall.toolName,
    startedAt: latestCall.startedAt,
    lastActivityAt,
    idleSec: Math.floor(idleMs / 1000),
  }
}

/**
 * Build a concise factual wake-up signal for the dedicated Observer Agent.
 * Role, inspection methodology, and output schema live ONCE in the backend
 * `pinned_system_prompt` for `kind: "observer"`, never repeated inside user turns.
 */
export function buildObserverInspectPrompt(input: {
  readonly watchedTaskId: string
  readonly watchedTitle: string
  readonly watchedStatus: string
  readonly workspaceRoot?: string | undefined
  readonly trigger: ObserverTrigger
  readonly recentSummary?: string | undefined
}): string {
  const detail = input.trigger.detail ? `（${input.trigger.detail}）` : ''
  const extra = input.recentSummary ? ` ${input.recentSummary}` : ''
  return `[巡检信号] ${input.trigger.label}${detail}（主会话状态：${input.watchedStatus}）。请从尾部检查最近在做什么任务，必要时可翻阅历史 DB 记录。${extra}`
}

export const OBSERVER_META_START = '<!--observer-meta-start-->'
export const OBSERVER_META_END = '<!--observer-meta-end-->'

/**
 * Pass the user's message straight to the dedicated Observer Agent without
 * injecting hidden boilerplate paragraphs.
 */
export function buildObserverUserChatPrompt(input: {
  readonly watchedTaskId: string
  readonly watchedStatus: string
  readonly userText: string
  readonly isFirstMessage: boolean
}): string {
  return input.userText.trim()
}

export function buildObserverBlueprintReportPrompt(): string {
  return '请复盘主会话截至目前的实战进展、关键改动的锚点文件、环境约束（端口/目录/服务隔离）、踩过的坑与有效验收命令，提炼成一份精炼的战地蓝图呈报，并在回复末尾附 ```blueprint-note``` JSON 代码块呈报给架构蓝图工作台。'
}

function toStringList(raw: unknown): string[] {
  if (!Array.isArray(raw)) return []
  const out: string[] = []
  for (const item of raw) {
    if (typeof item === 'string') {
      const t = item.trim()
      if (t && !out.includes(t)) out.push(t)
    }
  }
  return out
}

/**
 * Extract the latest `blueprint-note` JSON block from an Observer assistant message.
 */
export function parseObserverBlueprintNote(
  assistantText: string,
  sourceSeq?: number,
): ObserverBlueprintNote | null {
  const regex = /```blueprint-note\s*([\s\S]*?)```/gu
  let lastParsed: Record<string, unknown> | null = null
  for (const match of assistantText.matchAll(regex)) {
    const body = match[1]?.trim()
    if (!body || !body.startsWith('{')) continue
    try {
      const rec = asRecord(JSON.parse(body))
      if (rec && typeof rec.title === 'string' && rec.title.trim()) {
        lastParsed = rec
      }
    } catch {
      // Skip malformed blocks.
    }
  }
  if (!lastParsed) return null
  const title = typeof lastParsed.title === 'string' ? lastParsed.title.trim() : ''
  if (!title) return null
  return {
    title,
    trigger: typeof lastParsed.trigger === 'string' ? lastParsed.trigger.trim() : '',
    summary: typeof lastParsed.summary === 'string' ? lastParsed.summary.trim() : '',
    anchor_files: toStringList(lastParsed.anchor_files),
    environment: toStringList(lastParsed.environment),
    pitfalls: toStringList(lastParsed.pitfalls),
    verify_commands: toStringList(lastParsed.verify_commands),
    sourceSeq,
  }
}

function normalizeSeverity(raw: unknown): ObserverSeverity {
  if (raw === 'blocker' || raw === 'error' || raw === 'high') return 'blocker'
  if (raw === 'warn' || raw === 'warning' || raw === 'medium') return 'warn'
  return 'tip'
}

function normalizeAction(raw: unknown): ObserverAction {
  if (raw === 'stop') return 'stop'
  if (raw === 'stop_and_continue' || raw === 'stop_and_interject') return 'stop_and_continue'
  return 'interject'
}

/**
 * Extract the latest `observer-suggestion` JSON block from an Observer assistant message.
 */
export function parseObserverSuggestion(
  assistantText: string,
  sourceSeq?: number,
): ObserverSuggestion | null {
  const regex = /```(?:observer-suggestion|json)?\s*([\s\S]*?)```/gu
  let lastParsed: Record<string, unknown> | null = null
  for (const match of assistantText.matchAll(regex)) {
    const body = match[1]?.trim()
    if (!body || !body.startsWith('{')) continue
    try {
      const parsed = JSON.parse(body) as unknown
      const rec = asRecord(parsed)
      if (rec && ('should_interject' in rec || 'interject' in rec || 'action' in rec)) {
        lastParsed = rec
      }
    } catch {
      // Skip non-JSON or malformed fences.
    }
  }
  if (!lastParsed) return null

  const shouldInterject = lastParsed.should_interject === true
  const action = normalizeAction(lastParsed.action)
  const interject = typeof lastParsed.interject === 'string' ? lastParsed.interject.trim() : ''
  const title = typeof lastParsed.title === 'string' ? lastParsed.title.trim() : ''
  const reason = typeof lastParsed.reason === 'string' ? lastParsed.reason.trim() : ''
  const severity = normalizeSeverity(lastParsed.severity)

  const actionable = shouldInterject && (action === 'stop' || interject.length > 0)
  if (!actionable) {
    return {
      id: `obs:${sourceSeq ?? 0}:noop`,
      shouldInterject: false,
      action,
      severity,
      title,
      reason,
      interject: '',
      sourceSeq,
    }
  }

  return {
    id: `obs:${sourceSeq ?? 0}:${action}:${title || interject.slice(0, 24)}`,
    shouldInterject: true,
    action,
    severity,
    title: title || (action === 'stop' ? '建议停止当前运行' : '观察者纠偏建议'),
    reason: reason || '观察者从 SQLite 任务记录中发现了值得确认的决断事项。',
    interject,
    sourceSeq,
  }
}

function extractContentText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  const parts: string[] = []
  for (const item of content) {
    const rec = asRecord(item)
    if (!rec) continue
    if (rec.type === 'reasoning') continue
    if (typeof rec.text === 'string') parts.push(rec.text)
  }
  return parts.join('')
}

function readEventMessageText(ev: AgentEvent): string {
  const msg = asRecord(ev.data.message)
  if (msg && 'content' in msg) {
    const fromMsg = extractContentText(msg.content)
    if (fromMsg) return fromMsg
  }
  if (typeof ev.data.text === 'string' && ev.data.text) return ev.data.text
  if ('content' in ev.data) return extractContentText(ev.data.content)
  return ''
}

function rewriteEventMessageText(ev: AgentEvent, transform: (text: string) => string): AgentEvent {
  const msg = asRecord(ev.data.message)
  if (msg && 'content' in msg) {
    if (typeof msg.content === 'string') {
      const next = transform(msg.content)
      if (next === msg.content) return ev
      return { ...ev, data: { ...ev.data, message: { ...msg, content: next } } }
    }
    if (Array.isArray(msg.content)) {
      let changed = false
      const nextContent = msg.content.map((part) => {
        const rec = asRecord(part)
        if (!rec || rec.type === 'reasoning' || typeof rec.text !== 'string') return part
        const updated = transform(rec.text)
        if (updated !== rec.text) changed = true
        return updated === rec.text ? part : { ...rec, text: updated }
      })
      if (!changed) return ev
      return { ...ev, data: { ...ev.data, message: { ...msg, content: nextContent } } }
    }
  }
  if (typeof ev.data.text === 'string') {
    const next = transform(ev.data.text)
    if (next === ev.data.text) return ev
    return { ...ev, data: { ...ev.data, text: next } }
  }
  return ev
}

/**
 * Strip internal observer context headers from user messages before rendering in the UI.
 */
export function stripObserverUserPromptMeta(raw: string): string {
  // 1. Delimited meta wrapper (new messages).
  const startIdx = raw.indexOf(OBSERVER_META_START)
  const endIdx = raw.indexOf(OBSERVER_META_END)
  if (startIdx >= 0 && endIdx > startIdx) {
    const stripped = `${raw.slice(0, startIdx)}${raw.slice(endIdx + OBSERVER_META_END.length)}`.trim()
    return stripped || raw
  }

  // 2. Legacy first-turn wrapper already persisted in SQLite.
  if (raw.startsWith('[观察者上下文]')) {
    const marker = '方便用户一键确认执行。'
    const cut = raw.indexOf(marker)
    if (cut >= 0) {
      const after = raw.slice(cut + marker.length).trim()
      if (after) return after
    }
  }

  // 3. Legacy follow-up wrapper already persisted in SQLite.
  if (raw.startsWith('[当前主会话 ')) {
    const cut = raw.indexOf(']\n\n')
    if (cut >= 0) {
      const after = raw.slice(cut + 3).trim()
      if (after) return after
    }
  }

  // 4. Auto/manual SQLite inspect system prompt -> compact readable pill line.
  if (raw.startsWith('你是「蓝图全景观察者（只读）」')) {
    const match = /- 触发原因：(.+)/u.exec(raw)
    return match?.[1] ? `SQLite 全局巡检 · ${match[1].trim()}` : 'SQLite 全局巡检'
  }

  return raw
}

/**
 * Strip the machine-readable `observer-suggestion` JSON fence from assistant markdown
 * so the user sees clean prose + our styled confirmation push card (not a 1-line JSON dump).
 */
export function stripObserverSuggestionFence(raw: string): string {
  const note = parseObserverBlueprintNote(raw)
  let stripped = raw
    .replace(/\n*```observer-suggestion\s*[\s\S]*?```\s*/gu, '\n')
    .replace(/\n*```blueprint-note\s*[\s\S]*?```\s*/gu, '\n')
    .replace(/\n*```json\s*\{[\s\S]*?"should_interject"[\s\S]*?\}\s*```\s*/gu, '\n')
    .trim()
  if (note) {
    const line = `> 已呈报战地蓝图线索：**${note.title}**`
    stripped = stripped ? `${stripped}\n\n${line}` : line
  }
  return stripped
}

/**
 * Prepare Observer session events for clean display in the Observer side chat:
 * - Hides internal prompt boilerplate from user bubbles.
 * - Hides raw `observer-suggestion` JSON fences from assistant messages (shown via the styled push card).
 */
export function sanitizeObserverEventsForDisplay(events: readonly AgentEvent[]): AgentEvent[] {
  return events.map((ev) => {
    if (ev.type === 'user/message') {
      return rewriteEventMessageText(ev, stripObserverUserPromptMeta)
    }
    if (ev.type === 'assistant/message') {
      return rewriteEventMessageText(ev, stripObserverSuggestionFence)
    }
    return ev
  })
}

/**
 * Extract the text of the latest completed assistant message from an Observer session's events.
 * Supports `event.data.message.content` (QuickAgent's durable format) as well as flat `text`/`content`.
 */
export function extractLatestAssistantMessage(
  events: readonly AgentEvent[],
): { readonly seq: number; readonly text: string } | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const ev = events[i]!
    if (ev.type === 'assistant/message') {
      const text = readEventMessageText(ev)
      if (text.trim()) return { seq: ev.seq, text }
    }
  }
  return null
}
