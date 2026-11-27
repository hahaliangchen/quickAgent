/**
 * Experimental, fully detachable Blueprint Observer sidecar for main conversations.
 *
 * Architecture:
 * - Lives 100% inside `frontend/src/experimental/blueprintObserver/`.
 * - Mounted via ONE single optional component `<BlueprintObserverHost ... />` in `App.tsx`.
 * - Each watched main conversation owns a dedicated, read-only (`permission_mode: 'read_only'`)
 *   archived Observer session that inspects SQLite (`blueprint__inspect_session`) + playbooks.
 * - When enabled and an actionable pitfall / drift / stop decision is found, a floating
 *   suggestion popup appears in the main work view; clicking it opens the dedicated Observer
 *   side chat drawer where the user can discuss the plan with the Observer and confirm
 *   execution (interject into running session, send follow-up, stop main run, or stop + redirect).
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import type { AgentEvent, SettingsData, Task } from '../../api.ts'
import { useAgentApi } from '../../cordis/react.tsx'
import type { AgentSlotProps } from '../../cordis/slot-contract.ts'
import { startHorizontalResize } from '../../components/dragResize.ts'
import { PREFILL_EVENT, type PrefillDetail } from '../../components/RewindControl.tsx'
import { ScrollToBottom, transcriptScroller, useScrollToBottom } from '../../components/ScrollToBottom.tsx'
import { Icon } from '../../components/WorkbenchIcon.tsx'
import { ModelSelect } from '../../dsh/model-selection/ModelSelect.tsx'
import { randomId } from '../../randomId.ts'
import { taskTitle } from '../../sidebarModel.ts'
import { useSession } from '../../useSession.ts'
import {
  OBSERVER_COOLDOWN_MS,
  OBSERVER_STALL_THRESHOLD_MS,
  OBSERVER_TITLE_PREFIX,
  buildObserverInspectPrompt,
  buildObserverUserChatPrompt,
  detectObserverTrigger,
  extractLatestAssistantMessage,
  findStalledInFlightTool,
  getObserverSessionId,
  isObserverOwnTask,
  loadDismissedSuggestionIds,
  loadObserverPrefs,
  parseObserverSuggestion,
  sanitizeObserverEventsForDisplay,
  saveDismissedSuggestionIds,
  saveObserverPrefs,
  setObserverSessionId,
  stripObserverUserPromptMeta,
  type ObserverPrefs,
  type ObserverSeverity,
  type ObserverSuggestion,
  type ObserverTrigger,
} from './observerProtocol.ts'
import css from './BlueprintObserverHost.module.css'

export interface BlueprintObserverHostProps {
  readonly watchedTask: Task
  readonly watchedEvents: readonly AgentEvent[]
  readonly watchedRunning: boolean
  readonly workspaceRoot: string
  readonly renderSlot: AgentSlotProps['renderSlot']
  readonly onMainSessionUpdated: () => void
  readonly onOpenTask: (taskId: string) => void
  readonly onFilePreview?: ((taskId: string, path: string, options?: { line?: number; url?: string }, root?: string) => void) | undefined
}

function severityLabel(severity: ObserverSeverity): string {
  switch (severity) {
    case 'blocker': return '高危阻断'
    case 'warn': return '避坑提醒'
    default: return '蓝图建议'
  }
}

function actionLabel(action: ObserverSuggestion['action'], mainRunning: boolean): string {
  switch (action) {
    case 'stop': return '确认停止主会话'
    case 'stop_and_continue': return mainRunning ? '停止并发送纠偏指令' : '确认发送纠偏指令'
    default: return mainRunning ? '确认插嘴' : '确认发送给主会话'
  }
}

export function BlueprintObserverHost({
  watchedTask,
  watchedEvents,
  watchedRunning,
  workspaceRoot,
  renderSlot,
  onMainSessionUpdated,
  onOpenTask,
  onFilePreview,
}: BlueprintObserverHostProps) {
  // Never observe an observer session itself.
  if (isObserverOwnTask(watchedTask)) return null

  return (
    <BlueprintObserverInner
      key={watchedTask.task_id}
      watchedTask={watchedTask}
      watchedEvents={watchedEvents}
      watchedRunning={watchedRunning}
      workspaceRoot={workspaceRoot}
      renderSlot={renderSlot}
      onMainSessionUpdated={onMainSessionUpdated}
      onOpenTask={onOpenTask}
      onFilePreview={onFilePreview}
    />
  )
}

function BlueprintObserverInner({
  watchedTask,
  watchedEvents,
  watchedRunning,
  workspaceRoot,
  renderSlot,
  onMainSessionUpdated,
  onOpenTask,
  onFilePreview,
}: BlueprintObserverHostProps) {
  const api = useAgentApi()
  const watchedTaskId = watchedTask.task_id
  const watchedTitle = taskTitle(watchedTask)

  const [prefs, setPrefs] = useState<ObserverPrefs>(() => loadObserverPrefs())
  const [drawerOpen, setDrawerOpen] = useState(false)
  const [observerTaskId, setObserverTaskId] = useState<string | undefined>(() => getObserverSessionId(watchedTaskId))
  const [settings, setSettings] = useState<SettingsData | undefined>(undefined)
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const [executing, setExecuting] = useState(false)
  const [error, setError] = useState('')
  const [actionToast, setActionToast] = useState<string | null>(null)

  // Latest structured suggestion from this session's Observer & optional inline edit state.
  const [dismissedIds, setDismissedIds] = useState<Set<string>>(() => loadDismissedSuggestionIds())
  const [activeSuggestion, setActiveSuggestion] = useState<ObserverSuggestion | null>(null)
  const [popupDismissedId, setPopupDismissedId] = useState<string | null>(null)
  const [decisionText, setDecisionText] = useState('')
  const [lastTriggerLabel, setLastTriggerLabel] = useState<string>('')

  const observerSession = useSession(observerTaskId, () => {})
  const observerTask = observerSession.task?.task_id === observerTaskId ? observerSession.task : undefined
  const observerRunning = observerTask?.status === 'running' || observerTask?.status === 'cancelling'

  const transcriptRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLTextAreaElement>(null)
  const { away: tailHidden, jump: jumpToTail, following } = useScrollToBottom(transcriptRef, drawerOpen)

  // Baseline event seq on mount so opening an old historical session does NOT immediately fire an auto-inspect.
  const lastInspectedSeqRef = useRef<number>(watchedEvents.at(-1)?.seq ?? 0)
  const lastAutoInspectAtRef = useRef<number>(0)
  const watchdogFiredCallIdsRef = useRef<Set<string>>(new Set())
  const watchedEventsRef = useRef(watchedEvents)
  watchedEventsRef.current = watchedEvents
  const observerBusyRef = useRef(sending || observerRunning)
  observerBusyRef.current = sending || observerRunning
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  const showToast = useCallback((text: string) => {
    if (toastTimerRef.current !== null) clearTimeout(toastTimerRef.current)
    setActionToast(text)
    toastTimerRef.current = setTimeout(() => {
      setActionToast(null)
      toastTimerRef.current = null
    }, 2800)
  }, [])

  useEffect(() => () => {
    if (toastTimerRef.current !== null) clearTimeout(toastTimerRef.current)
  }, [])

  const updatePrefs = useCallback((patch: Partial<ObserverPrefs>) => {
    setPrefs(prev => {
      const next: ObserverPrefs = { ...prev, ...patch }
      saveObserverPrefs(next)
      return next
    })
  }, [])

  // Lazily fetch model settings when the drawer opens or observer is enabled.
  useEffect(() => {
    if ((!drawerOpen && !prefs.enabled) || settings !== undefined) return
    api.getSettings().then(setSettings, () => {})
  }, [api, drawerOpen, prefs.enabled, settings])

  // Capture rewinds from THIS observer session only, stripping internal prompt headers.
  useEffect(() => {
    if (!drawerOpen || !observerTaskId) return
    const onPrefill = (event: Event) => {
      const detail = (event as CustomEvent<PrefillDetail | string>).detail
      const targetTaskId = typeof detail === 'string' ? undefined : detail?.taskId
      if (!targetTaskId || targetTaskId !== observerTaskId) return
      const text = typeof detail === 'string' ? detail : detail.text
      if (!text) return
      setDraft(stripObserverUserPromptMeta(text))
      window.requestAnimationFrame(() => inputRef.current?.focus())
    }
    window.addEventListener(PREFILL_EVENT, onPrefill)
    return () => window.removeEventListener(PREFILL_EVENT, onPrefill)
  }, [drawerOpen, observerTaskId])

  // Auto-scroll observer transcript to newest message.
  useLayoutEffect(() => {
    if (!drawerOpen) return
    const host = transcriptRef.current
    if (host === null) return
    const box = transcriptScroller(host)
    following.current = true
    const pin = () => { if (following.current) box.scrollTop = box.scrollHeight }
    pin()
    const observer = new ResizeObserver(pin)
    observer.observe(box)
    for (const node of Array.from(box.children)) observer.observe(node)
    return () => observer.disconnect()
  }, [drawerOpen, observerTaskId, following])

  // Clean events for display: hide internal prompt wrappers and raw JSON fences
  // (structured suggestions render in our dedicated confirmation push card instead).
  const displayObserverEvents = useMemo(
    () => sanitizeObserverEventsForDisplay(observerSession.events),
    [observerSession.events],
  )

  // Keep latest structured suggestion synced from the raw Observer session transcript.
  const latestAssistant = useMemo(
    () => extractLatestAssistantMessage(observerSession.events),
    [observerSession.events],
  )
  useEffect(() => {
    if (!latestAssistant || observerRunning) return
    const parsed = parseObserverSuggestion(latestAssistant.text, latestAssistant.seq)
    if (!parsed || !parsed.shouldInterject) return
    setActiveSuggestion(prev => {
      if (prev?.id === parsed.id) return prev
      setDecisionText(parsed.interject)
      return parsed
    })
  }, [latestAssistant, observerRunning])

  const providerId = prefs.model?.providerId ?? settings?.orchestrator_provider ?? ''
  const modelId = prefs.model?.modelId ?? settings?.orchestrator_model ?? ''
  const shownEffort = prefs.reasoningEffort !== undefined
    ? prefs.reasoningEffort
    : (observerTask?.reasoning_effort ?? null)
  const shownFast = prefs.fastMode !== undefined
    ? prefs.fastMode
    : (observerTask?.fast_mode ?? false)

  const createOrReuseObserverSession = useCallback(async (): Promise<string> => {
    if (observerTaskId && !observerSession.error) return observerTaskId
    const id = await api.createSession(workspaceRoot || undefined, 'observer', watchedTaskId)
    setObserverSessionId(watchedTaskId, id)
    // Set title while keeping the observer session archived and isolated from the main sidebar.
    void api.updateSession(id, {
      title: `${OBSERVER_TITLE_PREFIX}${watchedTitle.slice(0, 36)}`,
      archived: true,
    }).catch(() => {})
    return id
  }, [api, observerTaskId, observerSession.error, workspaceRoot, watchedTaskId, watchedTitle])

  const triggerInspect = useCallback(async (trigger: ObserverTrigger) => {
    if (sending || observerRunning) return
    setSending(true)
    setError('')
    setLastTriggerLabel(trigger.label)
    try {
      const id = await createOrReuseObserverSession()
      const prompt = buildObserverInspectPrompt({
        watchedTaskId,
        watchedTitle,
        watchedStatus: watchedTask.status,
        workspaceRoot: workspaceRoot || undefined,
        trigger,
      })
      await api.prompt(id, prompt, {
        permission_mode: 'read_only',
        requestId: randomId(),
        ...(modelId ? { provider: providerId, model: modelId } : {}),
        ...(shownEffort ? { reasoning_effort: shownEffort } : {}),
        fast_mode: shownFast,
      })
      lastInspectedSeqRef.current = Math.max(lastInspectedSeqRef.current, trigger.latestSeq)
      if (trigger.kind !== 'manual') lastAutoInspectAtRef.current = Date.now()
      // Only attach `observerTaskId` AFTER `api.prompt` transitions the task from
      // `draft` to `running`, so `useSession` opens the live SSE stream on first load.
      if (id === observerTaskId) observerSession.reload()
      else setObserverTaskId(id)
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err)
      setError(msg)
      if (msg.toLowerCase().includes('still running')) observerSession.reload()
    } finally {
      setSending(false)
    }
  }, [
    api,
    createOrReuseObserverSession,
    modelId,
    observerRunning,
    observerSession,
    observerTaskId,
    providerId,
    sending,
    shownEffort,
    shownFast,
    watchedTask.status,
    watchedTaskId,
    watchedTitle,
    workspaceRoot,
  ])

  const triggerInspectRef = useRef(triggerInspect)
  triggerInspectRef.current = triggerInspect

  // Automatic milestone / failure watcher (ONLY active when prefs.enabled && prefs.autoInspect).
  // Never auto-inspect when the user or Observer cancelled/aborted the main session.
  useEffect(() => {
    const latestSeq = watchedEvents.at(-1)?.seq ?? 0
    if (watchedTask.status === 'cancelled' || watchedTask.status === 'cancelling') {
      lastInspectedSeqRef.current = Math.max(lastInspectedSeqRef.current, latestSeq)
      return
    }
    if (!prefs.enabled || !prefs.autoInspect) return
    if (sending || observerRunning) return
    const trigger = detectObserverTrigger(watchedEvents, lastInspectedSeqRef.current, watchedRunning)
    if (!trigger) {
      // Advance baseline past aborted/cancelled turn/end events so they never re-trigger later.
      const sawCancel = watchedEvents.some(ev => {
        if (ev.seq <= lastInspectedSeqRef.current || ev.type !== 'turn/end') return false
        const reason = ev.data.reason as Record<string, unknown> | undefined
        return reason?.kind === 'aborted' || reason?.kind === 'cancelled'
      })
      if (sawCancel) lastInspectedSeqRef.current = Math.max(lastInspectedSeqRef.current, latestSeq)
      return
    }
    const elapsed = Date.now() - lastAutoInspectAtRef.current
    if (trigger.kind !== 'turn_failed' && elapsed < OBSERVER_COOLDOWN_MS) return
    void triggerInspect(trigger)
  }, [prefs.enabled, prefs.autoInspect, watchedEvents, watchedRunning, watchedTask.status, sending, observerRunning, triggerInspect])

  // Zero-cost local silent-stall watchdog: wakes the Observer AT MOST ONCE per in-flight callId
  // when the main session is running with 0 new events for >= 3 minutes (never loops).
  useEffect(() => {
    if (!prefs.enabled || !prefs.autoInspect || !watchedRunning) return
    const timer = setInterval(() => {
      if (observerBusyRef.current) return
      const events = watchedEventsRef.current
      const stalled = findStalledInFlightTool(events, Date.now(), OBSERVER_STALL_THRESHOLD_MS)
      if (!stalled) return
      if (watchdogFiredCallIdsRef.current.has(stalled.callId)) return
      watchdogFiredCallIdsRef.current.add(stalled.callId)
      const latestSeq = events.at(-1)?.seq ?? stalled.seq
      void triggerInspectRef.current({
        kind: 'stalled_tool',
        label: '疑似卡死看门狗（单次唤醒）',
        latestSeq,
        detail: `${stalled.toolName} 已静默 ${stalled.idleSec}s 无新事件（callId: ${stalled.callId}）`,
      })
    }, 15_000)
    return () => clearInterval(timer)
  }, [prefs.enabled, prefs.autoInspect, watchedRunning])

  // Send a user message directly to this session's Observer AI.
  const sendObserverChat = useCallback(async () => {
    const text = draft.trim()
    if (!text || sending || observerRunning) return
    setSending(true)
    setError('')
    try {
      const id = await createOrReuseObserverSession()
      const isFirstMessage = id !== observerTaskId || observerSession.events.length === 0
      const prompt = buildObserverUserChatPrompt({
        watchedTaskId,
        watchedStatus: watchedTask.status,
        userText: text,
        isFirstMessage,
      })
      await api.prompt(id, prompt, {
        permission_mode: 'read_only',
        requestId: randomId(),
        ...(modelId ? { provider: providerId, model: modelId } : {}),
        ...(shownEffort ? { reasoning_effort: shownEffort } : {}),
        fast_mode: shownFast,
      })
      setDraft('')
      if (id === observerTaskId) observerSession.reload()
      else setObserverTaskId(id)
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err)
      setError(msg)
      if (msg.toLowerCase().includes('still running')) observerSession.reload()
    } finally {
      setSending(false)
    }
  }, [
    api,
    createOrReuseObserverSession,
    draft,
    modelId,
    observerRunning,
    observerSession,
    observerTaskId,
    providerId,
    sending,
    shownEffort,
    shownFast,
    watchedTask.status,
    watchedTaskId,
  ])

  const markSuggestionHandled = useCallback((id: string) => {
    setDismissedIds(prev => {
      const next = new Set(prev)
      next.add(id)
      saveDismissedSuggestionIds(next)
      return next
    })
    setPopupDismissedId(id)
  }, [])

  // Confirmed decision: Interject into running main session, OR send as follow-up if idle/failed.
  const executeInterject = useCallback(async (textOverride?: string) => {
    const text = (textOverride ?? decisionText).trim()
    if (!text || executing) return
    setExecuting(true)
    setError('')
    try {
      if (watchedRunning) {
        await api.interject(watchedTaskId, text)
        showToast('已确认并插嘴到主会话')
      } else {
        await api.prompt(watchedTaskId, text, { requestId: randomId() })
        showToast('主会话当前未运行，已作为新指令发送给主会话')
      }
      if (activeSuggestion) markSuggestionHandled(activeSuggestion.id)
      onMainSessionUpdated()
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setExecuting(false)
    }
  }, [activeSuggestion, api, decisionText, executing, markSuggestionHandled, onMainSessionUpdated, showToast, watchedRunning, watchedTaskId])

  // Confirmed decision: Stop the running main session.
  const executeStopMain = useCallback(async () => {
    if (!watchedRunning || executing) return
    setExecuting(true)
    setError('')
    try {
      await api.cancel(watchedTaskId)
      const latestSeq = watchedEventsRef.current.at(-1)?.seq ?? 0
      lastInspectedSeqRef.current = Math.max(lastInspectedSeqRef.current, latestSeq)
      if (activeSuggestion) markSuggestionHandled(activeSuggestion.id)
      onMainSessionUpdated()
      showToast('已向主会话发出停止指令')
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setExecuting(false)
    }
  }, [activeSuggestion, api, executing, markSuggestionHandled, onMainSessionUpdated, showToast, watchedRunning, watchedTaskId])

  // Confirmed decision: Stop derailed main run, wait briefly for terminal state, then send corrected prompt.
  const executeStopAndContinue = useCallback(async () => {
    const text = decisionText.trim()
    if (executing) return
    setExecuting(true)
    setError('')
    try {
      if (watchedRunning) {
        await api.cancel(watchedTaskId)
        for (let attempt = 0; attempt < 15; attempt++) {
          await new Promise(resolve => setTimeout(resolve, 300))
          const latest = await api.getTask(watchedTaskId).catch(() => null)
          if (latest && latest.status !== 'running' && latest.status !== 'cancelling') break
        }
        const latestSeq = watchedEventsRef.current.at(-1)?.seq ?? 0
        lastInspectedSeqRef.current = Math.max(lastInspectedSeqRef.current, latestSeq)
      }
      if (text) {
        await api.prompt(watchedTaskId, text, { requestId: randomId() })
        showToast('已停止原轮次并发送纠偏方案')
      } else {
        showToast('已停止主会话运行')
      }
      if (activeSuggestion) markSuggestionHandled(activeSuggestion.id)
      onMainSessionUpdated()
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setExecuting(false)
    }
  }, [activeSuggestion, api, decisionText, executing, markSuggestionHandled, onMainSessionUpdated, showToast, watchedRunning, watchedTaskId])

  const resetObserverSession = useCallback(() => {
    setObserverSessionId(watchedTaskId, undefined)
    setObserverTaskId(undefined)
    watchdogFiredCallIdsRef.current.clear()
    setActiveSuggestion(null)
    setDecisionText('')
    setError('')
  }, [watchedTaskId])

  const handleObserverFilePreview = useCallback((path: string, options?: { line?: number; url?: string }) => {
    if (!onFilePreview) return
    onFilePreview(watchedTaskId, path, options, workspaceRoot)
  }, [onFilePreview, watchedTaskId, workspaceRoot])

  const hasUnhandledSuggestion = Boolean(
    activeSuggestion && activeSuggestion.shouldInterject && !dismissedIds.has(activeSuggestion.id),
  )
  const showFloatingPopup = Boolean(
    prefs.enabled && !drawerOpen && hasUnhandledSuggestion && activeSuggestion && popupDismissedId !== activeSuggestion.id,
  )

  const handleToggleEnabled = () => {
    const nextEnabled = !prefs.enabled
    updatePrefs({ enabled: nextEnabled })
    if (nextEnabled) {
      // Advance baseline so simply turning the switch on won't spam old historical errors,
      // while letting the user click "立即巡检" or wait for the next live event.
      lastInspectedSeqRef.current = watchedEvents.at(-1)?.seq ?? 0
      setDrawerOpen(true)
    }
  }

  const startResize = (event: React.PointerEvent<HTMLDivElement>) => {
    startHorizontalResize({
      event,
      startWidth: prefs.width,
      direction: -1,
      min: 320,
      max: () => Math.min(720, Math.max(360, window.innerWidth - 360)),
      onWidth: next => setPrefs(prev => ({ ...prev, width: next })),
      onCommit: next => updatePrefs({ width: next }),
    })
  }

  return (
    <>
      {/* Floating compact launcher pill on the right edge when drawer is collapsed */}
      {!drawerOpen && (
        <div className={css.launcherStack}>
          <button
            type="button"
            className={clsx(
              css.launcherPill,
              prefs.enabled && css.launcherPillOn,
              hasUnhandledSuggestion && css.launcherPillAlert,
            )}
            onClick={() => setDrawerOpen(true)}
            title={prefs.enabled ? '打开蓝图观察者会话（已开启）' : '蓝图观察者（当前关闭，点击展开设置）'}
          >
            <span className={clsx(css.eyeDot, prefs.enabled && css.eyeDotLive, observerRunning && css.eyeDotPulse)} />
            <span>观察者</span>
            <span className={css.launcherState}>
              {observerRunning ? '巡检中…' : prefs.enabled ? (hasUnhandledSuggestion ? '有建议' : '开') : '关'}
            </span>
          </button>
        </div>
      )}

      {/* Floating suggestion notification popup in the main work view */}
      {showFloatingPopup && activeSuggestion && (
        <aside
          className={clsx(
            css.popupCard,
            activeSuggestion.severity === 'blocker' && css.popupBlocker,
            activeSuggestion.severity === 'warn' && css.popupWarn,
          )}
          role="dialog"
          aria-label="观察者建议弹窗"
        >
          <div
            className={css.popupClickableBody}
            onClick={() => setDrawerOpen(true)}
            title="点击展开专属观察者会话，和观察者讨论后再确认执行"
          >
            <div className={css.popupHeader}>
              <span className={clsx(css.severityBadge, css[`sev_${activeSuggestion.severity}`])}>
                {severityLabel(activeSuggestion.severity)}
              </span>
              <strong className={css.popupTitle}>{activeSuggestion.title}</strong>
              <button
                type="button"
                className={css.popupClose}
                aria-label="暂时收起建议弹窗"
                onClick={e => {
                  e.stopPropagation()
                  setPopupDismissedId(activeSuggestion.id)
                }}
              >
                <Icon name="close" size={14} />
              </button>
            </div>
            <p className={css.popupReason}>{activeSuggestion.reason}</p>
            {activeSuggestion.interject && (
              <div className={css.popupPreview}>
                <span className={css.popupPreviewTag}>拟插嘴</span>
                <span>{activeSuggestion.interject}</span>
              </div>
            )}
          </div>
          <div className={css.popupActions}>
            <button
              type="button"
              className={css.popupPrimaryBtn}
              onClick={() => setDrawerOpen(true)}
            >
              打开观察者会话讨论
            </button>
            {activeSuggestion.action === 'stop' ? (
              <button
                type="button"
                className={css.popupDangerBtn}
                disabled={executing || !watchedRunning}
                onClick={() => { void executeStopMain() }}
              >
                停止运行
              </button>
            ) : (
              <button
                type="button"
                className={css.popupConfirmBtn}
                disabled={executing || !activeSuggestion.interject}
                onClick={() => { void executeInterject(activeSuggestion.interject) }}
              >
                {watchedRunning ? '直接插嘴' : '直接发送'}
              </button>
            )}
            <button
              type="button"
              className={css.popupGhostBtn}
              onClick={() => markSuggestionHandled(activeSuggestion.id)}
            >
              忽略
            </button>
          </div>
        </aside>
      )}

      {/* Resizable right-hand Observer Side Chat Drawer (dedicated to watchedTaskId) */}
      {drawerOpen && (
        <section
          className={css.drawer}
          style={{ width: prefs.width }}
          aria-label="蓝图观察者侧边栏"
        >
          <div className={css.resizer} title="拖动调整观察者宽度" onPointerDown={startResize} />

          <header className={css.drawerHeader}>
            <div className={css.headerTitle}>
              <span className={clsx(css.eyeDot, prefs.enabled && css.eyeDotLive, observerRunning && css.eyeDotPulse)} />
              <strong>蓝图观察者</strong>
              <span className={css.readonlyTag}>只读 · 看 DB</span>
            </div>
            <div className={css.headerControls}>
              <button
                type="button"
                role="switch"
                aria-checked={prefs.enabled}
                className={clsx(css.switchBtn, prefs.enabled && css.switchBtnOn)}
                onClick={handleToggleEnabled}
                title={prefs.enabled ? '关闭观察者' : '开启观察者'}
              >
                <span className={css.switchThumb} />
                <span>{prefs.enabled ? '已开启' : '已关闭'}</span>
              </button>
              <button
                type="button"
                className={css.iconBtn}
                title="收起观察者侧边栏"
                aria-label="收起"
                onClick={() => setDrawerOpen(false)}
              >
                <Icon name="close" size={16} />
              </button>
            </div>
          </header>

          {/* Status & control strip */}
          <div className={css.metaBar}>
            <div className={css.metaTarget} title={`正在观察：${watchedTitle} (${watchedTaskId})`}>
              <span className={css.metaLabel}>当前观察</span>
              <span className={css.metaValue}>{watchedTitle}</span>
              <span className={clsx(css.statusPill, watchedRunning && css.statusPillRunning)}>
                {watchedRunning ? '运行中' : watchedTask.status}
              </span>
            </div>
            <div className={css.metaButtons}>
              <button
                type="button"
                className={css.inspectBtn}
                disabled={sending || observerRunning}
                onClick={() => {
                  if (!prefs.enabled) updatePrefs({ enabled: true })
                  void triggerInspect({
                    kind: 'manual',
                    label: '用户手动触发 SQLite 全局巡检',
                    latestSeq: watchedEvents.at(-1)?.seq ?? 0,
                  })
                }}
                title="让观察者立刻读取 SQLite 数据库检查当前会话全局进展"
              >
                {observerRunning ? '正在查库分析…' : '立即巡检 DB'}
              </button>
              <label className={css.autoCheck} title="主会话报错、中断、计划阶段变更、本轮结束或运行中工具静默卡死超 3 分钟时自动巡检（单次唤醒，绝不循环）">
                <input
                  type="checkbox"
                  checked={prefs.autoInspect}
                  disabled={!prefs.enabled}
                  onChange={e => updatePrefs({ autoInspect: e.target.checked })}
                />
                <span>自动巡检</span>
              </label>
              {observerTaskId && (
                <button
                  type="button"
                  className={css.subtleBtn}
                  disabled={sending || observerRunning}
                  onClick={resetObserverSession}
                  title="清空并重新开启本任务的观察者对话"
                >
                  重置
                </button>
              )}
            </div>
            {lastTriggerLabel && (
              <div className={css.triggerNote}>最近触发：{lastTriggerLabel}</div>
            )}
          </div>

          {/* Dedicated Observer Chat Transcript */}
          <div className={css.transcriptWrap}>
            <div ref={transcriptRef} className={css.transcript}>
              {observerTaskId && observerTask && (observerSession.events.length > 0 || observerRunning) ? (
                renderSlot('agent.chat', {
                  events: displayObserverEvents,
                  task: observerTask,
                  running: observerRunning,
                  onOpenTask,
                  onChangesUpdated: observerSession.reload,
                  onFilePreview: handleObserverFilePreview,
                })
              ) : (
                <div className={css.emptyState}>
                  <strong>专属于当前会话的只读观察者</strong>
                  <p>
                    观察者通过 <code>blueprint.inspect_session</code> 一步直读 SQLite 全局摘要与最新 20 条实时事件，并结合只读本地源码核验，绝不修改任何代码。
                  </p>
                  <p>
                    你可以点上方<strong>「立即巡检 DB」</strong>，或者直接在下面和观察者对话；确认方案后，再由你拍板插嘴或停止主会话。
                  </p>
                </div>
              )}
            </div>
            {observerTaskId && observerTask && (observerSession.events.length > 0 || observerRunning) && (
              <ScrollToBottom show={tailHidden} onJump={jumpToTail} />
            )}
          </div>

          {/* AI-proposed confirmation push card — ONLY shown when the Observer actually raises a decision */}
          {hasUnhandledSuggestion && activeSuggestion && (
            <div
              className={css.confirmCard}
              role="region"
              aria-label="观察者待确认方案"
            >
              <div className={css.confirmCardHeader}>
                <span className={clsx(css.confirmEyebrow, css[`eyebrow_${activeSuggestion.severity}`])}>
                  <span className={css.confirmEyebrowDot} aria-hidden="true" />
                  <span>{severityLabel(activeSuggestion.severity)}</span>
                </span>
                <button
                  type="button"
                  className={css.confirmDismissBtn}
                  title="忽略这条建议"
                  aria-label="忽略"
                  onClick={() => {
                    markSuggestionHandled(activeSuggestion.id)
                    setDecisionText('')
                  }}
                >
                  <Icon name="close" size={13} />
                </button>
              </div>

              <div className={css.confirmTextBlock}>
                <strong className={css.confirmCardTitle}>{activeSuggestion.title}</strong>
                <p className={css.confirmCardReason}>{activeSuggestion.reason}</p>
              </div>

              {(activeSuggestion.action !== 'stop' || decisionText.trim()) && (
                <textarea
                  rows={3}
                  className={css.confirmEditInput}
                  value={decisionText}
                  placeholder="可在此直接微调发送给主会话的指令…"
                  onChange={e => setDecisionText(e.target.value)}
                />
              )}

              <div className={css.confirmCardFooter}>
                <div className={css.confirmFooterLeft}>
                  <button
                    type="button"
                    className={css.confirmGhostBtn}
                    onClick={() => {
                      markSuggestionHandled(activeSuggestion.id)
                      setDecisionText('')
                    }}
                  >
                    忽略
                  </button>
                  {activeSuggestion.action !== 'stop' && watchedRunning && (
                    <button
                      type="button"
                      className={css.confirmDangerBtn}
                      disabled={executing}
                      onClick={() => { void executeStopMain() }}
                      title="直接停止当前主会话的运行"
                    >
                      仅停止
                    </button>
                  )}
                </div>

                {activeSuggestion.action === 'stop' ? (
                  <button
                    type="button"
                    className={css.confirmDangerSolidBtn}
                    disabled={executing || !watchedRunning}
                    onClick={() => { void executeStopMain() }}
                  >
                    {watchedRunning ? '确认停止' : '主会话已停止'}
                  </button>
                ) : activeSuggestion.action === 'stop_and_continue' ? (
                  <button
                    type="button"
                    className={activeSuggestion.severity === 'blocker' ? css.confirmDangerSolidBtn : css.confirmPrimaryBtn}
                    disabled={executing || (!decisionText.trim() && !watchedRunning)}
                    onClick={() => { void executeStopAndContinue() }}
                  >
                    {actionLabel('stop_and_continue', watchedRunning)}
                  </button>
                ) : (
                  <button
                    type="button"
                    className={css.confirmPrimaryBtn}
                    disabled={executing || !decisionText.trim()}
                    onClick={() => { void executeInterject() }}
                  >
                    {watchedRunning ? '确认插嘴' : '确认发送'}
                  </button>
                )}
              </div>
            </div>
          )}

          {/* Observer Chat Composer */}
          <footer className={css.composer}>
            <div className={css.inputCard}>
              <textarea
                ref={inputRef}
                rows={2}
                value={draft}
                disabled={sending || observerRunning}
                placeholder="和观察者聊聊：比如“看看它是不是走偏了，给个插嘴方案”…"
                onChange={e => setDraft(e.target.value)}
                onKeyDown={e => {
                  if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                    e.preventDefault()
                    void sendObserverChat()
                  }
                }}
              />
              <div className={css.composerBar}>
                <ModelSelect
                  className={css.modelSelect}
                  providers={settings?.providers ?? []}
                  providerId={providerId || undefined}
                  modelName={modelId}
                  reasoningEffort={shownEffort}
                  fastMode={shownFast}
                  disabled={sending || observerRunning}
                  onSelect={async (provider, id) => {
                    updatePrefs({ model: { providerId: provider, modelId: id } })
                  }}
                  onSetGenerationOptions={(nextEffort, nextFast) => {
                    updatePrefs({ reasoningEffort: nextEffort, fastMode: nextFast })
                  }}
                />
                <span className={css.spacer} />
                {observerRunning ? (
                  <button
                    type="button"
                    className={css.stopObsBtn}
                    onClick={() => { if (observerTaskId) void api.cancel(observerTaskId).then(observerSession.reload, () => {}) }}
                  >
                    <Icon name="stop" size={13} />停止巡检
                  </button>
                ) : (
                  <button
                    type="button"
                    className={css.sendBtn}
                    disabled={!draft.trim() || sending}
                    onClick={() => { void sendObserverChat() }}
                    title="发送给观察者"
                  >
                    <Icon name="send" size={15} />
                  </button>
                )}
              </div>
            </div>
            {error && <p role="alert" className={css.errorText}>{error}</p>}
          </footer>
        </section>
      )}

      {actionToast && (
        <div role="status" className={css.toast}>
          {actionToast}
        </div>
      )}
    </>
  )
}
