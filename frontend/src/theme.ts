// The QuickAgent geometric 4-leaf mark; the SVG follows the system's light or dark itself.
import sproutIcon from './assets/windmill-mark.svg'

const KEY = 'agent-theme'
const STYLE_KEY = 'agent-ui-style'

export type ThemeMode = 'light' | 'dark' | 'system'
export type UiStyle = 'crisp' | 'paper'

export function getThemeMode(): ThemeMode {
  const stored = localStorage.getItem(KEY)
  if (stored === 'light' || stored === 'dark' || stored === 'system') return stored
  return 'system'
}

export function getUiStyle(): UiStyle {
  const stored = localStorage.getItem(STYLE_KEY)
  if (stored === 'crisp' || stored === 'paper') return stored
  return 'crisp'
}

function prefersDark(mode: ThemeMode = getThemeMode()): boolean {
  if (mode === 'dark' || mode === 'light') return mode === 'dark'
  return matchMedia('(prefers-color-scheme: dark)').matches
}

function syncUiStyleAttribute(style: UiStyle): void {
  if (style === 'paper') {
    document.body.setAttribute('data-ui-style', 'paper')
  } else {
    document.body.removeAttribute('data-ui-style')
  }
}

/** The brand mark. */
export const brandIcon = (): string => sproutIcon

export function applyStoredTheme(): void {
  document.body.toggleAttribute('data-ds-dark-theme', prefersDark())
  syncUiStyleAttribute(getUiStyle())
  // The legacy colour theme switch is gone; drop what an earlier version stored.
  localStorage.removeItem('agent-palette')
}

export function setThemeMode(mode: ThemeMode): boolean {
  localStorage.setItem(KEY, mode)
  const dark = prefersDark(mode)
  document.body.toggleAttribute('data-ds-dark-theme', dark)
  return dark
}

export function setUiStyle(style: UiStyle): void {
  localStorage.setItem(STYLE_KEY, style)
  syncUiStyleAttribute(style)
}

export function toggleTheme(): boolean {
  const dark = !document.body.hasAttribute('data-ds-dark-theme')
  return setThemeMode(dark ? 'dark' : 'light')
}
