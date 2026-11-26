// The AI live pane used to render the raw SSH byte stream: one prompt line, its
// output and a status line, all in the same font and the same colour, so a
// reader could not tell which line the AI issued and which lines were the reply.
// The backend already marks both boundaries — an "[AI · cwd] $ command" line
// before a command and an "[exit N · X ms]" line after it — so the stream can be
// split into the commands it holds and drawn as one card per command.
//
// Kept free of React so the split can be unit-tested without a browser.

/** How one command ended, read from the backend's outcome line. */
export interface CommandExit {
  /** The exit code, or null when the command ended without one. */
  code: number | null
  /** Wall-clock milliseconds the command took, when the line carries them. */
  elapsed: number | null
}

/** One command the AI issued, together with everything it printed. */
export interface CommandBlock {
  kind: 'command'
  /** Identity for React keys; increases with every block. */
  id: number
  /** Working directory the command ran in. */
  cwd: string
  /** The command as the backend echoed it; control characters arrive escaped. */
  command: string
  /** Raw output between the command line and the outcome line, ANSI included. */
  output: string
  /** Set once the outcome line arrived. */
  exit?: CommandExit
}

/** Stream text that belongs to no single command: banners and SFTP notices. */
export interface NoteBlock {
  kind: 'note'
  /** Identity for React keys. */
  id: number
  /** The notice text, ANSI included. */
  text: string
}

/** One entry of the transcript, in arrival order. */
export type TranscriptBlock = CommandBlock | NoteBlock

/** Everything the incremental parser carries between chunks. */
export interface TranscriptState {
  /** Finished blocks, oldest first. */
  blocks: TranscriptBlock[]
  /** Bytes held back because they may be the start of a marker. */
  buffer: string
  /** The command still collecting output, if any. */
  open: CommandBlock | null
  /** Next block id. */
  next: number
}

/** Blocks kept in memory; older commands scroll out of the pane anyway. */
const MAX_BLOCKS = 80
/** Per-block output characters kept, so one runaway command cannot freeze the pane. */
const MAX_OUTPUT_CHARS = 120_000
/** Per-block output lines kept beside {@link MAX_OUTPUT_CHARS}. */
const MAX_OUTPUT_LINES = 1_500

const COMMAND_MARK = '\u001b[36m[AI \u00b7 '
const SFTP_MARK = '\u001b[33m[AI SFTP]\u001b[0m '
const PASS_EXIT_MARK = '\u001b[32m[exit '
const FAIL_EXIT_MARK = '\u001b[31m[exit '
const PASS_NO_STATUS_MARK = '\u001b[32m[no exit status'
const FAIL_NO_STATUS_MARK = '\u001b[31m[no exit status'
const MARKS = [
  COMMAND_MARK,
  SFTP_MARK,
  PASS_EXIT_MARK,
  FAIL_EXIT_MARK,
  PASS_NO_STATUS_MARK,
  FAIL_NO_STATUS_MARK,
]
/** Longest marker prefix, the most that can be held back across a chunk boundary. */
const MARK_LIMIT = Math.max(...MARKS.map(mark => mark.length))
/** Maximum bytes an outcome marker line can legitimately span before its newline. */
const MAX_OUTCOME_LINE_CHARS = 128

const COMMAND_LINE = /^\u001b\[36m\[AI \u00b7 ([^\r\n]*?)\]\u001b\[0m \$ ([^\r\n]*?)\r?\n/
const SFTP_LINE = /^\u001b\[33m\[AI SFTP\]\u001b\[0m ([^\r\n]*?)\r?\n/
const OUTCOME_LINE = /^\u001b\[(?:32|31)m\[((?:exit \d+|no exit status)(?: \u00b7 \d+ ms)?)\]\u001b\[0m\r?\n/

/**
 * A transcript with nothing in it yet.
 * @returns fresh parse state.
 */
export function emptyTranscript(): TranscriptState {
  return { blocks: [], buffer: '', open: null, next: 1 }
}

/**
 * Read an outcome line's body ("exit 0 · 758 ms", "no exit status · 12 ms").
 * @param text - the text between the outcome line's brackets.
 * @returns the exit code and elapsed time, either possibly unknown.
 */
export function readExit(text: string): CommandExit {
  const code = /exit (\d+)/.exec(text)
  const elapsed = /(\d+) ms/.exec(text)
  return {
    code: code?.[1] === undefined ? null : Number(code[1]),
    elapsed: elapsed?.[1] === undefined ? null : Number(elapsed[1]),
  }
}

/**
 * Show a command the way a shell would run it: the backend escapes control
 * characters so a multi-line command cannot move the cursor, which leaves the
 * breaks visible as literal "r n" text.
 * @param command - the echoed command text.
 * @returns one display line per real line of the command.
 */
export function commandLines(command: string): string[] {
  const expanded = command.replace(/\\r\\n/g, '\n').replace(/\\n/g, '\n').replace(/\\r/g, '\n').replace(/\\t/g, '  ')
  const body = expanded.endsWith('\n') ? expanded.slice(0, -1) : expanded
  return body === '' ? [''] : body.split('\n')
}

/**
 * Keep a block's output bounded: whole-line trimming from the front, so the tail
 * a reader is watching stays put.
 * @param text - accumulated output.
 * @returns the output, trimmed only when it exceeds the caps.
 */
function bounded(text: string): string {
  let out = text
  if (out.length > MAX_OUTPUT_CHARS) out = out.slice(out.length - MAX_OUTPUT_CHARS)
  const lines = out.split('\n')
  if (lines.length > MAX_OUTPUT_LINES) out = lines.slice(lines.length - MAX_OUTPUT_LINES).join('\n')
  return out
}

/**
 * Length of a trailing run that could still become a marker once more bytes
 * arrive, so it is held back instead of being shown as output.
 * @param text - the unparsed buffer.
 * @returns how many trailing characters to keep for the next chunk.
 */
function partialMark(text: string): number {
  const longest = Math.min(text.length, MARK_LIMIT)
  for (let length = longest; length > 0; length -= 1) {
    const tail = text.slice(text.length - length)
    if (MARKS.some(mark => mark.length > length && mark.startsWith(tail))) return length
  }
  return 0
}

/**
 * Append stream text to whatever is currently collecting it.
 * @param state - parse state to mutate.
 * @param text - the text to append; ignored when empty.
 */
function append(state: TranscriptState, text: string): void {
  if (text === '') return
  if (state.open !== null) { state.open.output = bounded(state.open.output + text); return }
  // The blank line the backend writes before each marker is framing, not
  // output: without this, every command would be preceded by an empty notice.
  if (text.trim() === '') return
  const last = state.blocks[state.blocks.length - 1]
  if (last !== undefined && last.kind === 'note') { last.text = bounded(last.text + text); return }
  push(state, { kind: 'note', id: state.next, text: bounded(text) })
}

/**
 * Drop the blank line the backend writes after a command's output, so a block
 * holds exactly what the command printed.
 * @param state - parse state whose open command is ending.
 */
function settle(state: TranscriptState): void {
  if (state.open !== null) state.open.output = state.open.output.replace(/\r?\n$/, '')
}

/**
 * Add a block, dropping the oldest once the cap is reached.
 * @param state - parse state to mutate.
 * @param block - the block to add; its id comes from the caller.
 */
function push(state: TranscriptState, block: TranscriptBlock): void {
  state.next += 1
  state.blocks.push(block)
  if (state.blocks.length > MAX_BLOCKS) state.blocks.splice(0, state.blocks.length - MAX_BLOCKS)
}

/**
 * Feed one chunk of the live stream, finishing every block it completes.
 * A marker split across two chunks is held back until its last byte arrives, so
 * a wedged packet cannot turn a prompt line into stray output.
 * @param state - parse state to mutate.
 * @param text - decoded stream text, as many lines as the chunk carried.
 */
export function feedTranscript(state: TranscriptState, text: string): void {
  state.buffer += text
  for (;;) {
    const buffer = state.buffer
    let at = -1
    for (const mark of MARKS) {
      const found = buffer.indexOf(mark)
      if (found >= 0 && (at < 0 || found < at)) at = found
    }
    if (at < 0) {
      const hold = partialMark(buffer)
      append(state, hold === 0 ? buffer : buffer.slice(0, buffer.length - hold))
      state.buffer = hold === 0 ? '' : buffer.slice(buffer.length - hold)
      return
    }
    if (at > 0) {
      append(state, buffer.slice(0, at))
      state.buffer = buffer.slice(at)
      continue
    }
    const command = COMMAND_LINE.exec(buffer)
    if (command !== null) {
      settle(state)
      const block: CommandBlock = {
        kind: 'command',
        id: state.next,
        cwd: command[1] ?? '',
        command: command[2] ?? '',
        output: '',
      }
      push(state, block)
      state.open = block
      state.buffer = buffer.slice(command[0].length)
      continue
    }
    const sftp = SFTP_LINE.exec(buffer)
    if (sftp !== null) {
      settle(state)
      state.open = null
      push(state, { kind: 'note', id: state.next, text: sftp[1] ?? '' })
      state.buffer = buffer.slice(sftp[0].length)
      continue
    }
    const outcome = OUTCOME_LINE.exec(buffer)
    if (outcome !== null) {
      settle(state)
      if (state.open !== null) state.open.exit = readExit(outcome[1] ?? '')
      state.open = null
      state.buffer = buffer.slice(outcome[0].length)
      continue
    }
    // If the line already ended (or an outcome candidate grew past any valid
    // outcome line) without matching a control line, this prefix came from
    // ordinary command output. Advance past the leading ESC so parsing never
    // stalls on colored bracketed logs.
    const isOutcomePrefix = !buffer.startsWith(COMMAND_MARK) && !buffer.startsWith(SFTP_MARK)
    if (buffer.includes('\n') || (isOutcomePrefix && buffer.length > MAX_OUTCOME_LINE_CHARS)) {
      append(state, buffer.slice(0, 1))
      state.buffer = buffer.slice(1)
      continue
    }
    // A marker arrived but its line is not complete yet: wait for the rest.
    return
  }
}

/**
 * Count the commands a transcript holds.
 * @param blocks - the transcript.
 * @returns how many commands, and how many of those failed.
 */
export function countCommands(blocks: readonly TranscriptBlock[]): { total: number; failed: number } {
  let total = 0
  let failed = 0
  for (const block of blocks) {
    if (block.kind !== 'command') continue
    total += 1
    const code = block.exit?.code
    if (code !== undefined && code !== null && code !== 0) failed += 1
  }
  return { total, failed }
}
