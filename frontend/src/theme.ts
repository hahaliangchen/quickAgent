// The pinwheel sits on a deep green tile: its pale folds vanish on a white page.
import sproutIcon from './assets/pinwheel-tile.svg'

const KEY = 'agent-theme'

export type ThemeMode = 'light' | 'dark' | 'system'

export function getThemeMode(): ThemeMode {
  const stored = localStorage.getItem(KEY)
  if (stored === 'light' || stored === 'dark' || stored === 'system') return stored
  return 'system'
}

function prefersDark(mode: ThemeMode = getThemeMode()): boolean {
  if (mode === 'dark' || mode === 'light') return mode === 'dark'
  return matchMedia('(prefers-color-scheme: dark)').matches
}

/** The brand mark. */
export const brandIcon = (): string => sproutIcon

export function applyStoredTheme(): void {
  document.body.toggleAttribute('data-ds-dark-theme', prefersDark())
  // The colour theme switch is gone; drop what an earlier version stored.
  localStorage.removeItem('agent-palette')
}

export function setThemeMode(mode: ThemeMode): boolean {
  localStorage.setItem(KEY, mode)
  const dark = prefersDark(mode)
  document.body.toggleAttribute('data-ds-dark-theme', dark)
  return dark
}

export function toggleTheme(): boolean {
  const dark = !document.body.hasAttribute('data-ds-dark-theme')
  return setThemeMode(dark ? 'dark' : 'light')
}
