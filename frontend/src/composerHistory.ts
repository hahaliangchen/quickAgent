export const HISTORY_LIMIT = 50
export const HISTORY_STORAGE_KEY = 'dsh_composer_history'

type HistoryStorage = Pick<Storage, 'getItem' | 'setItem'>

/** Entries are ordered oldest to newest, as they were successfully sent. */
export function appendHistory(entries: readonly string[], message: string): readonly string[] {
  const text = message.trim()
  if (!text || entries.at(-1) === text) return entries
  return [...entries.slice(1 - HISTORY_LIMIT), text]
}

export function createComposerHistoryStore(getStorage: () => HistoryStorage = () => globalThis.localStorage) {
  let fallback: readonly string[] = []
  let writeFailed = false
  const read = (): readonly string[] => {
    if (writeFailed) return fallback
    try {
      const raw: unknown = JSON.parse(getStorage().getItem(HISTORY_STORAGE_KEY) ?? '[]')
      fallback = Array.isArray(raw) ? raw.reduce<readonly string[]>((items, entry: unknown) =>
        typeof entry === 'string' ? appendHistory(items, entry) : items, []) : []
    } catch { /* A malformed value or unavailable storage must not block typing. */ }
    return fallback
  }
  return {
    read,
    record(message: string): void {
      const entries = read()
      const next = appendHistory(entries, message)
      if (next === entries) return
      fallback = next
      try {
        getStorage().setItem(HISTORY_STORAGE_KEY, JSON.stringify(next))
        writeFailed = false
      } catch { writeFailed = true }
    },
  }
}

export const composerHistory = createComposerHistoryStore()

export interface HistoryText {
  readonly text: string
  readonly caret: number
}

export interface HistoryCursor {
  readonly entries: readonly string[]
  readonly index: number
  readonly draft: HistoryText
}

export interface HistoryStep extends HistoryText {
  readonly cursor: HistoryCursor | null
}

export function stepHistoryUp(entries: readonly string[], cursor: HistoryCursor | null, draft: HistoryText): HistoryStep | null {
  if (cursor === null && entries.length === 0) return null
  const next = cursor === null ? { entries, draft, index: entries.length - 1 }
    : { ...cursor, index: Math.max(0, cursor.index - 1) }
  const text = next.entries[next.index]!
  const firstBreak = text.indexOf('\n')
  return { cursor: next, text, caret: firstBreak === -1 ? text.length : firstBreak }
}

export function stepHistoryDown(cursor: HistoryCursor | null): HistoryStep | null {
  if (cursor === null) return null
  const index = cursor.index + 1
  const text = cursor.entries[index]
  if (text === undefined) return { cursor: null, ...cursor.draft }
  return { cursor: { ...cursor, index }, text, caret: text.length }
}

interface HistoryKey {
  readonly key: string
  readonly keyCode: number
  readonly isComposing: boolean
  readonly shiftKey: boolean
  readonly ctrlKey: boolean
  readonly altKey: boolean
  readonly metaKey: boolean
}

export function isCompositionKey(event: Pick<HistoryKey, 'isComposing' | 'keyCode'>): boolean {
  return event.isComposing || event.keyCode === 229
}

/** Check logical textarea lines; wrapped display lines do not alter the draft. */
export function historyDirection(
  event: HistoryKey,
  { value, selectionStart, selectionEnd }: Pick<HTMLTextAreaElement, 'value' | 'selectionStart' | 'selectionEnd'>,
  navigating: boolean,
  menuOpen: boolean,
): 'up' | 'down' | null {
  if (menuOpen || isCompositionKey(event) || event.shiftKey || event.ctrlKey || event.altKey || event.metaKey
    || selectionStart !== selectionEnd) return null
  if (event.key === 'ArrowUp' && !value.slice(0, selectionStart).includes('\n')
    && (value === '' || navigating || selectionStart === 0)) return 'up'
  if (event.key === 'ArrowDown' && navigating && !value.slice(selectionStart).includes('\n')) return 'down'
  return null
}
