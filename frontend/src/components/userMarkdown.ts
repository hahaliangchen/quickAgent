/**
 * Lightweight heuristics for rendering Markdown-formatted user chat messages
 * with the built-in `MarkdownText` renderer while preserving plain text and
 * `@session` / `/skill` reference chips.
 */

const SESSION_WIRE_RE = /@\[[^\]\n]+\]\(dsh-session:[^)\s]+\)/u
const FENCE_RE = /(^|\n)\s*(```|~~~)/u
const INLINE_CODE_RE = /`[^`\n]+`/u
const STRONG_RE = /(\*\*[^\s*](?:[^*\n]*[^\s*])?\*\*|__[^\s_](?:[^_\n]*[^\s_])?__)/u
const HEADING_RE = /(^|\n)#{1,6}\s+\S/u
const BLOCKQUOTE_RE = /(^|\n)>\s+\S/u
const LINK_RE = /\[[^\]\n]+\]\((?:https?:\/\/|file:\/\/|\/|\.\.?\/)[^)\s]+\)/u
const TABLE_RE = /(^|\n)\|[^\n]+\|\r?\n\|[\s:|-]+\|/u
const LIST_LINE_RE = /^\s*(?:[-*+]|\d+[.)])\s+\S/u

/**
 * Returns true when a user message intentionally uses Markdown formatting
 * (code fences, inline code, bold, headings, lists, blockquotes, links, or tables).
 * Messages containing session reference wire syntax stay on `projectUserText`.
 */
export function hasUserMarkdownSyntax(text: string): boolean {
  const trimmed = text.trim()
  if (!trimmed) return false
  if (SESSION_WIRE_RE.test(trimmed)) return false

  if (
    FENCE_RE.test(trimmed)
    || INLINE_CODE_RE.test(trimmed)
    || STRONG_RE.test(trimmed)
    || HEADING_RE.test(trimmed)
    || BLOCKQUOTE_RE.test(trimmed)
    || LINK_RE.test(trimmed)
    || TABLE_RE.test(trimmed)
  ) {
    return true
  }

  // Treat lists as Markdown when there are 2+ list items or a multi-line structured prompt.
  const lines = trimmed.split(/\r?\n/u)
  if (lines.length >= 2) {
    let listLines = 0
    for (const line of lines) {
      if (LIST_LINE_RE.test(line)) listLines += 1
    }
    if (listLines >= 2) return true
  }

  return false
}

const BLOCK_BOUNDARY_RE = /^\s*(?:$|#{1,6}\s|[-*+]\s|\d+[.)]\s|>\s|\||```|~~~|---+\s*$|\*\*\*+\s*$|___+\s*$)/u

/**
 * Preserve single-Enter line breaks inside ordinary prose paragraphs while leaving
 * fenced code blocks, lists, headings, blockquotes, and tables untouched.
 */
export function preserveUserLineBreaks(text: string): string {
  const normalized = text.replace(/\r\n/gu, '\n')
  const lines = normalized.split('\n')
  const out: string[] = []
  let activeFence: string | null = null

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i]!
    const fenceMatch = /^\s*(```+|~~~+)/u.exec(line)
    if (fenceMatch) {
      const marker = fenceMatch[1]![0]!
      if (activeFence === null) {
        activeFence = marker
      } else if (activeFence === marker) {
        activeFence = null
      }
      out.push(line)
      continue
    }

    if (activeFence !== null || i === lines.length - 1) {
      out.push(line)
      continue
    }

    const next = lines[i + 1]!
    const currentIsProse = !BLOCK_BOUNDARY_RE.test(line)
    const nextIsProse = !BLOCK_BOUNDARY_RE.test(next)
    if (currentIsProse && nextIsProse && !line.endsWith('  ')) {
      out.push(`${line}  `)
    } else {
      out.push(line)
    }
  }

  return out.join('\n')
}
