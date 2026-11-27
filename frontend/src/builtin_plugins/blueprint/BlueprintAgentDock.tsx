import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import clsx from 'clsx'
import type { SettingsData } from '../../api.ts'
import { useAgentApi } from '../../cordis/react.tsx'
import type { AgentSlotProps } from '../../cordis/slot-contract.ts'
import { startHorizontalResize } from '../../components/dragResize.ts'
import { ParchmentQuillIcon } from '../../components/ParchmentQuillIcon.tsx'
import { ScrollToBottom, transcriptScroller, useScrollToBottom } from '../../components/ScrollToBottom.tsx'
import { Icon } from '../../components/WorkbenchIcon.tsx'
import { ModelSelect } from '../../dsh/model-selection/ModelSelect.tsx'
import { randomId } from '../../randomId.ts'
import { useSession } from '../../useSession.ts'
import css from './BlueprintAgentDock.module.css'

export interface BlueprintAgentCommand {
  readonly id: number
  readonly prompt: string
}

const SESSION_MAP_KEY = 'blueprint_agent_sessions_v1'
const WIDTH_KEY = 'blueprint_agent_width_v1'
const MODEL_KEY = 'blueprint_agent_model_v1'

interface ModelPick {
  readonly providerId: string
  readonly modelId: string
}

function loadWidth(): number {
  try {
    const n = Number(localStorage.getItem(WIDTH_KEY))
    if (Number.isFinite(n) && n >= 320 && n <= 720) return n
  } catch {}
  return 400
}

function loadModel(): ModelPick | undefined {
  try {
    const raw = localStorage.getItem(MODEL_KEY) ?? ''
    const cut = raw.indexOf('/')
    if (cut > 0 && cut < raw.length - 1) {
      return { providerId: raw.slice(0, cut), modelId: raw.slice(cut + 1) }
    }
  } catch {}
  return undefined
}

function loadSessionMap(): Record<string, string> {
  try {
    const raw = localStorage.getItem(SESSION_MAP_KEY)
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

function getBlueprintSessionId(workspace: string): string | undefined {
  return loadSessionMap()[workspace]
}

function setBlueprintSessionId(workspace: string, taskId: string | undefined): void {
  try {
    const map = loadSessionMap()
    if (taskId) map[workspace] = taskId
    else delete map[workspace]
    const trimmed = Object.fromEntries(Object.entries(map).slice(-40))
    localStorage.setItem(SESSION_MAP_KEY, JSON.stringify(trimmed))
  } catch {}
}

function baseName(path: string): string {
  const clean = path.replace(/^\\\\\?\\/, '').replace(/[\\/]+$/, '')
  return clean.split(/[\\/]/).filter(Boolean).pop() || clean || path
}

/**
 * Isolated plugin-owned Agent drawer for the Blueprint workbench (`kind: "blueprint"`).
 *
 * - Runs exclusively in its own archived Blueprint session (never touches or pollutes
 *   the user's main chat sessions or composer input).
 * - Relies on `src/server/builtin_plugins/blueprint/SKILL.md` pinned by the backend
 *   system prompt, so action buttons send only concise 1-line factual triggers.
 */
export function BlueprintAgentDock({
  workspace,
  renderSlot,
  command,
  onFinished,
  onFilePreview,
  onOpenTask,
}: {
  readonly workspace: string
  readonly renderSlot: AgentSlotProps['renderSlot']
  readonly command?: BlueprintAgentCommand | undefined
  readonly onFinished?: (() => void) | undefined
  readonly onFilePreview?: ((taskId: string, path: string, options?: { line?: number; url?: string }, root?: string) => void) | undefined
  readonly onOpenTask?: ((taskId: string) => void) | undefined
}) {
  const api = useAgentApi()
  const [open, setOpen] = useState(false)
  const [width, setWidth] = useState(loadWidth)
  const [taskId, setTaskId] = useState<string | undefined>(() => getBlueprintSessionId(workspace))
  const [settings, setSettings] = useState<SettingsData | undefined>(undefined)
  const [pickedModel, setPickedModel] = useState<ModelPick | undefined>(loadModel)
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const [stopping, setStopping] = useState(false)
  const [error, setError] = useState('')
  const [queuedPrompt, setQueuedPrompt] = useState<string | null>(null)

  const session = useSession(taskId, () => {})
  const task = session.task?.task_id === taskId ? session.task : undefined
  const running = task?.status === 'running' || task?.status === 'cancelling'

  const transcriptRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLTextAreaElement>(null)
  const { away: tailHidden, jump: jumpToTail, following } = useScrollToBottom(transcriptRef, open)
  const handledCommandId = useRef<number>(0)
  const wasRunning = useRef(false)

  useEffect(() => {
    setTaskId(getBlueprintSessionId(workspace))
    setError('')
  }, [workspace])

  useEffect(() => {
    if (!open && !command) return
    if (settings !== undefined) return
    api.getSettings().then(setSettings, () => {})
  }, [api, open, command, settings])

  // Notify Blueprint workbench to refresh canvases/playbooks/inbox once the Blueprint Agent finishes a run.
  useEffect(() => {
    if (wasRunning.current && !running) {
      onFinished?.()
    }
    wasRunning.current = running
  }, [running, onFinished])

  useLayoutEffect(() => {
    if (!open) return
    const host = transcriptRef.current
    if (!host) return
    const box = transcriptScroller(host)
    following.current = true
    const pin = () => { if (following.current) box.scrollTop = box.scrollHeight }
    pin()
    const observer = new ResizeObserver(pin)
    observer.observe(box)
    for (const node of Array.from(box.children)) observer.observe(node)
    return () => observer.disconnect()
  }, [open, taskId, following])

  const providerId = pickedModel?.providerId ?? settings?.orchestrator_provider ?? ''
  const modelId = pickedModel?.modelId ?? settings?.orchestrator_model ?? ''

  const createOrReuseSession = useCallback(async (): Promise<string> => {
    if (taskId && !session.error) return taskId
    const id = await api.createSession(workspace || undefined, 'blueprint')
    setBlueprintSessionId(workspace, id)
    void api.updateSession(id, {
      title: `[蓝图] ${baseName(workspace)}`,
      archived: true,
    }).catch(() => {})
    return id
  }, [api, session.error, taskId, workspace])

  const dispatchPrompt = useCallback(async (text: string) => {
    const trimmed = text.trim()
    if (!trimmed || sending || running) return
    setSending(true)
    setError('')
    try {
      const id = await createOrReuseSession()
      await api.prompt(id, trimmed, {
        permission_mode: 'workspace_write',
        requestId: randomId(),
        ...(modelId ? { provider: providerId, model: modelId } : {}),
      })
      if (id === taskId) session.reload()
      else setTaskId(id)
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err)
      setError(msg)
      if (msg.toLowerCase().includes('still running')) session.reload()
    } finally {
      setSending(false)
    }
  }, [api, createOrReuseSession, modelId, providerId, running, sending, session, taskId])

  // Execute action commands directly in Blueprint's own isolated Agent (never prefilled into user chat).
  // If the Blueprint Agent is currently busy, queue the latest command instead of dropping it silently.
  useEffect(() => {
    if (!command || command.id === handledCommandId.current) return
    handledCommandId.current = command.id
    setOpen(true)
    if (sending || running) {
      setQueuedPrompt(command.prompt)
      return
    }
    void dispatchPrompt(command.prompt)
  }, [command, dispatchPrompt, running, sending])

  // Auto-drain queued command once the active run settles.
  useEffect(() => {
    if (!queuedPrompt || sending || running) return
    const next = queuedPrompt
    setQueuedPrompt(null)
    void dispatchPrompt(next)
  }, [queuedPrompt, sending, running, dispatchPrompt])

  const handleSend = () => {
    const text = draft.trim()
    if (!text || sending || running) return
    setDraft('')
    void dispatchPrompt(text)
  }

  const handleStop = async () => {
    if (!taskId || !running || stopping) return
    setStopping(true)
    try {
      await api.cancel(taskId)
      session.reload()
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setStopping(false)
    }
  }

  const handleReset = () => {
    if (running || sending) return
    setBlueprintSessionId(workspace, undefined)
    setTaskId(undefined)
    setError('')
  }

  const handleFilePreview = useCallback((path: string, options?: { line?: number; url?: string }) => {
    if (!onFilePreview || !taskId) return
    onFilePreview(taskId, path, options, workspace)
  }, [onFilePreview, taskId, workspace])

  const startResize = (event: React.PointerEvent<HTMLDivElement>) => {
    startHorizontalResize({
      event,
      startWidth: width,
      direction: -1,
      min: 320,
      max: () => Math.min(720, Math.max(360, window.innerWidth - 360)),
      onWidth: setWidth,
      onCommit: next => {
        setWidth(next)
        try { localStorage.setItem(WIDTH_KEY, String(next)) } catch {}
      },
    })
  }

  if (!open) {
    return (
      <button
        type="button"
        className={clsx(css.launcher, running && css.launcherBusy)}
        onClick={() => setOpen(true)}
        title="打开蓝图专属 AI 执行面板（独立于用户主会话，基于蓝图 SKILL.md 运行）"
      >
        <ParchmentQuillIcon size={15} variant="line" />
        <span>{running ? '蓝图执行中…' : '蓝图 AI'}</span>
      </button>
    )
  }

  return (
    <aside className={css.dock} style={{ width }} aria-label="蓝图专属 AI 助手">
      <div className={css.resizer} title="拖动调整蓝图助手宽度" onPointerDown={startResize} />
      <header className={css.header}>
        <div className={css.headerTitle}>
          <span className={css.headerMark}>
            <ParchmentQuillIcon size={15} variant="line" />
          </span>
          <strong>蓝图助手</strong>
          <span className={css.skillBadge} title="由 blueprint/SKILL.md 驱动，与用户普通会话隔离">SKILL.md</span>
          {running && <span className={css.runningPill}>执行中</span>}
        </div>
        <div className={css.headerActions}>
          {taskId && (
            <button
              type="button"
              className={css.subtleBtn}
              disabled={running || sending}
              onClick={handleReset}
              title="开启全新的蓝图维护会话"
            >
              重置
            </button>
          )}
          <button
            type="button"
            className={css.iconBtn}
            aria-label="收起蓝图助手"
            title="收起蓝图助手"
            onClick={() => setOpen(false)}
          >
            <Icon name="close" size={15} />
          </button>
        </div>
      </header>

      <div className={css.metaStrip}>
        <span className={css.metaLabel}>项目</span>
        <span className={css.metaValue} title={workspace}>{baseName(workspace)}</span>
        <span className={css.metaNote}>独立蓝图会话 · 不干扰主对话</span>
      </div>

      {queuedPrompt && (
        <div role="status" className={css.queueBanner}>
          <span>已排队 1 条指令，当前任务结束后自动执行</span>
          <button type="button" onClick={() => setQueuedPrompt(null)} aria-label="取消排队">取消</button>
        </div>
      )}

      {error && (
        <div role="alert" className={css.errorBanner}>
          <span>{error}</span>
          <button type="button" onClick={() => setError('')} aria-label="关闭错误">×</button>
        </div>
      )}

      <div className={css.transcriptWrap}>
        <div ref={transcriptRef} className={css.transcript}>
          {taskId && task && (session.events.length > 0 || running) ? (
            renderSlot('agent.chat', {
              events: session.events,
              task,
              running,
              onOpenTask: onOpenTask ?? (() => {}),
              onChangesUpdated: session.reload,
              onFilePreview: handleFilePreview,
            })
          ) : (
            <div className={css.empty}>
              <ParchmentQuillIcon size={40} variant="emblem" />
              <strong>蓝图专属 Agent（SKILL.md 驱动）</strong>
              <p>
                左侧点击「启动分析」「增量同步」「从会话提炼」或「交由蓝图 AI 整理合入」时，会直接在这里独立执行，不会往你的普通聊天会话里插消息。
              </p>
            </div>
          )}
        </div>
        {taskId && task && (session.events.length > 0 || running) && (
          <ScrollToBottom show={tailHidden} onJump={jumpToTail} />
        )}
      </div>

      <footer className={css.composer}>
        <textarea
          ref={inputRef}
          rows={2}
          className={css.textarea}
          value={draft}
          placeholder="直接吩咐蓝图助手（例如：把登录鉴权链补充到画布里）…"
          onChange={e => setDraft(e.target.value)}
          onKeyDown={e => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault()
              handleSend()
            }
          }}
        />
        <div className={css.composerBar}>
          <ModelSelect
            providers={settings?.providers ?? []}
            providerId={providerId || undefined}
            modelName={modelId}
            disabled={sending || running}
            onSelect={async (nextProvider, nextModel) => {
              setPickedModel({ providerId: nextProvider, modelId: nextModel })
              try { localStorage.setItem(MODEL_KEY, `${nextProvider}/${nextModel}`) } catch {}
            }}
          />
          <div className={css.spacer} />
          {running ? (
            <button
              type="button"
              className={css.stopBtn}
              disabled={stopping}
              onClick={() => { void handleStop() }}
            >
              {stopping ? '停止中…' : '停止'}
            </button>
          ) : (
            <button
              type="button"
              className={css.sendBtn}
              disabled={sending || !draft.trim()}
              onClick={handleSend}
            >
              发送
            </button>
          )}
        </div>
      </footer>
    </aside>
  )
}
