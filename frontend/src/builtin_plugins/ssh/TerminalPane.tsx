import { useEffect, useRef, useState } from 'react'
import { Terminal, type ITheme } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import '@xterm/xterm/css/xterm.css'
import { type Access, type Failure, type Server, message, ssh, SshError } from './api.ts'
import { appConfirm } from '../../components/AppDialog.tsx'
import css from './style.module.css'

export type OutputStatus = '连接中' | '已连接' | '已断开' | '连接失败'
export const terminalFontFamily = '"JetBrains Mono", "Cascadia Mono", "DejaVu Sans Mono", "Ubuntu Mono", "Liberation Mono", Menlo, Consolas, monospace'

/** Harmonious 16-color ANSI palettes matching QuickAgent's sage/slate design system. */
export const terminalThemes: Record<'dark' | 'light', ITheme> = {
  dark: {
    background: '#121618',
    foreground: '#e2e8e5',
    cursor: '#8adbc1',
    cursorAccent: '#121618',
    selectionBackground: 'rgba(138, 219, 193, 0.26)',
    selectionInactiveBackground: 'rgba(138, 219, 193, 0.14)',
    black: '#1f2624',
    red: '#ef7b70',
    green: '#6fd4b2',
    yellow: '#e5b567',
    blue: '#7cb7f0',
    magenta: '#c997e6',
    cyan: '#68cbd4',
    white: '#c5d1cc',
    brightBlack: '#66756e',
    brightRed: '#ff9b91',
    brightGreen: '#93e5c9',
    brightYellow: '#f2ca85',
    brightBlue: '#9ecbf5',
    brightMagenta: '#dcb5f2',
    brightCyan: '#8be0e8',
    brightWhite: '#f0f5f2',
  },
  light: {
    background: '#f8faf9',
    foreground: '#1c2421',
    cursor: '#146c53',
    cursorAccent: '#f8faf9',
    selectionBackground: 'rgba(20, 108, 83, 0.2)',
    selectionInactiveBackground: 'rgba(20, 108, 83, 0.1)',
    black: '#25302c',
    red: '#b9382c',
    green: '#146c53',
    yellow: '#946200',
    blue: '#1f5f9e',
    magenta: '#7a3e8f',
    cyan: '#0f6f7c',
    white: '#5d6a64',
    brightBlack: '#6b7a73',
    brightRed: '#d14334',
    brightGreen: '#1c8566',
    brightYellow: '#b37400',
    brightBlue: '#2c75bd',
    brightMagenta: '#9452ab',
    brightCyan: '#148896',
    brightWhite: '#1c2421',
  },
}

export default function TerminalPane({ server, access, active, theme, onError, diagnose, onConnection }: {
  server: Server; access: Access; active: boolean; theme: 'dark' | 'light'; onError: (error: unknown) => void; diagnose?: ((value: string) => void) | undefined; onConnection: () => void
}) {
  const host = useRef<HTMLDivElement>(null)
  const terminal = useRef<Terminal>()
  const fit = useRef<FitAddon>()
  const callback = useRef({ onError, onConnection })
  callback.current = { onError, onConnection }
  const [status, setStatus] = useState<OutputStatus>('连接中')
  const [attempt, setAttempt] = useState(0)
  const [selected, setSelected] = useState(false)

  useEffect(() => {
    if (!host.current) return
    const controller = new AbortController()
    setStatus('连接中')
    setSelected(false)
    const term = new Terminal({
      fontSize: 13,
      lineHeight: 1.5,
      letterSpacing: 0.2,
      fontFamily: terminalFontFamily,
      fontWeight: '400',
      fontWeightBold: '600',
      cursorStyle: 'bar',
      cursorWidth: 2,
      cursorBlink: true,
      scrollback: 5000,
      smoothScrollDuration: 100,
      theme: terminalThemes[theme],
    })
    const addon = new FitAddon()
    terminal.current = term
    fit.current = addon
    term.loadAddon(addon)
    term.loadAddon(new WebLinksAddon((event, uri) => {
      if (!event.isTrusted || !/^https?:\/\//i.test(uri)) return
      void appConfirm({
        title: '打开外部链接',
        message: '打开终端中的外部链接？\n' + uri,
        confirmText: '打开链接',
      }).then(ok => {
        if (ok) window.open(uri, '_blank', 'noopener,noreferrer')
      })
    }))
    term.open(host.current)

    let socket: WebSocket | undefined
    let resizeTimer: number | undefined
    let ready = false
    let failed = false

    const sendBytes = (data: string) => {
      if (!ready || socket?.readyState !== WebSocket.OPEN || socket.bufferedAmount > 512 * 1024) return
      const encoded = new TextEncoder().encode(data)
      for (let offset = 0; offset < encoded.length; offset += 16 * 1024) {
        socket.send(encoded.subarray(offset, offset + 16 * 1024))
      }
    }

    term.attachCustomKeyEventHandler(event => {
      if (event.type !== 'keydown') return true
      if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === 'c') {
        const value = term.getSelection()
        if (value) {
          void navigator.clipboard.writeText(value).catch(error => callback.current.onError(new Error('无法复制：' + message(error))))
          return false
        }
      }
      if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === 'v') {
        void navigator.clipboard.readText().then(text => {
          if (text) sendBytes(text)
        }).catch(error => callback.current.onError(new Error('无法粘贴：' + message(error))))
        return false
      }
      return true
    })

    const sendSize = () => {
      if (ready && socket?.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }))
      }
    }
    const resizeObserver = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer)
      resizeTimer = window.setTimeout(() => {
        if (host.current?.clientWidth && host.current.clientHeight) addon.fit()
      }, 80)
    })
    resizeObserver.observe(host.current)
    const onSize = term.onResize(sendSize)
    const input = term.onData(sendBytes)
    const selection = term.onSelectionChange(() => setSelected(term.hasSelection()))

    void ssh.ticket(server.id, access, controller.signal).then(({ ticket }) => {
      if (controller.signal.aborted) return
      socket = ssh.terminal(server.id, ticket)
      socket.onmessage = event => {
        if (controller.signal.aborted) return
        if (event.data instanceof ArrayBuffer) {
          term.write(new Uint8Array(event.data))
          return
        }
        const control = JSON.parse(String(event.data)) as { type: string; detail?: Failure; code?: number }
        if (control.type === 'ready') {
          ready = true
          setStatus('已连接')
          sendSize()
          callback.current.onConnection()
        } else if (control.type === 'error') {
          failed = true
          setStatus('连接失败')
          callback.current.onError(new SshError(control.detail ?? {}))
        } else if (control.type === 'exit') {
          term.writeln('\r\n\x1b[90m[会话已退出，状态码 ' + control.code + ']\x1b[0m')
        }
      }
      socket.onclose = () => { if (!controller.signal.aborted && !failed) setStatus('已断开') }
      socket.onerror = () => {
        if (!controller.signal.aborted && !failed) {
          failed = true
          setStatus('连接失败')
          callback.current.onError(new Error('终端 WebSocket 连接失败'))
        }
      }
    }).catch(error => {
      if (!controller.signal.aborted) {
        setStatus('连接失败')
        callback.current.onError(error)
      }
    })

    return () => {
      controller.abort()
      window.clearTimeout(resizeTimer)
      resizeObserver.disconnect()
      if (socket?.readyState === WebSocket.CONNECTING) {
        socket.onerror = null
        socket.onopen = () => socket?.close()
      } else {
        socket?.close()
      }
      onSize.dispose()
      input.dispose()
      selection.dispose()
      term.dispose()
      terminal.current = undefined
      fit.current = undefined
    }
  }, [server.id, access, attempt])

  useEffect(() => {
    if (terminal.current) terminal.current.options.theme = terminalThemes[theme]
  }, [theme])

  useEffect(() => {
    if (!active) return
    const frame = requestAnimationFrame(() => {
      fit.current?.fit()
      terminal.current?.focus()
    })
    return () => cancelAnimationFrame(frame)
  }, [active])

  async function copy() {
    const value = terminal.current?.getSelection()
    if (value) {
      try {
        await navigator.clipboard.writeText(value)
      } catch (error) {
        onError(new Error('无法复制：' + message(error)))
      }
    }
  }

  function clearScreen() {
    terminal.current?.clear()
    terminal.current?.focus()
  }

  async function selectForAI() {
    const value = terminal.current?.getSelection().trim().slice(0, 4000)
    if (!value) return
    const ok = await appConfirm({
      title: '加入问答草稿',
      message: '将选中的终端输出加入问答草稿？请先确认没有密钥、密码或其他敏感数据。',
      confirmText: '加入草稿',
    })
    if (ok) diagnose?.(value)
  }

  return (
    <section className={css.terminalPane} hidden={!active}>
      <div className={css.terminalTools}>
        <strong className={css.humanBadge}>交互终端</strong>
        <span className={css.terminalTarget} title={`${server.username}@${server.host}:${server.port}`}>
          {server.username}@{server.host}
        </span>
        <span
          role="status"
          className={css.statusPill}
          data-connected={status === '已连接'}
          data-failed={status === '连接失败'}
        >
          {status}
        </span>
        {(status === '已断开' || status === '连接失败') && (
          <button onClick={() => setAttempt(value => value + 1)}>重新连接</button>
        )}
        <button onClick={clearScreen} title="清空当前屏幕输出">清屏</button>
        <button disabled={!selected} onClick={() => void copy()} title="复制选区 (Ctrl+Shift+C)">复制选区</button>
        {diagnose && (
          <button disabled={!selected} onClick={() => void selectForAI()}>加入问答草稿</button>
        )}
      </div>
      <div className={css.terminalCard}>
        <div ref={host} className={css.terminalHost} data-testid="ssh-terminal" />
      </div>
    </section>
  )
}
