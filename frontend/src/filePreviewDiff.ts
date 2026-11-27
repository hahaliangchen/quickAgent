import { diffLines } from 'diff'
import type { AgentEvent } from './api.ts'
import { changeSummaries } from './changes.ts'
import { workspacePath } from './filePreview.ts'

export interface InlineDiffRow {
  readonly kind: 'context' | 'add' | 'del'
  readonly oldLine: number | null
  readonly newLine: number | null
  readonly text: string
}

export interface InlineDiffResult {
  readonly rows: readonly InlineDiffRow[]
  readonly added: number
  readonly deleted: number
  readonly hasChanges: boolean
  /** Row indices where each contiguous changed hunk begins. */
  readonly hunkIndices: readonly number[]
}

export interface SessionFileChangeInfo {
  readonly earliestTurn: number
  readonly latestTurn: number
  readonly entryPath: string
  readonly added: number
  readonly deleted: number
  readonly removed: boolean
}

function normalizeNewlines(value: string): string {
  return value.replace(/\r\n/gu, '\n').replace(/\r/gu, '\n')
}

function splitChunkLines(value: string): string[] {
  if (!value) return []
  const body = value.endsWith('\n') ? value.slice(0, -1) : value
  return body.split('\n')
}

function sameWorkspaceFile(a: string, b: string, root: string): boolean {
  const winMode = /^[A-Za-z]:[\\/]/u.test(root) || /^[A-Za-z]:[\\/]/u.test(a) || /^[A-Za-z]:[\\/]/u.test(b)
  const normA = workspacePath(a, root)
  const normB = workspacePath(b, root)
  return winMode ? normA.toLowerCase() === normB.toLowerCase() : normA === normB
}

/**
 * Find non-reverted turn change snapshots touching `filePath` in this session.
 * Comparing `earliestTurn`'s `oldText` against the live file yields the full
 * session diff in a single snapshot lookup.
 */
export function findSessionFileChange(
  events: readonly AgentEvent[],
  filePath: string,
  root: string,
): SessionFileChangeInfo | undefined {
  const summaries = [...changeSummaries(events).values()].sort((a, b) => a.turn - b.turn)
  let earliestTurn: number | undefined
  let latestTurn: number | undefined
  let entryPath = ''
  let added = 0
  let deleted = 0
  let removed = false

  for (const summary of summaries) {
    for (const item of summary.files) {
      if (item.reverted || item.binary || item.unavailable) continue
      if (!sameWorkspaceFile(item.path, filePath, root)) continue
      if (earliestTurn === undefined) earliestTurn = summary.turn
      latestTurn = summary.turn
      entryPath = item.path
      added += item.added
      deleted += item.deleted
      removed = item.removed
    }
  }

  if (earliestTurn === undefined || latestTurn === undefined) return undefined
  return { earliestTurn, latestTurn, entryPath, added, deleted, removed }
}

/**
 * Build a full-file inline diff between `oldText` (or `null` for a newly created file)
 * and `newText`, preserving 1-based old and new line numbers on every row so callers
 * can render full syntax-highlighted context and still jump directly to `:line`.
 */
export function buildInlineCodeDiff(oldText: string | null, newText: string): InlineDiffResult {
  const normalizedNew = normalizeNewlines(newText)
  if (oldText === null) {
    const newLines = splitChunkLines(normalizedNew)
    const rows: InlineDiffRow[] = newLines.map((text, idx) => ({
      kind: 'add',
      oldLine: null,
      newLine: idx + 1,
      text,
    }))
    return {
      rows,
      added: rows.length,
      deleted: 0,
      hasChanges: rows.length > 0,
      hunkIndices: rows.length > 0 ? [0] : [],
    }
  }

  const normalizedOld = normalizeNewlines(oldText)
  if (normalizedOld === normalizedNew) {
    const lines = splitChunkLines(normalizedNew)
    const rows: InlineDiffRow[] = lines.map((text, idx) => ({
      kind: 'context',
      oldLine: idx + 1,
      newLine: idx + 1,
      text,
    }))
    return {
      rows,
      added: 0,
      deleted: 0,
      hasChanges: false,
      hunkIndices: [],
    }
  }

  const parts = diffLines(normalizedOld, normalizedNew)
  const rows: InlineDiffRow[] = []
  const hunkIndices: number[] = []
  let oldLine = 1
  let newLine = 1
  let added = 0
  let deleted = 0
  let inHunk = false

  for (const part of parts) {
    const lines = splitChunkLines(part.value)
    if (part.added) {
      if (!inHunk && lines.length > 0) {
        hunkIndices.push(rows.length)
        inHunk = true
      }
      for (const text of lines) {
        rows.push({ kind: 'add', oldLine: null, newLine, text })
        newLine += 1
        added += 1
      }
    } else if (part.removed) {
      if (!inHunk && lines.length > 0) {
        hunkIndices.push(rows.length)
        inHunk = true
      }
      for (const text of lines) {
        rows.push({ kind: 'del', oldLine, newLine: null, text })
        oldLine += 1
        deleted += 1
      }
    } else {
      inHunk = false
      for (const text of lines) {
        rows.push({ kind: 'context', oldLine, newLine, text })
        oldLine += 1
        newLine += 1
      }
    }
  }

  return {
    rows,
    added,
    deleted,
    hasChanges: added > 0 || deleted > 0,
    hunkIndices,
  }
}
