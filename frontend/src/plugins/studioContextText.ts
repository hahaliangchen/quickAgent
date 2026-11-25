import type { StudioViewContext } from './protocol.ts'

export interface ParsedStudioViewContext {
  readonly label: string
  readonly text: string
}

const OPEN_TAG_RE = /^<studio-view-context(?:\s+label="([^"]*)")?>\s*\n?([\s\S]*?)\n?<\/studio-view-context>(?:\s*\n\n?([\s\S]*))?$/

/**
 * Legacy format used before `<studio-view-context>` tags were introduced:
 * `[当前工作台视图上下文 — <label>]\n...\n\n<user text>`
 * Because the body may itself contain fenced code blocks and blank lines,
 * locate the final double-newline outside code fences when splitting legacy messages.
 */
function splitLegacyStudioContext(raw: string): { context?: ParsedStudioViewContext; body: string } | null {
  const header = /^\[当前工作台视图上下文\s*—\s*([^\]]+)\]\r?\n/.exec(raw)
  if (!header) return null
  const label = header[1]?.trim() || '工作台视图上下文'
  const rest = raw.slice(header[0].length)
  const lines = rest.split(/\r?\n/)
  let inFence = false
  let lastSeparator = -1
  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i]!.trim()
    if (trimmed.startsWith('```')) {
      inFence = !inFence
      continue
    }
    if (!inFence && trimmed === '' && i + 1 < lines.length && lines[i + 1]!.trim() !== '') {
      const next = lines[i + 1]!.trim()
      const looksLikeContextLine = /^(?:连接名称:|当前表:|当前 SQL:|结果列|当前页|用户选中|表结构|```)/.test(next)
      if (!looksLikeContextLine) lastSeparator = i
    }
  }
  if (lastSeparator >= 0) {
    const contextBody = lines.slice(0, lastSeparator).join('\n').trim()
    const userBody = lines.slice(lastSeparator + 1).join('\n').trim()
    return { context: { label, text: contextBody }, body: userBody }
  }
  return { context: { label, text: rest.trim() }, body: '' }
}

/** Serialize an attached studio view snapshot so the model receives full context while the UI keeps it separate from the user's bubble. */
export function formatStudioViewContext(ctx: StudioViewContext): string {
  const label = `${ctx.badge}${ctx.detail ? ` · ${ctx.detail}` : ''}`.replace(/"/g, "'")
  return `<studio-view-context label="${label}">\n${ctx.text.trim()}\n</studio-view-context>`
}

/** Separate an attached studio view snapshot (new or legacy) from the user's actual message text. */
export function splitStudioViewContext(raw: string): { context?: ParsedStudioViewContext; body: string } {
  const trimmed = raw.trimStart()
  const match = OPEN_TAG_RE.exec(trimmed)
  if (match) {
    const label = (match[1] ?? '').trim() || '工作台视图上下文'
    const text = (match[2] ?? '').trim()
    const body = (match[3] ?? '').trim()
    return { context: { label, text }, body }
  }
  const legacy = splitLegacyStudioContext(trimmed)
  if (legacy) return legacy
  return { body: raw }
}

export function stripStudioViewContext(raw: string): string {
  const split = splitStudioViewContext(raw)
  return split.body || (split.context ? `[${split.context.label}]` : raw)
}
