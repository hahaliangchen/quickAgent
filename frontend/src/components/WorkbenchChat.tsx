import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import clsx from 'clsx'
import { MarkdownText } from '@deepseek-ai/dsh-client-ui-primitives'
import { api, type Task } from '../api.ts'
import { changeSummaries } from '../changes.ts'
import { markdownLabels } from '../dsh/markdown-labels.ts'
import { t } from '../i18n.ts'
import { chatItems } from '../model.ts'
import { useSession } from '../useSession.ts'
import { clock } from '../workbenchModel.ts'
import { toolVisual } from './toolVisuals.tsx'
import { Icon } from './WorkbenchIcon.tsx'
import styles from './SkillWorkbench.module.css'

const STARTERS = ['说明每段程序做了什么、确认点在哪里', '把只属于这一次的值改成参数', '检查改写后的流程有没有漏掉原来的步骤']

function parsed(text: string): unknown {
  try { return JSON.parse(text) } catch { return undefined }
}

/**
 * The draft's own Agent conversation, floating over the workbench. It is an
 * ordinary code session working in the draft folder: what it says and every
 * file it changes come from that session's record.
 */
export function DraftChat({ chatId, draftLabel, draftFolder, source, hidden, expanded, onExpand, onClose, onRunning, onSettled, onShowFile }: {
  chatId: string
  draftLabel: string
  draftFolder: string
  source: Task
  hidden: boolean
  expanded: boolean
  onExpand: () => void
  onClose: () => void
  onRunning: (running: boolean) => void
  onSettled: () => void
  onShowFile: (path: string) => void
}) {
  const session = useSession(chatId, onSettled)
  const items = useMemo(() => chatItems(session.events, session.task), [session.events, session.task])
  const changes = useMemo(() => changeSummaries(session.events), [session.events])
  const labels = useMemo(() => markdownLabels(t), [])
  const running = session.task?.status === 'running' || session.task?.status === 'cancelling'
  const [input, setInput] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const body = useRef<HTMLDivElement>(null)
  useEffect(() => { onRunning(running) }, [running, onRunning])
  useEffect(() => { body.current?.scrollTo({ top: body.current.scrollHeight }) }, [items])

  const send = async (text: string) => {
    if (!text.trim() || busy || running) return
    setBusy(true)
    setError('')
    try {
      await api.prompt(chatId, text.trim(), {
        permission_mode: 'workspace_write',
        model: source.model,
        provider: source.provider_name ?? undefined,
        reasoning_effort: source.reasoning_effort ?? undefined,
      })
      setInput('')
      session.reload()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }
  const stop = () => { void api.cancel(chatId).then(session.reload, cause => setError(String(cause))) }

  const rows: ReactNode[] = []
  let turn = 1
  for (const item of items) {
    switch (item.kind) {
      case 'turn':
        turn = item.turn
        break
      case 'user':
        rows.push(<div key={item.key} className={styles.chatMessage}>
          <span className={clsx(styles.chatAvatar, styles.chatAvatarUser)}><Icon name="user" size={16} /></span>
          <div className={styles.chatMessageBody}>
            <div className={clsx(styles.bubble, styles.bubbleUser)}>{item.text}</div>
            <time>{clock(item.time)}</time>
          </div>
        </div>)
        break
      case 'assistant':
        if (item.text.trim() === '' && !item.streaming) break
        rows.push(<div key={item.key} className={styles.chatMessage}>
          <span className={clsx(styles.chatAvatar, styles.chatAvatarAi)}><Icon name="box" size={16} /></span>
          <div className={styles.chatMessageBody}>
            <div className={clsx(styles.bubble, styles.bubbleAi)}>
              {item.text.trim() ? <MarkdownText text={item.text} streaming={item.streaming} labels={labels} variant="compact" /> : <span className={styles.thinking}>正在思考…</span>}
            </div>
            {!item.streaming && <time>{clock(item.time)}</time>}
          </div>
        </div>)
        break
      case 'tool': {
        const args = parsed(item.arguments)
        const visual = toolVisual(item.name, args)
        const record = typeof args === 'object' && args !== null ? args as Record<string, unknown> : {}
        // What the call worked on: the tool's own summary, else the file it named.
        const target = visual.summary ?? [record.path, record.file_path, record.query, record.pattern].find((value): value is string => typeof value === 'string')
        rows.push(<div key={item.key} className={clsx(styles.chatStep, item.result?.isError && styles.chatStepFailed)}>
          {visual.icon}<span>{visual.title}</span>{target && <code title={target}>{target}</code>}
          {item.result === undefined ? <em>进行中</em> : item.result.isError ? <em>失败</em> : null}
        </div>)
        break
      }
      case 'call':
        rows.push(<div key={item.key} className={styles.chatStep}><Icon name="spark" size={14} /><span>调用 {item.name}</span><em>{item.error ? '失败' : item.status === 'running' ? '进行中' : ''}</em></div>)
        break
      case 'program':
        rows.push(<div key={item.key} className={styles.chatStep}><Icon name="code" size={14} /><span>{item.event === 'started' ? '程序开始运行' : item.event === 'invalid' ? '程序未通过编译' : item.event === 'failed' ? '程序出错' : '程序记录'}</span>{item.text && <code title={item.text}>{item.text}</code>}</div>)
        break
      case 'turn-end': {
        const summary = changes.get(turn)
        if (summary && summary.files.length > 0) {
          rows.push(<div key={`${item.key}:changes`} className={styles.chatChanges}>
            <span>本轮改动了 {summary.files.length} 个文件</span>
            {summary.files.map(file => <button key={file.path} type="button" onClick={() => onShowFile(file.path)} title="在中间查看差异">
              {file.path}<b>{file.created ? '新增' : file.removed ? '删除' : `+${file.added} −${file.deleted}`}</b>
            </button>)}
          </div>)
        }
        if (item.reason.kind === 'error') rows.push(<div key={item.key} className={styles.chatError}>本轮出错：{item.reason.message}</div>)
        else if (item.reason.kind === 'aborted') rows.push(<div key={item.key} className={styles.chatNotice}>本轮已停止</div>)
        break
      }
      default:
        break
    }
  }
  const lastStreaming = items.at(-1)?.kind === 'assistant'

  return <aside className={clsx(styles.chat, expanded && styles.chatExpanded)} hidden={hidden} aria-label="草稿的 AI 协作对话">
    <div className={styles.chatHead}>
      <span className={styles.chatHeadIcon}><Icon name="chat" size={19} /></span>
      <strong>与 AI 协作</strong>
      <span className={styles.chatTarget} title={draftFolder}>当前草稿 · {draftLabel}</span>
      <button type="button" onClick={onClose} title="收起" aria-label="收起"><Icon name="minus" size={17} /></button>
      <button type="button" onClick={onExpand} title={expanded ? '还原' : '放大'} aria-label={expanded ? '还原' : '放大'}><Icon name={expanded ? 'shrink' : 'expand'} size={16} /></button>
    </div>
    <div className={styles.chatBody} ref={body}>
      {rows.length === 0 && <div className={styles.chatIntro}>
        <p>这是这份草稿自己的对话。AI 在草稿文件夹里工作，可以分析、修改 <code>programs/</code> 里的草稿，原始流程不会被改动。</p>
        <p>这里没有插件页面的工具，真实插件测试尚未接入；AI 说的检查结果以它实际读过、跑过的为准。</p>
        <div className={styles.starters}>{STARTERS.map(text => <button key={text} type="button" onClick={() => setInput(text)}>{text}</button>)}</div>
      </div>}
      {rows}
      {running && !lastStreaming && <div className={styles.chatWorking}><span className={styles.spinner} />AI 正在处理…</div>}
      {(error || session.error) && <div className={styles.chatError}>{error || session.error}</div>}
    </div>
    <form className={styles.chatForm} onSubmit={event => { event.preventDefault(); void send(input) }}>
      <textarea value={input} onChange={event => setInput(event.target.value)} aria-label="给 AI 的修改或测试要求" placeholder="输入修改或测试要求…" rows={2}
        onKeyDown={event => { if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); void send(input) } }} />
      <div className={styles.chatFormBar}>
        <span>{running ? 'AI 正在处理，可随时停止' : 'Enter 发送 · Shift+Enter 换行'}</span>
        {running
          ? <button type="button" className={styles.stopButton} onClick={stop}><Icon name="stop" size={14} />停止</button>
          : <button type="submit" className={styles.sendButton} disabled={!input.trim() || busy} aria-label="发送"><Icon name="send" size={18} /></button>}
      </div>
    </form>
  </aside>
}
