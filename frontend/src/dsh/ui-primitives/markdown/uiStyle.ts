import { useSyncExternalStore } from 'react'

function subscribeUiStyle(callback: () => void): () => void {
  if (typeof document === 'undefined' || !document.body) return () => {}
  const observer = new MutationObserver(callback)
  observer.observe(document.body, { attributes: true, attributeFilter: ['data-ui-style'] })
  return () => { observer.disconnect() }
}

function getIsPaperStyle(): boolean {
  return typeof document !== 'undefined' && document.body?.getAttribute('data-ui-style') === 'paper'
}

/** True when the UI is in Borderless Paper mode (`body[data-ui-style="paper"]`). */
export function useIsPaperStyle(): boolean {
  return useSyncExternalStore(subscribeUiStyle, getIsPaperStyle, () => false)
}
