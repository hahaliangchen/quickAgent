import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import '@xterm/xterm/css/xterm.css'
import { type AiActivity, message, ssh } from './api.ts'
import { terminalFontFamily, terminalThemes, type OutputStatus } from './TerminalPane.tsx'
import css from './style.module.css'

export default function AiTerminalPane({ serverId, taskId, active, theme, onOpenHuman, onActivity, diagnose }: {
  serverId: string; taskId: string; active: boolean; theme: 'dark' | 'light'
  onOpenHuman: () => void; onActivity: (event: AiActivity) => void; diagnose?: ((text: string) => void) | undefined
}) {
  const host = useRef<HTMLDivElement>(null)
  const terminal = useRef<Terminal>()
  const fit = useRef<FitAddon>()
  const callback = useRef(onActivity)
  callback.current = onActivity
  const [status, setStatus] = useState<OutputStatus>('连接中')
  const [attempt, setAttempt] = useState(0)
  const [selected, setSelected] = useState(false)
  const [copyError, setCopyError] = useState('')

  useEffect(() => {
    const element = host.current
    if (!element) return
    const term = new Terminal({ fontSize: 13, lineHeight: 1.45, fontFamily: terminalFontFamily, disableStdin: true, cursorBlink: false, scrollback: 3500, screenReaderMode: true, theme: terminalThemes[theme] })
    const addon = new FitAddon()
    terminal.current = term; fit.current = addon
    term.loadAddon(addon)
    term.loadAddon(new WebLinksAddon((event, uri) => {
      if (event.isTrusted && /^https?:\/\//i.test(uri) && window.confirm('打开终端中的外部链接？\n' + uri)) window.open(uri, '_blank', 'noopener,noreferrer')
    }))
    term.open(element)
    const resize = new ResizeObserver(() => { if (element.clientWidth && element.clientHeight) addon.fit() })
    resize.observe(element)
    const selection = term.onSelectionChange(() => setSelected(term.hasSelection()))
    return () => { selection.dispose(); resize.disconnect(); term.dispose(); terminal.current = undefined; fit.current = undefined }
  }, [])

  useEffect(() => { if (terminal.current) terminal.current.options.theme = terminalThemes[theme] }, [theme])
  useEffect(() => {
    if (!active) return
    const frame = requestAnimationFrame(() => { if (host.current?.clientWidth && host.current.clientHeight) fit.current?.fit() })
    return () => cancelAnimationFrame(frame)
  }, [active])

  useEffect(() => {
    const term = terminal.current
    if (!term) return
    const controller = new AbortController()
    let socket: WebSocket | undefined
    let failed = false
    setStatus('连接中')
    const fail = () => { if (!controller.signal.aborted) { failed = true; setStatus('连接失败') } }
    void ssh.aiStreamTicket(serverId, taskId, controller.signal).then(({ ticket }) => {
      if (controller.signal.aborted) return
      let ready = false
      socket = ssh.aiStream(serverId, ticket)
      socket.onmessage = event => {
        if (controller.signal.aborted) return
        if (event.data instanceof ArrayBuffer) { if (ready) term.write(new Uint8Array(event.data)); return }
        let control: { type: 'ready' } | AiActivity
        try { control = JSON.parse(String(event.data)) } catch { return }
        if (control.type === 'ready') { term.reset(); ready = true; setStatus('已连接') }
        else if (ready && control.type === 'activity') callback.current(control)
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

  async function copy() {
    const text = terminal.current?.getSelection()
    if (text) try { await navigator.clipboard.writeText(text); setCopyError('') } catch (error) { setCopyError(message(error)) }
  }
  function addToDraft() {
    const text = terminal.current?.getSelection().trim().slice(0, 4000)
    if (text && window.confirm('将选中的终端输出加入问答草稿？请先确认没有密钥、密码或其他敏感数据。')) diagnose?.(text)
  }

  return <section className={css.terminalPane} hidden={!active} aria-label="AI 实时终端 · 只读">
    <div className={css.aiTools}>
      <strong>AI 实时终端 · 只读</strong><span role="status" data-connected={status === '已连接'}>{status}</span>
      {(status === '已断开' || status === '连接失败') && <button onClick={() => setAttempt(value => value + 1)}>重新连接</button>}
      <button onClick={onOpenHuman}>＋ 打开人工终端</button>
      <button disabled={!selected} onClick={() => void copy()}>复制选区</button>
      {diagnose && <button disabled={!selected} onClick={addToDraft}>加入问答草稿</button>}
    </div>
    {copyError && <p role="alert" className={css.muted}>{copyError}</p>}
    <div ref={host} className={css.terminalHost} data-testid="ssh-ai-terminal" />
  </section>
}
