import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import '@xterm/xterm/css/xterm.css'
import { type Access, type Failure, type Server, message, ssh, SshError } from './api.ts'
import css from './style.module.css'

export type OutputStatus = '连接中' | '已连接' | '已断开' | '连接失败'
export const terminalThemes = { dark: { background: '#131719', foreground: '#e1e6e4', cursor: '#90d7c7', selectionBackground: '#31554c' }, light: { background: '#f7f9f8', foreground: '#18201d', cursor: '#23715d', selectionBackground: '#c0e6dc' } }

export default function TerminalPane({ server, access, active, theme, onError, diagnose, onConnection }: {
  server: Server; access: Access; active: boolean; theme: 'dark' | 'light'; onError: (error: unknown) => void; diagnose?: ((value: string) => void) | undefined; onConnection: () => void
}) {
  const host = useRef<HTMLDivElement>(null)
  const terminal = useRef<Terminal>()
  const fit = useRef<FitAddon>()
  const callback = useRef({ onError, onConnection })
  callback.current = { onError, onConnection }
  const [status, setStatus] = useState<OutputStatus>('连接中')
  const [selected, setSelected] = useState(false)

  useEffect(() => {
    if (!host.current) return
    const controller = new AbortController()
    const term = new Terminal({ fontSize: 13, fontFamily: 'ui-monospace, monospace', cursorBlink: true, scrollback: 3500, screenReaderMode: true })
    const addon = new FitAddon()
    terminal.current = term; fit.current = addon
    term.loadAddon(addon)
    term.loadAddon(new WebLinksAddon((event, uri) => {
      if (event.isTrusted && /^https?:\/\//i.test(uri) && window.confirm('打开终端中的外部链接？\n' + uri)) window.open(uri, '_blank', 'noopener,noreferrer')
    }))
    term.open(host.current)
    let socket: WebSocket | undefined
    let resizeTimer: number | undefined
    let ready = false
    let failed = false
    const sendSize = () => {
      if (ready && socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }))
    }
    const resizeObserver = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer)
      resizeTimer = window.setTimeout(() => { if (host.current?.clientWidth && host.current.clientHeight) addon.fit() }, 80)
    })
    resizeObserver.observe(host.current)
    const onSize = term.onResize(sendSize)
    const input = term.onData(data => {
      if (!ready || socket?.readyState !== WebSocket.OPEN || socket.bufferedAmount > 512 * 1024) return
      const encoded = new TextEncoder().encode(data)
      for (let offset = 0; offset < encoded.length; offset += 16 * 1024) socket.send(encoded.subarray(offset, offset + 16 * 1024))
    })
    const selection = term.onSelectionChange(() => setSelected(term.hasSelection()))
    void ssh.ticket(server.id, access, controller.signal).then(({ ticket }) => {
      if (controller.signal.aborted) return
      socket = ssh.terminal(server.id, ticket)
      socket.onmessage = event => {
        if (controller.signal.aborted) return
        if (event.data instanceof ArrayBuffer) { term.write(new Uint8Array(event.data)); return }
        const control = JSON.parse(String(event.data)) as { type: string; detail?: Failure; code?: number }
        if (control.type === 'ready') { ready = true; setStatus('已连接'); sendSize(); callback.current.onConnection() }
        else if (control.type === 'error') { failed = true; setStatus('连接失败'); callback.current.onError(new SshError(control.detail ?? {})) }
        else if (control.type === 'exit') term.writeln('\r\n[退出状态 ' + control.code + ']')
      }
      socket.onclose = () => { if (!controller.signal.aborted && !failed) setStatus('已断开') }
      socket.onerror = () => { if (!controller.signal.aborted && !failed) { failed = true; setStatus('连接失败'); callback.current.onError(new Error('终端 WebSocket 连接失败')) } }
    }).catch(error => {
      if (!controller.signal.aborted) { setStatus('连接失败'); callback.current.onError(error) }
    })
    return () => {
      controller.abort(); window.clearTimeout(resizeTimer); resizeObserver.disconnect()
      if (socket?.readyState === WebSocket.CONNECTING) { socket.onerror = null; socket.onopen = () => socket?.close() }
      else socket?.close()
      onSize.dispose(); input.dispose(); selection.dispose(); term.dispose(); terminal.current = undefined; fit.current = undefined
    }
  }, [server.id, access])
  useEffect(() => { if (terminal.current) terminal.current.options.theme = terminalThemes[theme] }, [theme])
  useEffect(() => {
    if (!active) return
    const frame = requestAnimationFrame(() => { fit.current?.fit(); terminal.current?.focus() })
    return () => cancelAnimationFrame(frame)
  }, [active])
  async function copy() {
    const value = terminal.current?.getSelection()
    if (value) try { await navigator.clipboard.writeText(value) } catch (error) { onError(new Error('无法复制：' + message(error))) }
  }
  function selectForAI() {
    const value = terminal.current?.getSelection().trim().slice(0, 4000)
    if (value && window.confirm('将选中的终端输出加入问答草稿？请先确认没有密钥、密码或其他敏感数据。')) diagnose?.(value)
  }
  return <section className={css.terminalPane} hidden={!active}>
    <div className={css.terminalTools}><span role="status" data-connected={status === '已连接'}>{status}</span><button disabled={!selected} onClick={() => void copy()}>复制选区</button>{diagnose && <button disabled={!selected} onClick={selectForAI}>加入问答草稿</button>}</div>
    <div ref={host} className={css.terminalHost} data-testid="ssh-terminal" />
  </section>
}
