interface TauriInternals {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>
}

/**
 * The operating system's own folder chooser, or `undefined` when this page is
 * not running inside the Grow desktop shell. A browser can't hand a page a real
 * path, so there the caller has to ask for one to be typed.
 *
 * The shell exposes the chooser through its dialog plugin (permission granted
 * in `desktop/capabilities/agent-page.json`). The returned function opens the
 * chooser modal to the window and resolves to the chosen path, or `null` when
 * the user cancels.
 */
export function systemFolderPicker(): ((title: string) => Promise<string | null>) | undefined {
  const internals = (window as Window & { __TAURI_INTERNALS__?: TauriInternals }).__TAURI_INTERNALS__
  if (internals === undefined) return undefined
  return title => internals.invoke<string | null>('plugin:dialog|open', { options: { directory: true, title } })
}
