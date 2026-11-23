import { randomId } from '../randomId.ts'
import { extractClipboardFiles, isPasteShortcut, readFallbackClipboardImages } from '../clipboardPaste.ts'
import {AttachmentRail,useAttachmentDraft,type AttachmentSubmission} from './Attachments.tsx'
import { PREFILL_EVENT, recentPrefill } from './RewindControl.tsx'
import { draftFor, draftOf, keepDraft, type HeldDraft } from '../composerDraft.ts'
import { composerHistory, historyDirection, isCompositionKey, stepHistoryDown, stepHistoryUp, type HistoryStep } from '../composerHistory.ts'
import { ContextRing } from './ContextRing.tsx'
import type { ContextUsage } from './contextUsage.ts'
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import {
  IconChevronDownOutlineRegular,
  IconFolderOpenRegular,
  IconFolderCloseRegular,
  IconPlusOutlineRegular,
  RiskConfirmation,
  Tooltip,
} from '@deepseek-ai/dsh-client-ui-primitives'
import { Menu } from '../dsh/ui-primitives/Menu.tsx'
import { ModelSelect } from '../dsh/model-selection/ModelSelect.tsx'
import { IconPaperclipOutlineMedium } from '../dsh/ui-primitives/icons/index.tsx'
import heroShellCss from '../dsh/conversation/HeroShell.module.css'
import type { SettingsData } from '../api.ts'
import { useAgentApi } from '../cordis/react.tsx'
import { t } from '../i18n.ts'
import type { SettingsTab } from '../useAgentWorkspace.ts'
import { workspaceName } from './Sidebar.tsx'
import local from './Composer.module.css'
import codexCss from './CodexComposer.module.css'

const STOP_ICON = (
  <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden>
    <rect x="3" y="3" width="10" height="10" rx="3" fill="currentColor" />
  </svg>
)

interface ActiveToken {
  kind: '@' | '/'
  query: string
  start: number
}

interface Suggestion {
  id: string
  label: string
  detail: string
  insert: string
}

function activeToken(value: string, caret: number): ActiveToken | null {
  const match = /(^|\s)([@/])([^\s]*)$/.exec(value.slice(0, caret))
  const kind = match?.[2]
  if (match === null || (kind !== '@' && kind !== '/')) return null
  return { kind, query: match[3] ?? '', start: match.index + (match[1]?.length ?? 0) }
}

function mentionInsert(path: string): string {
  return path.includes(' ') ? `@"${path}" ` : `@${path} `
}

export type DshPermissionMode = 'read_only' | 'workspace_write' | 'ask' | 'auto_review' | 'full_access'

/**
 * The box's text, kept per session. A reader who leaves a half-written message
 * to go and read another conversation finds it again when they come back, and
 * going somewhere else never carries the old text along.
 */
function useDraft(key: string): [string, (next: string, at?: string) => void] {
  const [held, setHeld] = useState<HeldDraft>(() => ({ key, text: draftOf(key) }))
  // A ref keeps the setter stable (event handlers and pending sends hold on to
  // it) and pointed at the session that is open now.
  const session = useRef(key)
  session.current = key
  /**
   * Writes go to the session they were meant for: a send that resolves after
   * the reader has moved to another chat clears its own box, not the one now
   * on screen.
   */
  const setDraft = useCallback((next: string, at: string = session.current) => {
    keepDraft(at, next)
    if (at === session.current) setHeld({ key: at, text: next })
  }, [])
  return [draftFor(key, held).text, setDraft]
}

const FULL_ACCESS_ICON = <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.6" strokeLinejoin="round" aria-hidden="true"><path d="M9 1.5 3.5 9H8l-1 5.5L12.5 7H8l1-5.5Z" /></svg>
// One shield for the levels that gate the agent: only the mark inside changes, so the
// picker reads "asks first", "checks first" and "never asks" at a glance.
const PERMISSION_SHIELD =
  'M6.59624 2.14853C7.50155 1.80917 8.49914 1.80919 9.40444 2.14859L13.9245 3.84317V7.11961C13.9245 11.6089 10.5565 13.5975 8.00035 14.5779C5.44423 13.5975 2.07544 11.6089 2.07544 7.11961V3.84317L6.59624 2.14853Z'

// Ask first: a question mark, sized to stay legible at 14px.
const ASK_ICON = (
  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d={PERMISSION_SHIELD} />
    <path d="M6.5 6.2a1.5 1.5 0 1 1 2.46 1.15c-.45.42-.96.6-.96.85" strokeWidth="1.5" />
    <circle cx="8" cy="10.35" r="0.66" fill="currentColor" stroke="none" />
  </svg>
)

// Reviewed by the AI first: the same shield, marked with a check once it approves.
const AUTO_REVIEW_ICON = (
  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d={PERMISSION_SHIELD} />
    <path d="M5.55 8.2 7.32 9.97 10.45 6.15" strokeWidth="1.7" />
  </svg>
)

export function Composer({
  variant,
  disabled,
  note,
  running,
  workspacePath = '',
  workspaces = [],
  activeWorkspace,
  onSelectWorkspace,
  onOpenAddWorkspace,
  modelName = '',
  modelProviderId,
  reasoningEffort,
  fastMode = false,
  settings,
  onSelectModel,
  onSetGenerationOptions,
  onOpenSettings,
  onSubmit,
  onStop,
  onInterject,
  contextUsage,
  permissionMode,
  onSelectPermissionMode,
  draftKey = 'session',
}: {
  variant: 'hero' | 'composer'
  /** Which session's draft this box shows: leaving and coming back keeps the text. */
  draftKey?: string | undefined
  disabled: boolean
  note: string | undefined
  running: boolean
  workspacePath?: string | undefined
  workspaces?: readonly string[] | undefined
  activeWorkspace?: string | undefined
  onSelectWorkspace?: ((ws: string) => void) | undefined
  onOpenAddWorkspace?: (() => void) | undefined
  modelName?: string | undefined
  /** The provider of the selected model; several providers can offer the same model id. */
  modelProviderId?: string | undefined
  reasoningEffort?: string | null | undefined
  fastMode?: boolean | undefined
  settings?: SettingsData | undefined
  onSelectModel?: ((providerId: string, modelId: string) => Promise<void>) | undefined
  onSetGenerationOptions?: ((reasoningEffort: string, fastMode: boolean) => Promise<void>) | undefined
  onOpenSettings?: ((tab?: SettingsTab) => void) | undefined
  /** The open chat's own permission level; a new chat starts from the default. */
  permissionMode: DshPermissionMode
  /** Choosing a level saves it for this chat at once. */
  onSelectPermissionMode: (mode: DshPermissionMode) => void
  onSubmit: (text: string, permissionMode: DshPermissionMode, attachment?: AttachmentSubmission) => Promise<boolean>
  onStop: (() => void) | undefined
  /** Deliver text into the running turn; rejects with NOT_RUNNING once it has ended. */
  onInterject?: ((text: string) => Promise<void>) | undefined
  contextUsage?: ContextUsage | undefined
}) {
  const [savedDraft, setDraft] = useDraft(draftKey)
  // Recalled text is only a preview; the underlying session draft stays intact.
  const [browsing, setBrowsing] = useState<{ key: string; step: HistoryStep } | null>(null)
  const activeHistory = browsing?.key === draftKey ? browsing.step : undefined
  const draft = activeHistory?.text ?? savedDraft
  const historyCaret = useRef<{ key: string; position: number } | null>(null)
  useLayoutEffect(() => { setBrowsing(null) }, [draftKey])
  const recordSent = useCallback((text: string, from: string) => {
    composerHistory.record(text)
    setBrowsing(current => current?.key === from ? null : current)
  }, [])
  const attachments=useAttachmentDraft(draftKey)
  const fileInput=useRef<HTMLInputElement>(null)
  const uploadAbort=useRef<AbortController|null>(null)
  const requestId=useRef(randomId())
  const [uploadLabel,setUploadLabel]=useState('')
  useEffect(()=>{requestId.current=randomId()},[draft,attachments.items])
  useEffect(()=>()=>uploadAbort.current?.abort(),[])

  const [submitting, setSubmitting] = useState(false)
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false)
  const [modelMenuOpen, setModelMenuOpen] = useState(false)
  const workspaceAnchorRef = useRef<HTMLButtonElement>(null)
  const [permMenuOpen, setPermMenuOpen] = useState(false)
  const [permMenuPos, setPermMenuPos] = useState<{ top?: number; bottom?: number; left: number }>({ left: 16 })
  const permTriggerRef = useRef<HTMLButtonElement>(null)
  const permMenuRef = useRef<HTMLDivElement>(null)
  const [showRiskConfirm, setShowRiskConfirm] = useState(false)
  const [riskAcknowledged, setRiskAcknowledged] = useState(false)
  const api = useAgentApi()
  const [caret, setCaret] = useState(0)
  const [commands, setCommands] = useState<readonly { name: string; description: string }[]>([])
  const [files, setFiles] = useState<readonly { path: string; kind: 'file' | 'dir' }[]>([])
  const [menuIndex, setMenuIndex] = useState(0)
  const [dismissedKey, setDismissedKey] = useState('')
  const inputRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    if (variant === 'hero' && !disabled) inputRef.current?.focus()
  }, [variant, disabled])

  // A rewind hands the removed message back for editing.
  useEffect(() => {
    const prefill = () => {
      const text = recentPrefill()
      if (text === undefined) return
      setBrowsing(null)
      setDraft(text)
      window.setTimeout(() => inputRef.current?.focus(), 0)
    }
    prefill()
    window.addEventListener(PREFILL_EVENT, prefill)
    return () => window.removeEventListener(PREFILL_EVENT, prefill)
  }, [])
  // While a turn runs, text can still go out: into the turn (interject) or after it (queue).
  const [queued, setQueued] = useState<readonly string[]>([])
  const midTurn = running && onInterject !== undefined
  const canSend = !disabled && !submitting && (midTurn
    ? draft.trim() !== '' && attachments.items.length === 0
    : !running && (draft.trim() !== '' || attachments.items.length>0))
  useEffect(() => {
    if (running || disabled || submitting || queued.length === 0) return
    const [next, ...rest] = queued
    setQueued(rest)
    void onSubmit(next!, permissionMode).then(sent => {
      if (sent) recordSent(next!, draftKey)
      else setQueued(current => [next!, ...current])
    })
  }, [running, disabled, submitting, queued, onSubmit, permissionMode, recordSent, draftKey])
  const token = useMemo(() => activeHistory === undefined ? activeToken(draft, caret) : null, [activeHistory, draft, caret])
  const tokenKey = token === null ? '' : `${token.kind}:${token.start}:${token.query}`

  useEffect(() => {
    api.commands().then(
      (body) => { setCommands(body.commands) },
      () => {},
    )
  }, [api])

  useEffect(() => {
    if (token?.kind !== '@') return
    const query = token.query
    const timer = window.setTimeout(() => {
      api.files(query).then(
        (body) => { setFiles(body.files) },
        () => { setFiles([]) },
      )
    }, 120)
    return () => { window.clearTimeout(timer) }
  }, [api, token?.kind, token?.query])

  const suggestions = useMemo((): readonly Suggestion[] => {
    if (token?.kind === '@') {
      return files.map((file) => ({
        id: file.path,
        label: file.path,
        detail: file.kind === 'dir' ? '目录' : '文件',
        insert: mentionInsert(file.path),
      }))
    }
    if (token?.kind === '/') {
      const query = token.query.toLowerCase()
      return commands
        .filter((command) => query === '' || command.name.toLowerCase().includes(query))
        .slice(0, 20)
        .map((command) => ({
          id: command.name,
          label: `/${command.name}`,
          detail: command.description,
          insert: `/${command.name} `,
        }))
    }
    return []
  }, [token, files, commands])
  const menuOpen = token !== null && dismissedKey !== tokenKey && suggestions.length > 0
  const activeIndex = Math.min(menuIndex, Math.max(suggestions.length - 1, 0))

  const syncCaret = () => { setCaret(inputRef.current?.selectionStart ?? draft.length) }

  const applySuggestion = (suggestion: Suggestion) => {
    if (token === null) return
    const next = `${draft.slice(0, token.start)}${suggestion.insert}${draft.slice(caret)}`
    const pos = token.start + suggestion.insert.length
    setBrowsing(null)
    setDraft(next)
    setMenuIndex(0)
    window.requestAnimationFrame(() => {
      inputRef.current?.focus()
      inputRef.current?.setSelectionRange(pos, pos)
      setCaret(pos)
    })
  }

  useLayoutEffect(() => {
    const input = inputRef.current
    if (input === null) return
    input.style.height = 'auto'
    input.style.height = `${input.scrollHeight}px`
  }, [draft])

  useLayoutEffect(() => {
    const pending = historyCaret.current
    historyCaret.current = null
    if (pending?.key === draftKey) inputRef.current?.setSelectionRange(pending.position, pending.position)
  }, [draft, draftKey, browsing])

  useEffect(() => {
    if (!permMenuOpen) return
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null
      if (
        target !== null
        && !permTriggerRef.current?.contains(target)
        && !permMenuRef.current?.contains(target)
      ) {
        setPermMenuOpen(false)
      }
    }
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === 'Escape') setPermMenuOpen(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    window.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [permMenuOpen])

  const togglePermDropdown = () => {
    if (permMenuOpen) {
      setPermMenuOpen(false)
      return
    }
    const rect = permTriggerRef.current?.getBoundingClientRect()
    if (rect !== undefined) {
      const left = Math.max(16, rect.left)
      if (rect.top > 320) {
        setPermMenuPos({ bottom: window.innerHeight - rect.top + 8, left })
      } else {
        setPermMenuPos({ top: rect.bottom + 8, left })
      }
    }
    setPermMenuOpen(true)
  }

  const selectPermissionMode = (mode: DshPermissionMode) => {
    setPermMenuOpen(false)
    // Saved for this chat straight away, not when the next message is sent.
    onSelectPermissionMode(mode)
  }

  const handleSelectPermission = (mode: DshPermissionMode) => {
    setPermMenuOpen(false)
    if (mode === 'full_access' && permissionMode !== 'full_access') {
      setRiskAcknowledged(false)
      setShowRiskConfirm(true)
      return
    }
    selectPermissionMode(mode)
  }


  const submit = async (mode: 'interject' | 'queue' = 'interject') => {
    if (!canSend) return
    const text = draft.trim()
    if (midTurn) {
      setBrowsing(null)
      setDraft('')
      if (mode === 'queue') { setQueued(current => [...current, text]); return }
      try {
        await onInterject!(text)
        recordSent(text, draftKey)
      } catch {
        // The turn ended (or the request failed) before delivery: send it next instead of losing it.
        setQueued(current => [...current, text])
      }
      return
    }
    setSubmitting(true)
    // The box this text came from, which is not the box on screen if the reader
    // has gone to look at another chat while the upload ran.
    const sentFrom = draftKey
    try {
      const controller=new AbortController();uploadAbort.current=controller
      if (await onSubmit(text, permissionMode, { files: attachments.items.map(x => x.file), signal: controller.signal, requestId: requestId.current, onProgress: setUploadLabel })) {
        recordSent(text, sentFrom)
        setDraft('', sentFrom)
        attachments.clear(sentFrom)
      }
    } finally {
      setSubmitting(false);setUploadLabel('');uploadAbort.current=null
    }
  }

  const pasteEventSeen = useRef(false)
  const fallbackPasteTimer = useRef<number | null>(null)
  useEffect(() => () => {
    if (fallbackPasteTimer.current !== null) window.clearTimeout(fallbackPasteTimer.current)
  }, [])

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (isPasteShortcut(event) && !disabled && !running && !submitting) {
      pasteEventSeen.current = false
      if (fallbackPasteTimer.current !== null) window.clearTimeout(fallbackPasteTimer.current)
      fallbackPasteTimer.current = window.setTimeout(() => {
        fallbackPasteTimer.current = null
        if (pasteEventSeen.current) return
        void readFallbackClipboardImages().then(files => {
          if (files.length) attachments.add(files)
        })
      }, 40)
    }
    if (isCompositionKey(event.nativeEvent)) return
    if (menuOpen) {
      if (event.key === 'ArrowDown') {
        event.preventDefault()
        setMenuIndex((index) => (index + 1) % suggestions.length)
        return
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault()
        setMenuIndex((index) => (index - 1 + suggestions.length) % suggestions.length)
        return
      }
      if (event.key === 'Escape') {
        event.preventDefault()
        setDismissedKey(tokenKey)
        return
      }
      if (event.key === 'Enter' || event.key === 'Tab') {
        event.preventDefault()
        const suggestion = suggestions[activeIndex]
        if (suggestion !== undefined) applySuggestion(suggestion)
        return
      }
    }
    const input = event.currentTarget
    const overlayOpen = menuOpen || workspaceMenuOpen || permMenuOpen || modelMenuOpen || showRiskConfirm
    const cursor = activeHistory?.cursor ?? null
    const direction = event.defaultPrevented ? null : historyDirection(event.nativeEvent, input, cursor !== null, overlayOpen)
    if (direction !== null) {
      const next = direction === 'up'
        ? stepHistoryUp(cursor?.entries ?? composerHistory.read(), cursor, { text: draft, caret: input.selectionStart })
        : stepHistoryDown(cursor)
      if (next !== null) {
        event.preventDefault()
        historyCaret.current = { key: draftKey, position: next.caret }
        setCaret(next.caret)
        setBrowsing(next.cursor === null ? null : { key: draftKey, step: next })
        return
      }
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      void submit()
    }
  }

  const wsName = workspaceName(activeWorkspace || workspacePath || '')
  const placeholder = t(variant === 'hero' ? 'placeholder.hero' : 'placeholder.default')

  return (
    <div className={clsx(codexCss.root, local.anchor, variant === 'hero' && codexCss.hero)}>
      {note !== undefined && (
        <div
          className={codexCss.notice}
          role="status"
          style={{ cursor: onOpenSettings ? 'pointer' : undefined }}
          onClick={() => { onOpenSettings?.('models') }}
        >
          {note}
        </div>
      )}

      {/* Outer Composer Container */}
      <div className={codexCss.composerBox}>
        {/* DSH Workspace Row (Sits 12px above the card in hero mode) */}
        {variant === 'hero' && (
          <div className={heroShellCss.workspaceRow} style={{ marginBottom: 12, paddingLeft: 8 }}>
            <button
              ref={workspaceAnchorRef}
              type="button"
              className={heroShellCss.workspace}
              aria-label="选择工作区"
              aria-haspopup="menu"
              aria-expanded={workspaceMenuOpen}
              onClick={() => setWorkspaceMenuOpen(open => !open)}
            >
              <IconFolderOpenRegular className={heroShellCss.folder} size={16} />
              <span className={heroShellCss.workspaceLabel}>{wsName || '选择项目'}</span>
              <IconChevronDownOutlineRegular className={heroShellCss.chevron} size={12} />
            </button>

            <Menu
              open={workspaceMenuOpen}
              onClose={() => setWorkspaceMenuOpen(false)}
              anchor={null}
              portal
              getAnchorRect={() => workspaceAnchorRef.current?.getBoundingClientRect() ?? null}
              items={workspaces.map(ws => ({
                id: ws,
                label: workspaceName(ws),
                icon: <IconFolderCloseRegular size={16} />,
              }))}
              selectedId={activeWorkspace || workspacePath}
              onSelect={(id) => {
                if (id === '::add_workspace') {
                  setWorkspaceMenuOpen(false)
                  onOpenAddWorkspace?.()
                } else {
                  setWorkspaceMenuOpen(false)
                  onSelectWorkspace?.(id)
                }
              }}
              footer={[
                {
                  id: '::add_workspace',
                  label: '选择文件夹 / 添加工作区...',
                  icon: <IconPlusOutlineRegular size={16} />,
                },
              ]}
            />
          </div>
        )}

        {/* Layer 2: White Floating Input Card */}
        <div className={codexCss.card} data-composer-card onDragOver={event=>{if(event.dataTransfer.types.includes('Files'))event.preventDefault()}} onDrop={event=>{event.preventDefault();if(!disabled&&!running&&!submitting)attachments.add(event.dataTransfer.files)}}>
          <input ref={fileInput} type='file' multiple hidden aria-label='上传附件' onChange={event=>{if(event.target.files)attachments.add(event.target.files);event.target.value=''}}/>
          {queued.length > 0 && <div className={codexCss.queueList} aria-label="排队中的消息">
            {queued.map((text, index) => <div key={index} className={codexCss.queueItem}>
              <span className={codexCss.queueTag}>排队中</span>
              <span className={codexCss.queueText} title={text}>{text}</span>
              <button type="button" className={codexCss.queueRemove} aria-label="取消这条排队消息" onClick={() => setQueued(current => current.filter((_, i) => i !== index))}>✕</button>
            </div>)}
          </div>}
          <AttachmentRail items={attachments.items} onRemove={attachments.remove} disabled={submitting}/>
          {attachments.error&&<div role='alert' style={{color:'#b42318',padding:'4px 12px'}}>{attachments.error}</div>}
          {uploadLabel&&<div role='status' style={{padding:'4px 12px',fontSize:12}}>{uploadLabel}</div>}
          <div className={codexCss.inputWrap}>
            <textarea
              ref={inputRef}
              className={codexCss.textarea}
              rows={1}
              value={draft}
              disabled={disabled||submitting}
              onPaste={event => {
                pasteEventSeen.current = true
                if (fallbackPasteTimer.current !== null) {
                  window.clearTimeout(fallbackPasteTimer.current)
                  fallbackPasteTimer.current = null
                }
                const files = extractClipboardFiles(event.clipboardData)
                const plainText = event.clipboardData?.getData('text/plain') ?? ''
                if (files.length && !running) {
                  attachments.add(files)
                  if (!plainText) event.preventDefault()
                } else if (!files.length && !plainText && !running && !disabled && !submitting) {
                  void readFallbackClipboardImages().then(fallbackFiles => {
                    if (fallbackFiles.length) attachments.add(fallbackFiles)
                  })
                }
              }}
              placeholder={midTurn ? '补充或纠正方向，回车插话；也可以排队等这轮结束' : placeholder}
              aria-label={placeholder}
              onChange={(event) => {
                setBrowsing(null)
                setDraft(event.target.value)
                setCaret(event.target.selectionStart)
                setMenuIndex(0)
              }}
              onSelect={syncCaret}
              onKeyUp={syncCaret}
              onClick={syncCaret}
              onKeyDown={onKeyDown}
            />
          </div>

          <div className={codexCss.toolbar}>
            <div className={codexCss.leftControls}>
              <button type='button' title='添加附件（支持拖拽、粘贴图片）' aria-label='添加附件' disabled={disabled||running||submitting} onClick={()=>fileInput.current?.click()} className={codexCss.micBtn}><IconPaperclipOutlineMedium size={16}/></button>

              {/* Bare Plus Button matching media_1791084397752.png */}
              <Tooltip label="添加上下文或文件 (@)" side="top" delayMs={500}>
                <button
                  type="button"
                  className={codexCss.barePlusBtn}
                  title="添加上下文"
                  onClick={() => {
                    const next = draft ? `${draft} @` : '@'
                    setDraft(next)
                    setCaret(next.length)
                    setMenuIndex(0)
                    window.requestAnimationFrame(() => {
                      inputRef.current?.focus()
                      inputRef.current?.setSelectionRange(next.length, next.length)
                    })
                  }}
                >
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeLinejoin="round" strokeWidth="2.2" strokeLinecap="round">
                    <line x1="12" y1="5" x2="12" y2="19" />
                    <line x1="5" y1="12" x2="19" y2="12" />
                  </svg>
                </button>
              </Tooltip>

              {/* DSH Permission Badge with Codex styling (media_1791084397752.png & media_1791085181305.png) */}
              <button
                ref={permTriggerRef}
                type="button"
                className={clsx(
                  codexCss.permPill,
                  permissionMode !== 'ask' && codexCss.permPillNeutral,
                  permissionMode === 'ask' && codexCss.permPillSafe,
                )}
                onClick={togglePermDropdown}
                title="切换工作区操作权限"
              >
                {permissionMode === 'full_access' && (
                  <>
                    {FULL_ACCESS_ICON}
                    <span>完全权限</span>
                  </>
                )}
                {permissionMode === 'ask' && (
                  <>
                    {ASK_ICON}
                    <span>执行前询问</span>
                  </>
                )}
                {permissionMode === 'auto_review' && (
                  <>
                    {AUTO_REVIEW_ICON}
                    <span>自动审批</span>
                  </>
                )}
                {/* Levels the picker no longer offers: a chat saved at one of them keeps
                    showing it, so an older chat is never relabelled as a broader level. */}
                {permissionMode === 'workspace_write' && (
                  <>
                    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6">
                      <path d="M6.4209 1.68067C7.43922 1.299 8.56177 1.29898 9.58008 1.68067L14.0996 3.375C14.2946 3.44811 14.4236 3.63455 14.4238 3.84278V6.89063C14.1115 6.71853 13.7761 6.58312 13.4238 6.48926V4.18946L9.22852 2.61621C8.43657 2.31947 7.56341 2.31939 6.77148 2.61621L2.5752 4.18946V7.11914C2.5752 11.1796 5.52369 13.056 8 14.0391C8.27653 13.9293 8.55827 13.8067 8.8418 13.6729C9.07101 13.9468 9.33228 14.1929 9.62012 14.4053C9.12409 14.6579 8.63578 14.8696 8.17871 15.0449C8.0637 15.0889 7.93628 15.0889 7.82129 15.0449C5.22011 14.0472 1.5752 11.9381 1.5752 7.11914V3.84278C1.57541 3.63469 1.70463 3.44821 1.89941 3.375L6.4209 1.68067Z" fill="currentColor" />
                      <path d="M5.26392 6.60339H10.7361" stroke="currentColor" strokeLinecap="round" />
                      <path d="M5.26392 9.86902H8.32833" stroke="currentColor" strokeLinecap="round" />
                    </svg>
                    <span>工作区内修改</span>
                  </>
                )}
                {permissionMode === 'read_only' && (
                  <>
                    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6" stroke="currentColor">
                      <path d="M5.08545 8.13775L7.18455 10.2368C7.26636 10.3187 7.4003 10.3142 7.47649 10.2271L11.5148 5.61194" strokeLinecap="round" strokeLinejoin="round" />
                      <path d="M6.59624 2.14853C7.50155 1.80917 8.49914 1.80919 9.40444 2.14859L13.9245 3.84317V7.11961C13.9245 11.6089 10.5565 13.5975 8.00035 14.5779C5.44423 13.5975 2.07544 11.6089 2.07544 7.11961V3.84317L6.59624 2.14853Z" strokeLinejoin="round" />
                    </svg>
                    <span>仅可查看</span>
                  </>
                )}
              </button>


            </div>

            <div className={codexCss.rightControls}>
              <ContextRing usage={contextUsage} />
              {/* The same model selector as everywhere else: one pill, one card. */}
              <ModelSelect
                providers={settings?.providers ?? []}
                providerId={modelProviderId ?? settings?.orchestrator_provider ?? undefined}
                modelName={modelName || settings?.orchestrator_model || ''}
                reasoningEffort={reasoningEffort}
                fastMode={fastMode}
                onSelect={onSelectModel}
                onOpenChange={setModelMenuOpen}
                onSetGenerationOptions={onSetGenerationOptions}
                onOpenSettings={onOpenSettings === undefined ? undefined : () => { onOpenSettings('models') }}
                placeholder={t('app.configureModel')}
              />

              {/* Blue Action Circle: Stop / Send / Audio Wave (media_1791084397752.png) */}
              {midTurn && canSend && <>
                <Tooltip label="等这一轮结束后再发" side="top" delayMs={300}>
                  <button type="button" className={codexCss.queueBtn} onClick={() => { void submit('queue') }}>排队</button>
                </Tooltip>
                <Tooltip label="插话：送进正在进行的这一轮" side="top" delayMs={300}>
                  <button type="button" className={codexCss.actionBtn} aria-label="插话" onClick={() => { void submit('interject') }}>
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round">
                      <line x1="12" y1="19" x2="12" y2="5" />
                      <polyline points="5 12 12 5 19 12" />
                    </svg>
                  </button>
                </Tooltip>
              </>}
              {submitting ? <button type='button' aria-label='取消上传' className={codexCss.stopBtn} onClick={()=>uploadAbort.current?.abort()}>{STOP_ICON}</button> : running && onStop !== undefined ? (
                <Tooltip label={t('input.stop')} side="top" delayMs={500}>
                  <button
                    type="button"
                    className={codexCss.stopBtn}
                    aria-label={t('input.stop')}
                    onClick={onStop}
                  >
                    {STOP_ICON}
                  </button>
                </Tooltip>
              ) : (draft.trim() !== '' || attachments.items.length>0) ? (
                <Tooltip label={t('input.send')} side="top" delayMs={500} disabled={!canSend}>
                  <button
                    type="button"
                    className={codexCss.actionBtn}
                    aria-label={t('input.send')}
                    disabled={!canSend}
                    onClick={() => { void submit() }}
                  >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round">
                      <line x1="12" y1="19" x2="12" y2="5" />
                      <polyline points="5 12 12 5 19 12" />
                    </svg>
                  </button>
                </Tooltip>
              ) : (
                <button type="button" className={codexCss.actionBtn} aria-label={t('input.send')} disabled>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round">
                    <line x1="12" y1="19" x2="12" y2="5" />
                    <polyline points="5 12 12 5 19 12" />
                  </svg>
                </button>
              )}
            </div>
          </div>
        </div>
      </div>

      {menuOpen && (
        <div className={local.suggest} role="listbox" aria-label={token?.kind === '@' ? '引用文件' : '技能指令'}>
          {suggestions.map((suggestion, index) => (
            <button
              key={suggestion.id}
              type="button"
              role="option"
              aria-selected={index === activeIndex}
              className={clsx(local.suggestOption, index === activeIndex && local.suggestActive)}
              onMouseDown={(event) => {
                event.preventDefault()
                applySuggestion(suggestion)
              }}
            >
              <span className={local.suggestLabel}>{suggestion.label}</span>
              {suggestion.detail !== '' && <span className={local.suggestDetail}>{suggestion.detail}</span>}
            </button>
          ))}
        </div>
      )}

      {/* Codex-styled Permission Popover for DSH Permissions (media_1791085181305.png) */}
      {permMenuOpen && createPortal(
        <div
          ref={permMenuRef}
          className={codexCss.permPopover}
          style={{
            top: permMenuPos.top,
            bottom: permMenuPos.bottom,
            left: permMenuPos.left,
          }}
          role="dialog"
          aria-label="工作区操作权限"
        >
          <div className={codexCss.permPopoverHeader}>
            <span className={codexCss.permPopoverTitle}>工作区操作权限</span>
          </div>

          <div className={codexCss.permOptionList}>
            {/* 1. 执行前询问 (Ask) */}
            <button
              type="button"
              className={codexCss.permOptionItem}
              onClick={() => handleSelectPermission('ask')}
            >
              <span className={codexCss.permOptionIcon}>{ASK_ICON}</span>
              <div className={codexCss.permOptionBody}>
                <span className={codexCss.permOptionTitle}>执行前询问</span>
                <span className={codexCss.permOptionDesc}>运行命令或打开浏览器前先询问。Linux 的命令使用工作区写入沙箱，浏览器不受该沙箱约束。</span>
              </div>
              {permissionMode === 'ask' && (
                <span className={codexCss.permOptionCheck}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="20 6 9 17 4 12" />
                  </svg>
                </span>
              )}
            </button>

            <button type="button" className={codexCss.permOptionItem} onClick={() => handleSelectPermission('auto_review')}>
              <span className={codexCss.permOptionIcon}>{AUTO_REVIEW_ICON}</span>
              <div className={codexCss.permOptionBody}>
                <span className={codexCss.permOptionTitle}>自动审批</span>
                <span className={codexCss.permOptionDesc}>需要确认的操作先由 AI 对照你的要求审查：明确该做的直接放行，明确不该做的拒绝，拿不准时再问你。命令的沙箱与执行前询问相同。</span>
              </div>
              {permissionMode === 'auto_review' && (
                <span className={codexCss.permOptionCheck}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="20 6 9 17 4 12" />
                  </svg>
                </span>
              )}
            </button>

            {/* 3. 完全权限 (Full Access) */}
            <button
              type="button"
              className={codexCss.permOptionItem}
              onClick={() => handleSelectPermission('full_access')}
            >
              <span className={clsx(codexCss.permOptionIcon, codexCss.permOptionIconWarn)}>{FULL_ACCESS_ICON}</span>
              <div className={codexCss.permOptionBody}>
                <span className={codexCss.permOptionTitle}>完全权限</span>
                <span className={codexCss.permOptionDesc}>不再询问，agent 自己运行命令和浏览器。适合说完就交给它、不想被打断的时候</span>
              </div>
              {permissionMode === 'full_access' && (
                <span className={codexCss.permOptionCheck}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="20 6 9 17 4 12" />
                  </svg>
                </span>
              )}
            </button>
          </div>
        </div>,
        document.body,
      )}

      {/* DSH Native Risk Confirmation Dialog */}
      {showRiskConfirm && (
        <RiskConfirmation
          open={showRiskConfirm}
          title="切换到完全权限？"
          description="之后 agent 会直接运行命令、打开浏览器，不再逐条询问。命令以你的系统账户运行，不受工作区目录限制。只需要确认这一次。"
          acknowledgeLabel="我了解，之后不用再问"
          cancelLabel="取消"
          closeLabel="关闭"
          confirmLabel="切换"
          acknowledged={riskAcknowledged}
          onAcknowledgedChange={setRiskAcknowledged}
          onCancel={() => {
            setShowRiskConfirm(false)
            setRiskAcknowledged(false)
          }}
          onConfirm={() => {
            setShowRiskConfirm(false)
            setRiskAcknowledged(false)
            selectPermissionMode('full_access')
          }}
        />
      )}
    </div>
  )
}
