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
const SESSION_MAP_KEY = 'blueprint_observer_sessions_v1'
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
 * Decide whether newly arrived main-session events warrant an automatic Observer DB inspection.
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

  // Priority 1: turn ended with an error (e.g. WORKER_FAILED / HTTP 503).
  for (let i = fresh.length - 1; i >= 0; i--) {
    const ev = fresh[i]!
    if (ev.type === 'turn/end') {
      const reason = asRecord(ev.data.reason)
      if (reason?.kind === 'error') {
        const msg = typeof reason.message === 'string' ? reason.message : '主会话本轮运行失败'
        return {
          kind: 'turn_failed',
          label: '主会话本轮报错中断',
          latestSeq,
          detail: clipText(msg, 180),
        }
      }
    }
  }

  // Priority 2: tool execution error or TS program compile error while active.
  for (let i = fresh.length - 1; i >= 0; i--) {
    const ev = fresh[i]!
    if (ev.type === 'tool/result') {
      const msg = asRecord(ev.data.message)
      if (msg?.isError === true) {
        const content = Array.isArray(msg.content) ? asRecord(msg.content[0]) : undefined
        const text = typeof content?.text === 'string' ? content.text : '工具调用失败'
        return {
          kind: 'error',
          label: '检测到工具或编译报错',
          latestSeq,
          detail: clipText(text, 180),
        }
      }
    }
    if (ev.type === 'program/error' || ev.type === 'workspace/changes_error') {
      const msg = typeof ev.data.message === 'string' ? ev.data.message : ev.type
      return {
        kind: 'error',
        label: '检测到编排脚本报错',
        latestSeq,
        detail: clipText(msg, 180),
      }
    }
  }

  // Priority 3: semantic plan phase boundary (`plan/updated`) — driven by the
  // main Agent's own plan transitions rather than any hardcoded step count.
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

  // Priority 4: natural turn completion checkpoint.
  if (!running) {
    for (let i = fresh.length - 1; i >= 0; i--) {
      const ev = fresh[i]!
      if (ev.type === 'turn/end') {
        return {
          kind: 'turn_completed',
          label: '主会话本轮结束复核',
          latestSeq,
        }
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
 * Build the SQLite-first inspection prompt sent to the Observer AI session.
 */
export function buildObserverInspectPrompt(input: {
  readonly watchedTaskId: string
  readonly watchedTitle: string
  readonly watchedStatus: string
  readonly workspaceRoot?: string | undefined
  readonly trigger: ObserverTrigger
  readonly recentSummary?: string | undefined
}): string {
  const lines = [
    `你是「蓝图全景观察者（只读）」，专属于主会话 \`${input.watchedTaskId}\`。`,
    `- 主会话标题：${input.watchedTitle || '未命名会话'}`,
    `- 当前状态：${input.watchedStatus}`,
    ...(input.workspaceRoot ? [`- 工作区：${input.workspaceRoot}`] : []),
    `- 触发原因：${input.trigger.label}${input.trigger.detail ? `（${input.trigger.detail}）` : ''}`,
    ...(input.recentSummary ? [`- 最新进展摘要：${input.recentSummary}`] : []),
    '',
    '请按以下步骤高效执行（严格只读，禁止修改任何文件或执行写操作）：',
    `1. **一步拿到全局快照 + 多轮指令时间线 + 实时终端尾巴**：直接调用 \`blueprint.inspect_session({ task_id: "${input.watchedTaskId}", max_recent: 20 })\`（注意用点号 \`blueprint.inspect_session\`）。一次即可读出 SQLite 全量摘要（含按时间排序的多轮 \`user_instructions\` 与 \`latest_user_instruction\`、运行中工具 \`in_flight_tools\` 及实时 SSH/Shell 终端最后 30 行 \`tail_lines\`、计划进度、钉住事实、失败聚类、改动文件、环境线索、费用）以及 \`recent_tail\`（最新 20 条关键实时事件）。**严禁**用 \`after_seq\` 从头逐页翻历史浪费时间；如需看某条报错的完整内容，仅按 \`event_seq\` 点查 \`session.read_session_history({ task_id: "${input.watchedTaskId}", event_seq })\`。`,
    '2. **核心判据（必须严格遵守）**：',
    '   - **最新授权优先**：判定主会话「是否越界/跑偏」必须对照 `user_instructions` 全量时间线与 `latest_user_instruction`；当后续轮次的 `user/message` 或 `user/interjection` 明确放宽或覆盖了第 1 轮红线（如允许改代码、允许重启指定服务）时，以最新轮次用户授权为准，**严禁**拿第 1 轮已被覆盖的旧约束误报越界。',
    '   - **先看运行中终端尾巴**：优先检查 `in_flight_tools` 的 `elapsed_sec`、`idle_sec` 与 `tail_lines`；若终端尾部已明确报错（如 HTTP 4xx/5xx、异常堆栈、进程退出）或长时间零新输出卡死，直接建议 `action: "stop"` 或 `"stop_and_continue"` 并引用终端报错原文，不要凭空猜测进程是否还在后台。',
    '3. **源码真相双视野核验**：你拥有当前工作区的只读权限，如发现主会话怀疑某个源码逻辑或改动异常，可直接用 `read_file` / `search_text` / 只读 `git diff` 对照本地源码真相，并按需查 `blueprint.overview()` / `blueprint.read_playbook` 对照蓝图最短路径。',
    '4. **高信噪比决断**：如果主会话进展正常、无需打断，则设置 `"should_interject": false`；只有在发现**明确风险、踩坑、偏航、重复报错或需止损**时，才设置 `"should_interject": true`。',
    '',
    '请在回复末尾**必须**附上如下 JSON 代码块（前端会自动渲染为确认推送卡片，隐藏原始 JSON）：',
    '```observer-suggestion',
    '{',
    '  "should_interject": true,',
    '  "action": "interject",',
    '  "severity": "warn",',
    '  "title": "15字以内的简短提醒标题",',
    '  "reason": "结合 SQLite 记录与源码核验的一两句话诊断",',
    '  "interject": "用户确认后将直接插嘴发送给主会话的精炼指令（若 action 为 stop 可留空）"',
    '}',
    '```',
    '- `action` 取值：`"interject"`（向主会话插嘴/续发）、`"stop"`（建议立即停止主会话运行）、`"stop_and_continue"`（先停止当前跑偏轮次，再发送纠偏指令）。',
    '- 无需介入时输出 `{"should_interject": false, "action": "interject", "severity": "tip", "title": "", "reason": "当前进展正常", "interject": ""}`。',
  ]
  return lines.join('\n')
}

export const OBSERVER_META_START = '<!--observer-meta-start-->'
export const OBSERVER_META_END = '<!--observer-meta-end-->'

/**
 * Wrap a user's conversational message in the Observer side chat so the Observer
 * always knows which main task it is watching and emits an updated confirmation card
 * whenever a concrete action is agreed upon.
 */
export function buildObserverUserChatPrompt(input: {
  readonly watchedTaskId: string
  readonly watchedStatus: string
  readonly userText: string
  readonly isFirstMessage: boolean
}): string {
  const metaBody = input.isFirstMessage
    ? [
        `[观察者上下文] 你是专属于主会话 \`${input.watchedTaskId}\`（当前状态：${input.watchedStatus}）的只读蓝图观察者。`,
        `查主会话进展请直接一步调用 \`blueprint.inspect_session({ task_id: "${input.watchedTaskId}", max_recent: 20 })\`（含完整多轮 \`user_instructions\` / \`latest_user_instruction\`、运行中工具 \`in_flight_tools\` 实时终端 \`tail_lines\` 与最新 20 条事件 \`recent_tail\`），不要从头按 \`after_seq\` 翻页；需要核对代码真相可直接用只读 \`read_file\` / \`search_text\` / \`git diff\`。`,
        '判据铁律：①「最新授权优先」：判定是否越界/跑偏必须以 `user_instructions` 全量时间线与 `latest_user_instruction` 为准，禁止拿第 1 轮已被后续轮次覆盖的旧约束误报越界；②「先看运行中终端尾巴」：优先核对 `in_flight_tools` 的 `elapsed_sec`、`idle_sec` 与 `tail_lines`，若已报错或长时间零新输出，直接建议 `action: "stop"` 或 `"stop_and_continue"` 并引用终端报错原文。',
        '如果和用户讨论后形成了可直接执行的插嘴或停止决断方案，请在回复末尾附上 ```observer-suggestion JSON 块（含 should_interject, action: "interject"|"stop"|"stop_and_continue", severity, title, reason, interject），方便用户一键确认执行。',
      ].join('\n')
    : `[当前主会话 \`${input.watchedTaskId}\` 状态：${input.watchedStatus}；如需刷新进展请一步调用 blueprint.inspect_session({ task_id: "${input.watchedTaskId}", max_recent: 20 })，注意最新授权优先（以 user_instructions/latest_user_instruction 为准）并先看 in_flight_tools 实时终端 tail_lines，可配合只读 read_file/search_text 核验源码；形成新方案请在末尾附 \`\`\`observer-suggestion JSON 块]`
  return `${OBSERVER_META_START}\n${metaBody}\n${OBSERVER_META_END}\n\n${input.userText.trim()}`
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
  const stripped = raw
    .replace(/\n*```observer-suggestion\s*[\s\S]*?```\s*/gu, '\n')
    .replace(/\n*```json\s*\{[\s\S]*?"should_interject"[\s\S]*?\}\s*```\s*/gu, '\n')
    .trim()
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
