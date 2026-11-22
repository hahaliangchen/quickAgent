/**
 * The files a session picked but has not sent.
 *
 * Text written in a box already survives a look at another chat; the picture
 * pasted next to it has to survive with it, or the message is half gone when
 * the reader comes back. So the files live here, by session key, together with
 * the preview URL each one needs — a URL that is released when the file is
 * removed, when the message is sent and when the store forgets, and never
 * because some component happened to unmount.
 *
 * A draft, not storage: a reload starts clean.
 *
 * Kept free of React so it can be unit tested on its own.
 */
import { randomId } from './randomId.ts'

/** One picked file, with the picture preview an image needs. */
export interface DraftAttachment {
  readonly id: string
  readonly file: File
  readonly preview: string | undefined
}

const NONE: readonly DraftAttachment[] = []
/** How many files one message may carry. */
export const ATTACHMENT_LIMIT = 20
/** How large one file may be. */
export const ATTACHMENT_MAX_BYTES = 20 * 1024 * 1024

const drafts = new Map<string, readonly DraftAttachment[]>()
const listeners = new Set<() => void>()

function notify(): void {
  for (const listener of listeners) listener()
}

/** Listen for any session's files changing, so a box redraws when it comes back. */
export function subscribeAttachments(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

/** The files a session has picked; empty for a session that never picked one. */
export function attachmentsOf(key: string): readonly DraftAttachment[] {
  return drafts.get(key) ?? NONE
}

function release(item: DraftAttachment): void {
  if (item.preview !== undefined) URL.revokeObjectURL(item.preview)
}

function put(key: string, items: readonly DraftAttachment[]): void {
  if (items.length === 0) drafts.delete(key)
  else drafts.set(key, items)
  notify()
}

/**
 * Take files into a session's message. Rejected files come back as one line of
 * complaints rather than disappearing, and the oldest files fall off the end
 * once a message is full.
 */
export function addAttachments(key: string, files: Iterable<File>, extensions?: readonly string[]): string {
  const accepted: DraftAttachment[] = []
  const errors: string[] = []
  for (const file of files) {
    const ext = file.name.split('.').at(-1)?.toLowerCase() ?? ''
    if (extensions !== undefined && !extensions.includes(ext)) { errors.push(`不支持 ${file.name}`); continue }
    if (file.size === 0 || file.size > ATTACHMENT_MAX_BYTES) { errors.push(`${file.name} 必须非空且不超过 20 MiB`); continue }
    accepted.push({
      id: randomId(),
      file,
      preview: file.type.startsWith('image/') ? URL.createObjectURL(file) : undefined,
    })
  }
  if (accepted.length > 0) {
    const all = [...attachmentsOf(key), ...accepted]
    for (const overflow of all.slice(ATTACHMENT_LIMIT)) release(overflow)
    put(key, all.slice(0, ATTACHMENT_LIMIT))
  }
  return errors.join('；')
}

/** Take one file back out of a message, and release the preview it held. */
export function removeAttachment(key: string, id: string): void {
  const current = attachmentsOf(key)
  const gone = current.find(item => item.id === id)
  if (gone === undefined) return
  release(gone)
  put(key, current.filter(item => item.id !== id))
}

/**
 * Forget a session's files, as sending does. The caller names the session: a
 * send that finishes while the reader is looking at another chat must forget
 * the files it sent, not the ones being picked there.
 */
export function clearAttachments(key: string): void {
  const current = attachmentsOf(key)
  if (current.length === 0) return
  for (const item of current) release(item)
  put(key, NONE)
}

/** Forget every session's files. The app never needs it; a test does. */
export function forgetAttachments(): void {
  for (const items of drafts.values()) for (const item of items) release(item)
  drafts.clear()
  notify()
}
