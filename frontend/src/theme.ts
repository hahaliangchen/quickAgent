// The windmill silhouette; the file follows the system's light or dark itself.
import sproutIcon from './assets/windmill-mark.svg'

const KEY = 'agent-theme'

export type ThemeMode = 'light' | 'dark' | 'system' | 'garden'

export function getThemeMode(): ThemeMode {
  const stored = localStorage.getItem(KEY)
  if (stored === 'light' || stored === 'dark' || stored === 'system' || stored === 'garden') return stored
  return 'system'
}

function prefersDark(mode: ThemeMode = getThemeMode()): boolean {
  if (mode === 'dark' || mode === 'light' || mode === 'garden') return mode === 'dark'
  return matchMedia('(prefers-color-scheme: dark)').matches
}

/** The brand mark. */
export const brandIcon = (): string => sproutIcon

export function applyStoredTheme(): void {
  document.body.toggleAttribute('data-ds-dark-theme', prefersDark())
  document.body.toggleAttribute('data-garden-theme', getThemeMode() === 'garden')
  // The colour theme switch is gone; drop what an earlier version stored.
  localStorage.removeItem('agent-palette')
}

export function setThemeMode(mode: ThemeMode): boolean {
  localStorage.setItem(KEY, mode)
  const dark = prefersDark(mode)
  document.body.toggleAttribute('data-ds-dark-theme', dark)
  document.body.toggleAttribute('data-garden-theme', mode === 'garden')
  return dark
}

export function toggleTheme(): boolean {
  const dark = !document.body.hasAttribute('data-ds-dark-theme')
  return setThemeMode(dark ? 'dark' : 'light')
}
