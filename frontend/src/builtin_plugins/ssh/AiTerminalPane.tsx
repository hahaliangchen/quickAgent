import { type CSSProperties, memo, useEffect, useRef, useState } from 'react'
import { type AnsiLine, parseAnsiLines } from '../../dsh/ui-primitives/ansi.ts'
import { type AiActivity, message, ssh } from './api.ts'
import type { OutputStatus } from './TerminalPane.tsx'
import {
  type CommandExit,
  type TranscriptBlock,
  type TranscriptState,
  commandLines,
  countCommands,
  emptyTranscript,
  feedTranscript,
} from './transcript.ts'
import { ScrollToBottom } from '../../components/ScrollToBottom.tsx'
import { appConfirm } from '../../components/AppDialog.tsx'
import css from './style.module.css'

/** How one command reads in a card footer. */
interface Outcome {
  /** Footer text: the exit code plus how long the command took. */
  label: string
  /** Which colour the footer and the card's edge take. */
  tone: 'running' | 'ok' | 'fail' | 'unknown'
}

/**
 * Render a duration the way it reads best: milliseconds below a second.
 * @param ms - the duration in milliseconds, or null when unknown.
 * @returns the formatted duration, or an empty string when unknown.
 */
function duration(ms: number | null): string {
  if (ms === null) return ''
  return ms < 1000 ? ms + ' ms' : (ms / 1000).toFixed(1) + ' s'
}

/**
 * Turn one command's state into its footer label.
 * @param exit - the parsed outcome, or undefined while the command still runs.
 * @returns the label and the tone that colours it.
 */
function outcome(exit: CommandExit | undefined): Outcome {
  if (exit === undefined) return { label: '执行中…', tone: 'running' }
  const time = duration(exit.elapsed)
  const tail = time === '' ? '' : ' · ' + time
  if (exit.code === null) return { label: '无退出码' + tail, tone: 'unknown' }
  if (exit.code === 0) return { label: '退出 0' + tail, tone: 'ok' }
  return { label: '退出 ' + exit.code + tail, tone: 'fail' }
}

/**
 * One styled run of output as DOM.
 * @param line - the line's runs.
 * @returns the line's children, blank lines kept as a real row.
 */
function runs(line: AnsiLine) {
  if (line.length === 0) return ' '
  return line.map((run, index) => run.style === undefined
    ? run.text
    : <span key={index} style={run.style as CSSProperties}>{run.text}</span>)
}

/** Output text with its trailing blank lines dropped, so cards end where the text does. */
const trimmed = (text: string) => text.replace(/\s+$/, '')

/**
 * One AI command: the command line the AI sent, then everything it printed.
 * Memoized on its own text so a streaming command does not re-render the rest
 * of the transcript on every chunk.
 */
const CommandCard = memo(function CommandCard({ cwd, command, output, exit }: {
  cwd: string
  command: string
  output: string | undefined
  exit?: CommandExit | undefined
}) {
  const lines = commandLines(command)
  const rows = parseAnsiLines(trimmed(output ?? ''))
  const state = outcome(exit)
  const printed = rows.some(row => row.some(run => run.text.trim() !== ''))
  return <article className={css.cmd} data-tone={state.tone}>
    <header className={css.cmdHead}>
      <span className={css.cmdBadge}>AI</span>
      <span className={css.cmdCwd} title={cwd}>{cwd}</span>
      <span className={css.cmdState} data-tone={state.tone}>{state.label}</span>
    </header>
    <div className={css.cmdBody}>
      {lines.map((line, index) => <div key={index} className={css.cmdLine}>
        {/* Every command row keeps a `$`, the way a shell prompt does, so a
            multi-line command still reads as one command and not as two. */}
        <span className={css.cmdPrompt} aria-hidden="true">$</span>
        <span className={css.cmdText}>{line === '' ? ' ' : line}</span>
      </div>)}
      {printed && <pre className={css.cmdOut}>{rows.map((row, index) => <div key={index} className={css.outLine}>{runs(row)}</div>)}</pre>}
    </div>
  </article>
})

/**
 * Everything the AI did on one server, in order.
 * @param props.blocks - the transcript, oldest first.
 * @returns one card per command and one notice per non-command output.
 */
export function AiTranscript({ blocks }: { blocks: readonly TranscriptBlock[] }) {
  return <>{blocks.map(block => block.kind === 'note'
    ? <pre key={block.id} className={css.note}>{parseAnsiLines(trimmed(block.text)).map((row, index) => <div key={index} className={css.outLine}>{runs(row)}</div>)}</pre>
    : <CommandCard key={block.id} cwd={block.cwd} command={block.command} output={block.output} exit={block.exit} />)}</>
}

/**
 * The read-only mirror of what the AI is running over SSH.
 * @param props.serverId - the server profile the tab belongs to.
 * @param props.taskId - the chat whose tool calls this stream mirrors.
 * @param props.active - whether the tab is the visible one.
 * @param props.onOpenHuman - opens an interactive terminal on the same server.
 * @param props.onActivity - reports every finished tool call to the workbench.
 * @param props.diagnose - hands selected output to the chat composer.
 * @returns the pane: a status row and the command transcript.
 */
export default function AiTerminalPane({ serverId, taskId, active, onOpenHuman, onActivity, diagnose }: {
  serverId: string; taskId: string; active: boolean
  onOpenHuman: () => void; onActivity: (event: AiActivity) => void; diagnose?: ((text: string) => void) | undefined
}) {
  const state = useRef<TranscriptState>(emptyTranscript())
  const list = useRef<HTMLDivElement>(null)
  const follow = useRef(true)
  const callback = useRef(onActivity)
  callback.current = onActivity
  const [revision, setRevision] = useState(0)
  const [paused, setPaused] = useState(false)
  const [status, setStatus] = useState<OutputStatus>('连接中')
  const [attempt, setAttempt] = useState(0)
  const [selected, setSelected] = useState(false)
  const [copyError, setCopyError] = useState('')

  // Every chunk keeps the pane pinned to the newest output, unless the reader
  // scrolled up to look at something older.
  useEffect(() => {
    const element = list.current
    if (element && follow.current) element.scrollTop = element.scrollHeight
  }, [revision])

  // Selection is tracked here rather than by a terminal API: the transcript is
  // ordinary DOM text, so the browser selection is the source of truth.
  useEffect(() => {
    const onSelectionChange = () => {
      const selection = window.getSelection()
      const anchor = selection?.anchorNode
      setSelected(anchor !== null && anchor !== undefined && list.current !== null && list.current.contains(anchor)
        ? selection !== null && selection.toString() !== ''
        : false)
    }
    document.addEventListener('selectionchange', onSelectionChange)
    return () => document.removeEventListener('selectionchange', onSelectionChange)
  }, [])

  useEffect(() => {
    const decoder = new TextDecoder('utf-8')
    const controller = new AbortController()
    let socket: WebSocket | undefined
    let failed = false
    state.current = emptyTranscript()
    setRevision(value => value + 1)
    setStatus('连接中')
    const fail = () => { if (!controller.signal.aborted) { failed = true; setStatus('连接失败') } }
    void ssh.aiStreamTicket(serverId, taskId, controller.signal).then(({ ticket }) => {
      if (controller.signal.aborted) return
      let ready = false
      socket = ssh.aiStream(serverId, ticket)
      socket.onmessage = event => {
        if (controller.signal.aborted) return
        if (event.data instanceof ArrayBuffer) {
          if (!ready) return
          feedTranscript(state.current, decoder.decode(new Uint8Array(event.data), { stream: true }))
          setRevision(value => value + 1)
          return
        }
        let control: { type: 'ready' } | AiActivity
        try { control = JSON.parse(String(event.data)) } catch { return }
        if (control.type === 'ready') {
          ready = true
          state.current = emptyTranscript()
          setRevision(value => value + 1)
          setStatus('已连接')
        } else if (ready && control.type === 'activity') callback.current(control)
      }
      socket.onerror = fail
      socket.onclose = () => { if (!controller.signal.aborted && !failed) setStatus('已断开') }
    }).catch(fail)
    return () => {
      controller.abort()
      if (socket?.readyState === WebSocket.CONNECTING) { socket.onerror = null; socket.onopen = () => socket?.close() }
      else socket?.close()
    }
  }, [serverId, taskId, attempt])

  useEffect(() => {
    if (!active) return
    const frame = requestAnimationFrame(() => {
      const element = list.current
      if (element && follow.current) element.scrollTop = element.scrollHeight
    })
    return () => cancelAnimationFrame(frame)
  }, [active])

  function scrolled() {
    const element = list.current
    if (!element) return
    const atBottom = element.scrollHeight - element.scrollTop - element.clientHeight < 24
    follow.current = atBottom
    setPaused(!atBottom)
  }
  function jump() {
    const element = list.current
    if (!element) return
    element.scrollTop = element.scrollHeight
    follow.current = true
    setPaused(false)
  }
  function picked(): string {
    const selection = window.getSelection()
    const text = selection === null ? '' : selection.toString()
    return list.current !== null && selection?.anchorNode !== null && selection?.anchorNode !== undefined && list.current.contains(selection.anchorNode)
      ? text.trim()
      : ''
  }
  async function copy() {
    const text = picked()
    if (text === '') return
    try { await navigator.clipboard.writeText(text); setCopyError('') } catch (error) { setCopyError(message(error)) }
  }
  async function addToDraft() {
    const text = picked().slice(0, 4000)
    if (text !== '' && await appConfirm({ title: '加入问答草稿', message: '将选中的终端输出加入问答草稿？请先确认没有密钥、密码或其他敏感数据。', confirmText: '加入草稿' })) diagnose?.(text)
  }

  const blocks = state.current.blocks
  const counts = countCommands(blocks)
  const summary = counts.total === 0
    ? '暂无命令'
    : counts.total + ' 条命令' + (counts.failed === 0 ? '' : ' · ' + counts.failed + ' 失败')

  return <section className={css.terminalPane} hidden={!active} aria-label="AI 实时终端 · 只读">
    <div className={css.aiTools}>
      <strong>AI 实时终端 · 只读</strong>
      <em className={css.aiCount} data-failed={counts.failed > 0}>{summary}</em>
      <span role="status" data-connected={status === '已连接'}>{status}</span>
      {(status === '已断开' || status === '连接失败') && <button onClick={() => setAttempt(value => value + 1)}>重新连接</button>}
      <button onClick={onOpenHuman}>＋ 打开人工终端</button>
      <button disabled={!selected} onClick={() => void copy()}>复制选区</button>
      {diagnose && <button disabled={!selected} onClick={() => void addToDraft()}>加入问答草稿</button>}
    </div>
    {copyError && <p role="alert" className={css.muted}>{copyError}</p>}
    <div className={css.transcriptArea}>
      <div ref={list} className={css.transcript} data-testid="ssh-ai-terminal" onScroll={scrolled}>
        {blocks.length === 0
          ? <div className={css.emptyHint}>
            <p>AI 在这台服务器上执行的命令会按顺序显示在这里。</p>
            <p className={css.muted}>每条命令一张卡片：标题栏是 AI 发起的工作目录与退出状态，卡片里先命令、后输出。</p>
          </div>
          : <AiTranscript blocks={blocks} />}
      </div>
      <ScrollToBottom show={paused} onJump={jump} />
    </div>
  </section>
}
