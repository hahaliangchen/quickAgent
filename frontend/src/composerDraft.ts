/**
 * What each session has typed into its box but not sent.
 *
 * Leaving a half-written message to go and read another conversation, then
 * coming back to an empty box, throws away work the reader already did: the
 * text belongs to the session, not to the component that happens to show it.
 * So it is kept here, by session key, for as long as the page lives. It is a
 * draft, not storage — a reload starts clean, and sending, clearing or emptying
 * the box forgets it.
 *
 * Attachments are deliberately not kept: a File and its preview URL are
 * released when the composer goes away rather than pinned in memory by a draft.
 *
 * Kept free of React so it can be unit tested on its own.
 */
const drafts = new Map<string, string>()

/** Text a box holds, and the session it belongs to. */
export interface HeldDraft {
  readonly key: string
  readonly text: string
}

/** What a session left in its box. */
export function draftOf(key: string): string {
  return drafts.get(key) ?? ''
}

/** Remember the text of a session's box; an empty box is a forgotten draft. */
export function keepDraft(key: string, text: string): void {
  if (text === '') drafts.delete(key)
  else drafts.set(key, text)
}

/**
 * What a box shows: the text it already holds for this session, or the text the
 * session it has just been handed left behind.
 */
export function draftFor(key: string, held: HeldDraft): HeldDraft {
  return held.key === key ? held : { key, text: draftOf(key) }
}

/** Forget every draft. The app never needs it; a test does. */
export function forgetDrafts(): void {
  drafts.clear()
}
