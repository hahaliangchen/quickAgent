import { useCallback, useEffect, useState } from 'react'
import type { PermissionMode } from './api.ts'

/**
 * A chat that never stored a level asks first. The default is never unsandboxed
 * full access, so a chat that has no level — a new one, or one saved before
 * levels were kept per chat — can only run with a level that asks or is sandboxed.
 */
export const DEFAULT_PERMISSION_MODE: PermissionMode = 'ask'

const MODES: readonly PermissionMode[] = [
  'read_only',
  'workspace_write',
  'ask',
  'auto_review',
  'full_access',
]

/** A level this build knows, told apart from a value another build wrote. */
export function knownPermissionMode(value: unknown): PermissionMode | undefined {
  return typeof value === 'string' ? MODES.find(mode => mode === value) : undefined
}

/**
 * The level a chat runs at: the one stored with its model and thinking level in
 * the backend, which the desktop app and the browser both read. A value this
 * build does not know reads as asking first rather than granting more access.
 */
export function resolvePermissionMode(stored: unknown): PermissionMode {
  return knownPermissionMode(stored) ?? DEFAULT_PERMISSION_MODE
}

/** The chat a composer shows, as the task payload describes it. */
export interface PermissionChat {
  readonly task_id: string
  readonly permission_mode?: string | null
}

/** Save a chat's level; the backend is what both clients read back. */
export type SavePermissionMode = (taskId: string, mode: PermissionMode) => Promise<unknown>

/** The key of a chat that does not exist yet: its level is saved with its first message. */
const NEW_CHAT = ''

/**
 * The level one chat's composer shows and sends. Each chat keeps its own: opening
 * another chat shows that chat's level, and a new chat starts from the default
 * again, so full access chosen in one chat never reaches another.
 *
 * Choosing saves at once, so a reload, an app restart or the other client shows
 * the same level without waiting for a message. A failed save falls back to the
 * level the backend still holds.
 */
export function useChatPermissionMode(
  task: PermissionChat | undefined,
  save: SavePermissionMode,
): { mode: PermissionMode; select: (mode: PermissionMode) => void } {
  const [chosen, setChosen] = useState<Readonly<Record<string, PermissionMode>>>({})
  const key = task?.task_id ?? NEW_CHAT
  const mode = chosen[key] ?? resolvePermissionMode(task?.permission_mode)

  // A level chosen before the chat existed belongs to the chat the first message
  // created, not to the next new chat.
  const chat = task?.task_id
  useEffect(() => {
    if (chat === undefined) return
    setChosen(current => {
      if (!(NEW_CHAT in current)) return current
      const next = { ...current }
      delete next[NEW_CHAT]
      return next
    })
  }, [chat])

  const select = useCallback((next: PermissionMode) => {
    setChosen(current => ({ ...current, [key]: next }))
    if (key === NEW_CHAT) return
    void save(key, next).catch(() => {
      setChosen(current => {
        const restored = { ...current }
        delete restored[key]
        return restored
      })
    })
  }, [key, save])

  return { mode, select }
}
