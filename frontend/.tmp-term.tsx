import { useEffect, useRef } from 'react'
import { createRoot } from 'react-dom/client'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'

const family = '"JetBrains Mono", "Cascadia Mono", "DejaVu Sans Mono", "Ubuntu Mono", "Liberation Mono", Menlo, Consolas, monospace'
const SAMPLE = '\r\n\x1b[36m[AI · /nested]\x1b[0m $ ss -ltnp \'sport = :3001\'\r\nps -ef | grep -iE \'codex-workspace|quickagent\' | grep -v -E \'grep\' || true\r\n'
  + 'State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\r\n'
  + 'LISTEN 0       4096    127.0.0.1:3001      0.0.0.0:*      users:(("docker-proxy",pid=1738173,fd=7))\r\n'
  + '\r\n\x1b[32m[exit 0 · 758 ms]\x1b[0m\r\n'

function Pane({ sr, label }: { sr: boolean; label: string }) {
  const host = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const element = host.current
    if (!element) return
    const term = new Terminal({ fontSize: 13, lineHeight: 1.45, fontFamily: family, disableStdin: true, cursorBlink: false, scrollback: 3500, screenReaderMode: sr })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(element)
    term.write(SAMPLE)
    const measure = () => {
      fit.fit()
      const rows = element.querySelector('.xterm-rows')
      const row = element.querySelector('.xterm-rows .xterm-row') as HTMLElement | null
      const probe = document.createElement('span')
      probe.textContent = 'MMMMMMMMMM'
      probe.style.cssText = 'position:absolute;visibility:hidden;font:13px ' + family
      document.body.append(probe)
      const charWidth = probe.getBoundingClientRect().width / 10
      probe.remove()
      const style = rows ? getComputedStyle(rows) : undefined
      const cell = (term as unknown as { _core: { _renderService: { dimensions: { css: { cell: { width: number; height: number } } } } } })._core._renderService.dimensions
      return { label, screenReaderMode: sr, cols: term.cols, rows: term.rows, rowHeight: row?.getBoundingClientRect().height, cellWidth: cell.css.cell.width, cellHeight: cell.css.cell.height, measuredCharWidth: charWidth, rowWidth: rows?.getBoundingClientRect().width, rowsFont: style?.fontFamily, rowsFontSize: style?.fontSize, rowsLetterSpacing: style?.letterSpacing, renderer: (element.querySelector('.xterm-rows')?.children.length ?? 0) }
    }
    ;(window as unknown as { __probe: unknown[] }).__probe = ((window as unknown as { __probe?: unknown[] }).__probe ?? [])
    ;(window as unknown as { __probe: unknown[] }).__probe.push(measure())
    return () => term.dispose()
  }, [sr])
  return <div style={{ padding: 8 }}>
    <p style={{ font: '12px system-ui', color: '#555', margin: '4px 0' }}>{label}</p>
    <div ref={host} style={{ width: 720, height: 220, background: '#f8faf9' }} />
  </div>
}

function App() {
  return <div>
    <Pane sr label="screenReaderMode = true" />
    <Pane sr={false} label="screenReaderMode = false" />
    <div style={{ padding: 8 }}>
      <p style={{ font: '12px system-ui', color: '#555' }}>reference text at 13px same family</p>
      <div style={{ font: '13px ' + family, whiteSpace: 'pre', width: 720 }}>LISTEN 0       4096    127.0.0.1:3001      0.0.0.0:*      users:(("docker-proxy",pid=1738173,fd=7))</div>
    </div>
  </div>
}

createRoot(document.getElementById('root')!).render(<App />)
