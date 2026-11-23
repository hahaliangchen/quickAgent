/**
 * What each session has typed into its box but not sent.
 *
 * Leaving a half-written message to go and read another conversation, then
 * coming back to an empty box, throws away work the reader already did: the
 * text belongs to the session, not to the component that happens to show it.
 * Text for existing sessions stays in memory. The unsent new-chat text also
 * survives refresh in this tab, without creating a task or storing it on the server.
 * Files are handled separately by the in-memory attachment draft store.
 */
export const NEW_CHAT_DRAFT = 'new-chat'
const NEW_CHAT_STORAGE_KEY = 'codex_new_chat_draft'
const drafts = new Map<string, string>()
/** Text a box holds, and the session it belongs to. */
export interface HeldDraft {
  readonly key: string
  readonly text: string
}

/** What a session left in its box. */
export function draftOf(key: string): string {
  const text = drafts.get(key)
  if (text !== undefined || key !== NEW_CHAT_DRAFT) return text ?? ''
  try { return sessionStorage.getItem(NEW_CHAT_STORAGE_KEY) ?? '' } catch { return '' }
}

/** Remember the text of a session's box; an empty box is a forgotten draft. */
export function keepDraft(key: string, text: string): void {
  if (text === '') drafts.delete(key)
  else drafts.set(key, text)
  if (key !== NEW_CHAT_DRAFT) return
  try {
    if (text === '') sessionStorage.removeItem(NEW_CHAT_STORAGE_KEY)
    else sessionStorage.setItem(NEW_CHAT_STORAGE_KEY, text)
  } catch { /* Storage can be disabled; navigation still uses the in-memory draft. */ }
}

/**
 * What a box shows: the text it already holds for this session, or the text the
 * session it has just been handed left behind.
 */
export function draftFor(key: string, held: HeldDraft): HeldDraft {
  return held.key === key ? held : { key, text: draftOf(key) }
}

/** Forget every text draft. */
export function forgetDrafts(): void {
  keepDraft(NEW_CHAT_DRAFT, '')
  drafts.clear()
}
