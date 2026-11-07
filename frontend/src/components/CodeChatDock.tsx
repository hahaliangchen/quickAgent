import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type DragEvent } from 'react'
import clsx from 'clsx'
import type { CatalogModel, PermissionMode } from '../api.ts'
import { AttachmentRail, useAttachmentDraft } from './Attachments.tsx'
import { WORKSPACE_PATH_TYPE } from '../codeChat.ts'
import { useAgentApi, useCordis } from '../cordis/react.tsx'
import type { AgentSlotProps } from '../cordis/slot-contract.ts'
import { randomId } from '../randomId.ts'
import { useSession } from '../useSession.ts'
import { Icon } from './WorkbenchIcon.tsx'
import { ShellSetupDialog } from './ShellSetupDialog.tsx'
import css from './CodeChatDock.module.css'

/** One conversation per repository, remembered on this device. */
const taskKey = (workspace: string) => `code_chat_task:${workspace}`
function rememberedTask(workspace: string): string | undefined {
  try { return localStorage.getItem(taskKey(workspace)) ?? undefined } catch { return undefined }
}
function permissionMode(): PermissionMode {
  try {
    const saved = localStorage.getItem('dsh_permission_mode')
    if (saved === 'read_only' || saved === 'workspace_write' || saved === 'ask' || saved === 'full_access') return saved
  } catch {}
  return 'ask'
}
const WIDTH_KEY = 'code_chat_width'
function savedWidth(): number {
  try { const saved = Number(localStorage.getItem(WIDTH_KEY)); if (saved) return Math.min(720, Math.max(320, saved)) } catch {}
  // The garden design gives the chat a wider card.
  return document.body.hasAttribute('data-garden-theme') ? 460 : 400
}
const MODEL_KEY = 'code_chat_model'
const modelKey = (model: Pick<CatalogModel, 'provider' | 'id'>) => `${model.provider}/${model.id}`
function savedModel(): string { try { return localStorage.getItem(MODEL_KEY) ?? '' } catch { return '' } }
const mention = (path: string) => path.includes(' ') ? `@"${path}"` : `@${path}`
const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1) || path

/** Chat docked beside a page, VS Code style: asks about the file in view and files dropped on it. */
export function CodeChatDock({ workspace, renderSlot, onOpenInChat }: {
  workspace: string
  renderSlot: AgentSlotProps['renderSlot']
  onOpenInChat: (taskId: string) => void
}) {
  const api = useAgentApi()
  const chat = useCordis().codeChat
  const context = useSyncExternalStore(chat.subscribe, chat.getSnapshot)
  const [open, setOpen] = useState(false)
  const [width, setWidth] = useState(savedWidth)
  const [taskId, setTaskId] = useState(() => rememberedTask(workspace))
  const [draft, setDraft] = useState('')
  const [error, setError] = useState('')
  const [sending, setSending] = useState(false)
  const [dropping, setDropping] = useState(false)
  // The current file is offered again whenever it changes.
  const [skipped, setSkipped] = useState<string>()
  const session = useSession(taskId, () => {})
  const input = useRef<HTMLTextAreaElement>(null)
  const panel = useRef<HTMLElement>(null)
  const filePicker = useRef<HTMLInputElement>(null)
  const attachments = useAttachmentDraft()
  const [models, setModels] = useState<readonly CatalogModel[]>([])
  /** provider/model picked here; empty follows the agent default. */
  const [model, setModel] = useState(savedModel)
  const [defaultModel, setDefaultModel] = useState('')
  useEffect(() => {
    if (!open || models.length) return
    api.models().then(catalog => {
      setModels(catalog.models.filter(m => m.enabled && m.available_for_agent))
      const fallback = catalog.defaults.agent
      if (fallback.provider && fallback.model) setDefaultModel(`${fallback.provider}/${fallback.model}`)
    }, () => {})
  }, [open])
  // A page beside the panel can leave room for it (the garden theme floats the panel over the page's edge).
  useLayoutEffect(() => {
    const host = panel.current?.parentElement
    if (!open || !host) return
    host.style.setProperty('--code-chat-width', `${width}px`)
    return () => { host.style.removeProperty('--code-chat-width') }
  }, [open, width])
  useEffect(() => { setTaskId(rememberedTask(workspace)) }, [workspace])
  useEffect(() => { if (open) input.current?.focus() }, [open])

  const running = session.task?.status === 'running' || session.task?.status === 'cancelling'
  const current = context.current && context.current !== skipped && !context.tags.includes(context.current) ? context.current : undefined
  const tags = current ? [current, ...context.tags] : context.tags

  function forget() {
    try { localStorage.removeItem(taskKey(workspace)) } catch {}
    setTaskId(undefined); setError('')
  }

  async function send() {
    const text = draft.trim()
    if (!text || sending || running) return
    setSending(true); setError('')
    try {
      // A remembered conversation that was deleted elsewhere is replaced.
      let id = taskId && !session.error ? taskId : undefined
      if (!id) {
        id = await api.createSession(workspace, 'code')
        try { localStorage.setItem(taskKey(workspace), id) } catch {}
        void api.updateSession(id, { title: `代码问答 · ${baseName(workspace)}` }).catch(() => {})
      }
      const prompt = tags.length ? `${tags.map(mention).join(' ')}\n\n${text}` : text
      const content = []
      for (const item of attachments.items) content.push({ type: 'file', receiptId: (await api.upload(id, item.file, new AbortController().signal, () => {})).receiptId })
      const picked = models.find(m => modelKey(m) === model)
      await api.prompt(id, prompt, {
        permission_mode: permissionMode(), requestId: randomId(), content,
        ...(picked ? { provider: picked.provider, model: picked.id } : {}),
      })
      setDraft(''); chat.clear(); setSkipped(undefined); attachments.clear()
      if (id === taskId) session.reload(); else setTaskId(id)
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)) }
    finally { setSending(false) }
  }

  const accepts = (event: DragEvent) => event.dataTransfer.types.includes(WORKSPACE_PATH_TYPE)
  const dropProps = {
    onDragOver: (event: DragEvent) => { if (accepts(event)) { event.preventDefault(); event.dataTransfer.dropEffect = 'copy'; setDropping(true) } },
    onDragLeave: (event: DragEvent) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropping(false) },
    onDrop: (event: DragEvent) => {
      setDropping(false)
      const path = event.dataTransfer.getData(WORKSPACE_PATH_TYPE)
      if (!path) return
      event.preventDefault(); chat.add(path); setOpen(true)
    },
  }

  if (!open) return <button type="button" className={clsx(css.launcher, dropping && css.dropping)} title="代码问答：问 AI 当前文件"
    onClick={() => setOpen(true)} onDragEnter={event => { if (accepts(event)) setOpen(true) }} {...dropProps}>
    <Icon name="chat" size={20} />
  </button>

  // Drag the left edge to resize; the width is remembered on this device.
  function resize(event: React.PointerEvent) {
    event.preventDefault()
    const startX = event.clientX, startWidth = width
    let next = startWidth
    const move = (e: PointerEvent) => { next = Math.min(720, Math.max(320, startWidth + startX - e.clientX)); setWidth(next) }
    const up = () => {
      window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', up)
      try { localStorage.setItem(WIDTH_KEY, String(next)) } catch {}
    }
    window.addEventListener('pointermove', move); window.addEventListener('pointerup', up)
  }

  const shownModel = model || defaultModel
  return <section ref={panel} className={clsx(css.panel, dropping && css.dropping)} style={{ width }} aria-label="代码问答" data-code-chat="" {...dropProps}>
    <ShellSetupDialog enabled={open && !!workspace} scope={workspace} onReady={session.reload} />
    <div className={css.resizer} title="拖动调整宽度" onPointerDown={resize} />
    <header className={css.head}>
      <SparkleIcon />
      <strong>代码问答</strong>
      <span className={css.grow} />
      {taskId && <button type="button" className={css.iconButton} title="在主界面打开这个对话" onClick={() => onOpenInChat(taskId)}><Icon name="external" size={15} /></button>}
      <button type="button" className={css.iconButton} title="收起" onClick={() => setOpen(false)}><Icon name="close" size={17} /></button>
    </header>
    <div className={css.repoRow}>
      <span className={css.repo} title={workspace}><Icon name="folder" size={15} />{baseName(workspace)}<Icon name="info" size={14} /></span>
    </div>
    <div className={css.body}>
      {taskId && session.task
        ? renderSlot('agent.chat', { events: session.events, task: session.task, running, onOpenTask: onOpenInChat, onChangesUpdated: session.reload })
        : <div className={css.greeting}>
          <strong>👋 你好！我是 QuickAgent 代码助手</strong>
          <p>我可以阅读、解释当前仓库的代码，回答开发问题。打开文件后直接提问，AI 会读这个文件来回答；把左侧文件树里的文件或文件夹拖到这里，可以一起问。</p>
        </div>}
    </div>
    {dropping && <div className={css.dropMask}>松开即添加为上下文</div>}
    <footer className={css.composer}>
      {tags.length > 0 && <div className={css.tags}>
        {tags.map(path => <span key={path} className={clsx(css.tag, path === current && css.tagCurrent)} title={path === current ? `当前文件：${path}` : path}>
          <Icon name="file" size={13} /><span className={css.tagName}>{baseName(path)}</span>
          <button type="button" aria-label={`移除 ${path}`} onClick={() => { if (path === current) setSkipped(path); else chat.remove(path) }}><Icon name="close" size={12} /></button>
        </span>)}
      </div>}
      <AttachmentRail items={attachments.items} onRemove={attachments.remove} disabled={sending} />
      <div className={css.inputCard}>
        <textarea ref={input} rows={2} value={draft} placeholder={tags.length ? '问问这些代码…' : '描述你的需求或提出代码问题…'} disabled={sending}
          title="Enter 发送 · Shift+Enter 换行"
          onChange={event => setDraft(event.target.value)}
          onPaste={event => { if (event.clipboardData.files.length) { event.preventDefault(); attachments.add(event.clipboardData.files) } }}
          onKeyDown={event => { if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); void send() } }} />
        <div className={css.actions}>
          <button type="button" className={clsx(css.toolButton, css.newChat)} title="新对话" aria-label="新对话" disabled={running} onClick={forget}><span className={css.plus}>+</span></button>
          <button type="button" className={clsx(css.toolButton, css.attach)} title="添加附件" aria-label="添加附件" disabled={sending} onClick={() => filePicker.current?.click()}><Icon name="clip" size={15} /></button>
          <input ref={filePicker} type="file" multiple hidden onChange={event => { if (event.target.files) attachments.add(event.target.files); event.target.value = '' }} />
          {models.length > 0 && <label className={css.model} title="这个对话用的模型">
            <Icon name="box" size={14} />
            <select value={shownModel} onChange={event => { setModel(event.target.value); try { localStorage.setItem(MODEL_KEY, event.target.value) } catch {} }}>
              {!models.some(m => modelKey(m) === shownModel) && <option value={shownModel}>默认模型</option>}
              {models.map(m => <option key={modelKey(m)} value={modelKey(m)}>{m.name || m.id}</option>)}
            </select>
          </label>}
          <span className={css.grow} />
          {running
            ? <button type="button" className={css.stop} onClick={() => { if (taskId) void api.cancel(taskId).then(session.reload, () => {}) }}><Icon name="stop" size={14} />停止</button>
            : <button type="button" className={css.send} title="发送" aria-label="发送" disabled={!draft.trim() || sending} onClick={() => { void send() }}><Icon name="send" size={16} /></button>}
        </div>
      </div>
      {(error || attachments.error) && <p role="alert" className={css.error}>{error || attachments.error}</p>}
    </footer>
  </section>
}

function SparkleIcon() {
  return <svg className={css.sparkle} width={22} height={22} viewBox="0 0 24 24" aria-hidden="true">
    <path d="M10 2.5c.6 3.9 2.6 5.9 6.5 6.5-3.9.6-5.9 2.6-6.5 6.5-.6-3.9-2.6-5.9-6.5-6.5 3.9-.6 5.9-2.6 6.5-6.5Z" fill="currentColor" />
    <path d="M18.5 13.5c.3 1.9 1.2 2.8 3 3-1.8.3-2.7 1.1-3 3-.3-1.9-1.2-2.7-3-3 1.8-.2 2.7-1.1 3-3Z" fill="currentColor" opacity=".75" />
  </svg>
}
