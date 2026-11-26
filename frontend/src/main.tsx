import './dsh/web-base.css'
import './dsh/theme/brand-font.css'
import './dsh/theme/base.css'
import './dsh/theme/corner-shape.css'
import './dsh/theme/design-platform.css'
import './dsh/theme/scrollbar.css'
import './dsh/theme/gradient-shadow-text.css'
import './dsh/theme/shiki.css'
import './codex-theme.css'
import type { Context } from '@deepseek-ai/cordis'
import { bootBrowserCordis } from './cordis/runtime.ts'
import { applyStoredTheme } from './theme.ts'

applyStoredTheme()

// Desktop WebView (Tauri/WebKitGTK) does not bind Ctrl+R / F5 by default; wire
// a safe page reload shortcut while leaving Ctrl+R free inside SSH terminals.
window.addEventListener('keydown', (event) => {
  if (event.altKey) return
  const key = event.key.toLowerCase()
  const isF5 = event.key === 'F5'
  const isCtrlR = (event.ctrlKey || event.metaKey) && key === 'r'
  if (!isF5 && !isCtrlR) return
  if (isCtrlR && !event.shiftKey) {
    const target = event.target instanceof Element ? event.target : null
    if (target?.closest('.xterm')) return
  }
  event.preventDefault()
  window.location.reload()
})

function requireRoot(): HTMLElement {
  const element = document.getElementById('root')
  if (element === null) throw new Error('agent web: missing #root')
  return element
}

const root = requireRoot()
let context: Context | undefined
let unmount: (() => void) | undefined
let disposed = false

function showBootError(error: unknown): void {
  console.error('Browser Cordis failed to start', error)
  const message = error instanceof Error ? error.message : String(error)
  const alert = document.createElement('p')
  alert.setAttribute('role', 'alert')
  alert.textContent = `页面插件启动失败：${message}`
  root.replaceChildren(alert)
}

async function start(): Promise<void> {
  try {
    const ctx = await bootBrowserCordis()
    if (disposed) {
      await ctx.fiber.dispose()
      return
    }
    context = ctx
    unmount = ctx.uiRenderer.mount(root)
  } catch (error) {
    if (context !== undefined) await context.fiber.dispose()
    if (!disposed) showBootError(error)
  }
}

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    disposed = true
    unmount?.()
    if (context !== undefined) void context.fiber.dispose()
  })
}

void start()
