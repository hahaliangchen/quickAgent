/** Lightweight semantic classification, execution grouping, and token styling for chat Markdown code & output blocks. */

export type CodeBlockVariant = 'diagnostic' | 'terminal' | 'source' | 'plain'
export type SemanticTone = 'neutral' | 'success' | 'error' | 'warn'

export interface CodeBlockBadge {
  readonly text: string
  readonly tone: SemanticTone
}

export interface CodeBlockMeta {
  readonly variant: CodeBlockVariant
  readonly tone: SemanticTone
  readonly label?: string | undefined
  readonly badge?: CodeBlockBadge | undefined
  readonly compact: boolean
}

export type TerminalTokenKind = 'plain' | 'prompt' | 'command' | 'pass' | 'fail' | 'muted'

export interface TerminalToken {
  readonly kind: TerminalTokenKind
  readonly text: string
}

export interface ExecutionEntryData {
  readonly command: string
  readonly raw: string
  readonly outputLines: readonly string[]
  readonly exitCode?: number | undefined
  readonly passed?: number | undefined
  readonly failed?: number | undefined
  readonly ignored?: number | undefined
  readonly tone: SemanticTone
  readonly statusText: string
}

export interface GroupableBlockDescriptor {
  readonly type: string
  readonly lang?: string | undefined
  readonly value?: string | undefined
  readonly text?: string | undefined
}

export interface ExecutionAttachmentRef {
  readonly kind: 'note' | 'diagnostic'
  readonly blockIndex: number
  readonly tone?: SemanticTone | undefined
  readonly text?: string | undefined
}

export interface ExecutionGroupEntryRef {
  readonly blockIndex: number
  readonly entry: ExecutionEntryData
  readonly attachments: readonly ExecutionAttachmentRef[]
}

export type GroupedBlockItem =
  | { readonly kind: 'single'; readonly blockIndex: number }
  | {
    readonly kind: 'execution-group'
    readonly startIndex: number
    readonly endIndex: number
    readonly entries: readonly ExecutionGroupEntryRef[]
  }

const SHELL_LANG_RE = /^(sh|bash|zsh|shell|console|terminal|cmd|powershell|ps1)$/i
const PLAIN_LANG_RE = /^(text|txt|plain|output|log|stdout|stderr|error)?$/i
const COMMAND_START_RE = /^(?:\$\s+|>\s+)?(?:cargo|rustc|rustfmt|clippy|npm|npx|pnpm|yarn|node|bun|deno|git|python3?|pytest|pip|uv|go|make|cmake|rg|grep|find|ls|cat|sed|awk|curl|docker|kubectl|bash|sh|NODE_OPTIONS=)\b/
const DIAGNOSTIC_PREFIX_RE = /^(?:[A-Z][A-Z0-9_]{3,}:|error(?:\[E\d+\])?:|warning(?:\[[^\]]+\])?:|fatal:|panic:|thread '[^']+' panicked)/
const EXIT_CODE_LINE_RE = /^\s*(?:exit\s*code|退出码)\s*[:：=]\s*(-?\d+)\s*$/i

function extractExitCode(text: string): number | undefined {
  const match = text.match(/(?:^|\n)\s*(?:exit\s*code|退出码)\s*[:：=]\s*(-?\d+)\b/i)
  const raw = match?.[1]
  if (raw === undefined) return undefined
  const parsed = Number.parseInt(raw, 10)
  return Number.isNaN(parsed) ? undefined : parsed
}

function extractTestCounts(text: string): {
  passed?: number | undefined
  failed?: number | undefined
  ignored?: number | undefined
} {
  const passedRaw = text.match(/\b(\d+)\s+passed\b/i)?.[1]
  const failedRaw = text.match(/\b(\d+)\s+failed\b/i)?.[1]
  const ignoredRaw = text.match(/\b(\d+)\s+ignored\b/i)?.[1]
  const passed = passedRaw !== undefined ? Number.parseInt(passedRaw, 10) : undefined
  const failed = failedRaw !== undefined ? Number.parseInt(failedRaw, 10) : undefined
  const ignored = ignoredRaw !== undefined ? Number.parseInt(ignoredRaw, 10) : undefined
  return {
    passed: passed !== undefined && !Number.isNaN(passed) ? passed : undefined,
    failed: failed !== undefined && !Number.isNaN(failed) ? failed : undefined,
    ignored: ignored !== undefined && !Number.isNaN(ignored) ? ignored : undefined,
  }
}

/** Classify a settled or streaming chat Markdown fence into a visual variant without mutating its text. */
export function classifyCodeBlock(trimmed: string, lang: string | undefined): CodeBlockMeta {
  const normalizedLang = (lang ?? '').trim()
  const lines = trimmed.split('\n')
  const lineCount = lines.length
  const firstNonEmpty = lines.find(line => line.trim() !== '')?.trim() ?? ''
  const isShellLang = normalizedLang !== '' && SHELL_LANG_RE.test(normalizedLang)
  const isPlainLang = PLAIN_LANG_RE.test(normalizedLang)

  if (!isShellLang && !isPlainLang) {
    return {
      variant: 'source',
      tone: 'neutral',
      label: normalizedLang.toUpperCase(),
      compact: lineCount <= 3,
    }
  }

  const exitCode = extractExitCode(trimmed)
  const counts = extractTestCounts(trimmed)
  const hasCommandLine = lines.some(line => COMMAND_START_RE.test(line.trim()))
  const hasTestSummary = counts.passed !== undefined || counts.failed !== undefined

  if (
    !isShellLang
    && lineCount <= 2
    && trimmed.length <= 240
    && exitCode === undefined
    && !hasCommandLine
    && !hasTestSummary
    && DIAGNOSTIC_PREFIX_RE.test(firstNonEmpty)
  ) {
    const isWarn = /^warn(?:ing)?(?:\[|\s*:)/i.test(firstNonEmpty)
    return {
      variant: 'diagnostic',
      tone: isWarn ? 'warn' : 'error',
      compact: true,
    }
  }

  if (isShellLang || exitCode !== undefined || hasCommandLine || hasTestSummary) {
    const failedCount = counts.failed ?? 0
    const passedCount = counts.passed
    const isError = (exitCode !== undefined && exitCode !== 0)
      || failedCount > 0
      || /(?:^|\n)\s*(?:FAILED|ERROR:|error\[E\d+\]:)/.test(trimmed)
    const isSuccess = !isError && (exitCode === 0 || (passedCount !== undefined && passedCount > 0))

    let badge: CodeBlockBadge | undefined
    if (isError) {
      const parts: string[] = []
      if (exitCode !== undefined && exitCode !== 0) parts.push(`exit ${exitCode}`)
      if (failedCount > 0) parts.push(`${failedCount} failed`)
      if (parts.length > 0) {
        badge = { text: `✕ ${parts.join(' · ')}`, tone: 'error' }
      }
    } else if (isSuccess) {
      const parts: string[] = []
      if (exitCode === 0) parts.push('exit 0')
      if (passedCount !== undefined) parts.push(`${passedCount} passed`)
      if (parts.length > 0) {
        badge = { text: `✓ ${parts.join(' · ')}`, tone: 'success' }
      }
    }

    return {
      variant: 'terminal',
      tone: isError ? 'error' : isSuccess ? 'success' : 'neutral',
      label: 'TERMINAL',
      badge,
      compact: lineCount <= 4,
    }
  }

  return {
    variant: 'plain',
    tone: 'neutral',
    compact: lineCount <= 3,
  }
}

/** Parse one terminal code block into structured fields for the unified execution panel. */
export function parseExecutionEntry(trimmed: string, lang: string | undefined): ExecutionEntryData | undefined {
  const meta = classifyCodeBlock(trimmed, lang)
  if (meta.variant !== 'terminal') return undefined

  const lines = trimmed.split('\n')
  const exitCode = extractExitCode(trimmed)
  const counts = extractTestCounts(trimmed)

  let cmdIndex = lines.findIndex(line => COMMAND_START_RE.test(line.trim()))
  if (cmdIndex === -1) {
    cmdIndex = lines.findIndex(line => line.trim() !== '')
  }
  const rawCommand = (cmdIndex >= 0 ? lines[cmdIndex] : '') ?? ''
  const command = rawCommand.trim().replace(/^(?:\$\s+|>\s+)/, '')

  const outputLines = lines.filter((line, index) => {
    if (index === cmdIndex) return false
    if (EXIT_CODE_LINE_RE.test(line)) return false
    return true
  })
  // Trim leading/trailing empty lines from output body only.
  while (outputLines.length > 0 && outputLines[0]?.trim() === '') outputLines.shift()
  while (outputLines.length > 0 && outputLines[outputLines.length - 1]?.trim() === '') outputLines.pop()

  const statusText = meta.tone === 'error'
    ? 'failed'
    : meta.tone === 'success'
      ? 'passed'
      : 'output'

  return {
    command: command || 'command',
    raw: trimmed,
    outputLines,
    exitCode,
    passed: counts.passed,
    failed: counts.failed,
    ignored: counts.ignored,
    tone: meta.tone,
    statusText,
  }
}

const MAX_BRIDGE_NOTE_CHARS = 220

function classifyDescriptor(block: GroupableBlockDescriptor): CodeBlockMeta | undefined {
  if (block.type !== 'code') return undefined
  const value = (block.value ?? '').replace(/\n$/, '')
  if (value.trim() === '') return undefined
  return classifyCodeBlock(value, block.lang)
}

function isShortParagraph(block: GroupableBlockDescriptor | undefined): boolean {
  if (block === undefined || block.type !== 'paragraph') return false
  const text = (block.text ?? '').trim()
  return text.length > 0 && text.length <= MAX_BRIDGE_NOTE_CHARS
}

/**
 * Group consecutive settled terminal code blocks (plus inline diagnostic callouts and
 * short diagnostic notes immediately bridging them) into unified execution panels.
 * Emits a group only when at least 2 terminal commands appear in one run.
 */
export function groupExecutionBlocks(blocks: readonly GroupableBlockDescriptor[]): GroupedBlockItem[] {
  const result: GroupedBlockItem[] = []
  let i = 0

  while (i < blocks.length) {
    const block = blocks[i]
    const meta = block === undefined ? undefined : classifyDescriptor(block)
    if (block === undefined || meta?.variant !== 'terminal') {
      result.push({ kind: 'single', blockIndex: i })
      i += 1
      continue
    }

    const firstTrimmed = (block.value ?? '').replace(/\n$/, '')
    const firstEntry = parseExecutionEntry(firstTrimmed, block.lang)
    if (firstEntry === undefined) {
      result.push({ kind: 'single', blockIndex: i })
      i += 1
      continue
    }

    const startIndex = i
    const entries: Array<{ blockIndex: number; entry: ExecutionEntryData; attachments: ExecutionAttachmentRef[] }> = [
      { blockIndex: i, entry: firstEntry, attachments: [] },
    ]
    let cursor = i + 1

    while (cursor < blocks.length) {
      const next = blocks[cursor]
      if (next === undefined) break
      const nextMeta = classifyDescriptor(next)

      if (nextMeta?.variant === 'terminal') {
        const nextEntry = parseExecutionEntry((next.value ?? '').replace(/\n$/, ''), next.lang)
        if (nextEntry === undefined) break
        entries.push({ blockIndex: cursor, entry: nextEntry, attachments: [] })
        cursor += 1
        continue
      }

      const current = entries[entries.length - 1]
      if (current === undefined) break

      if (nextMeta?.variant === 'diagnostic') {
        current.attachments.push({
          kind: 'diagnostic',
          blockIndex: cursor,
          tone: nextMeta.tone,
          text: (next.value ?? '').replace(/\n$/, ''),
        })
        cursor += 1
        continue
      }

      if (isShortParagraph(next)) {
        const after = blocks[cursor + 1]
        const afterMeta = after === undefined ? undefined : classifyDescriptor(after)

        // Case A: short explanation immediately introducing a diagnostic error block.
        if (after !== undefined && afterMeta?.variant === 'diagnostic') {
          current.attachments.push({ kind: 'note', blockIndex: cursor })
          current.attachments.push({
            kind: 'diagnostic',
            blockIndex: cursor + 1,
            tone: afterMeta.tone,
            text: (after.value ?? '').replace(/\n$/, ''),
          })
          cursor += 2
          continue
        }

        // Case B: short follow-up note right after a diagnostic attachment, followed by another terminal command.
        const lastAttachment = current.attachments[current.attachments.length - 1]
        if (lastAttachment?.kind === 'diagnostic' && after !== undefined && afterMeta?.variant === 'terminal') {
          current.attachments.push({ kind: 'note', blockIndex: cursor })
          cursor += 1
          continue
        }
      }

      break
    }

    if (entries.length >= 2) {
      result.push({
        kind: 'execution-group',
        startIndex,
        endIndex: cursor - 1,
        entries,
      })
      i = cursor
    } else {
      result.push({ kind: 'single', blockIndex: i })
      i += 1
    }
  }

  return result
}

const TERMINAL_HIGHLIGHT_RE = /(\b\d+\s+passed\b|\b\d+\s+failed\b|\b\d+\s+ignored\b|\bexit\s*code\s*[:：=]\s*-?\d+|\b退出码\s*[:：=]\s*-?\d+|\bPASS(?:ED)?\b|\bFAIL(?:ED)?\b)/gi

/** Split one terminal output line into verbatim text spans (joining `.text` always equals the input line). */
export function tokenizeTerminalLine(line: string): TerminalToken[] {
  if (line === '') return [{ kind: 'plain', text: '' }]

  const trimmed = line.trimStart()
  const leadLen = line.length - trimmed.length
  const lead = leadLen > 0 ? line.slice(0, leadLen) : ''

  if (/^\$\s+/.test(trimmed)) {
    const promptLen = trimmed.indexOf(' ') + 1
    return [
      ...(lead ? [{ kind: 'plain' as const, text: lead }] : []),
      { kind: 'prompt', text: trimmed.slice(0, promptLen) },
      { kind: 'command', text: trimmed.slice(promptLen) },
    ]
  }

  if (COMMAND_START_RE.test(trimmed)) {
    return [
      ...(lead ? [{ kind: 'plain' as const, text: lead }] : []),
      { kind: 'command', text: trimmed },
    ]
  }

  const tokens: TerminalToken[] = []
  let lastIndex = 0
  TERMINAL_HIGHLIGHT_RE.lastIndex = 0
  let match: RegExpExecArray | null
  while ((match = TERMINAL_HIGHLIGHT_RE.exec(line)) !== null) {
    if (match.index > lastIndex) {
      tokens.push({ kind: 'plain', text: line.slice(lastIndex, match.index) })
    }
    const piece = match[0]
    let kind: TerminalTokenKind = 'muted'
    const exitMatch = piece.match(/(-?\d+)\s*$/)
    if (/passed/i.test(piece) || /^PASS(?:ED)?$/i.test(piece)) {
      const count = Number.parseInt(piece, 10)
      kind = Number.isNaN(count) || count > 0 ? 'pass' : 'muted'
    } else if (/failed/i.test(piece) || /^FAIL(?:ED)?$/i.test(piece)) {
      const count = Number.parseInt(piece, 10)
      kind = Number.isNaN(count) || count > 0 ? 'fail' : 'muted'
    } else if (/ignored/i.test(piece)) {
      kind = 'muted'
    } else if (exitMatch?.[1] !== undefined) {
      const code = Number.parseInt(exitMatch[1], 10)
      kind = code === 0 ? 'pass' : 'fail'
    }
    tokens.push({ kind, text: piece })
    lastIndex = match.index + piece.length
  }
  if (lastIndex < line.length) {
    tokens.push({ kind: 'plain', text: line.slice(lastIndex) })
  }
  return tokens.length > 0 ? tokens : [{ kind: 'plain', text: line }]
}
