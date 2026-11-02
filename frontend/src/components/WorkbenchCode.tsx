import { useMemo, useState } from 'react'
import { diffLines } from 'diff'
import { languageForPath, useCodeHighlighter } from '../dsh/ui-primitives/code-highlighting.ts'
import styles from './SkillWorkbench.module.css'

const lines = (text: string): string[] => {
  const all = text.replace(/\r\n/g, '\n').split('\n')
  return all.length > 1 && all.at(-1) === '' ? all.slice(0, -1) : all
}

/** Source with line numbers and highlighting; long lines scroll inside the block. */
export function CodeView({ source, path }: { source: string; path: string }) {
  const highlight = useCodeHighlighter(languageForPath(path) ?? 'javascript')
  const rows = lines(source)
  const tokens = highlight(rows.join('\n'))
  return <div className={styles.codeScroll}>
    <pre className={styles.codeLines}>{rows.map((line, index) => <div className={styles.codeLine} key={index}>
      <span className={styles.lineNumber}>{index + 1}</span>
      <span className={styles.lineText}>{tokens?.[index] ? tokens[index].map((token, i) => <span key={i} style={token.style}>{token.text}</span>) : line || ' '}</span>
    </div>)}</pre>
  </div>
}

type Row = { kind: 'same' | 'add' | 'del'; old?: number; now?: number; text: string }

function diffRows(before: string, after: string): Row[] {
  const rows: Row[] = []
  let old = 1
  let now = 1
  for (const part of diffLines(before.replace(/\r\n/g, '\n'), after.replace(/\r\n/g, '\n'))) {
    for (const text of lines(part.value)) {
      if (part.added) rows.push({ kind: 'add', now: now++, text })
      else if (part.removed) rows.push({ kind: 'del', old: old++, text })
      else rows.push({ kind: 'same', old: old++, now: now++, text })
    }
  }
  return rows
}

export function diffStats(before: string | null, after: string | null): { added: number; removed: number } {
  const rows = diffRows(before ?? '', after ?? '')
  return { added: rows.filter(row => row.kind === 'add').length, removed: rows.filter(row => row.kind === 'del').length }
}

/** Unchanged stretches longer than this fold to their first and last lines. */
const CONTEXT = 3

/** The snapshot against the draft on disk, both sides numbered; long unchanged stretches fold. */
export function DiffView({ before, after }: { before: string | null; after: string | null }) {
  const rows = useMemo(() => diffRows(before ?? '', after ?? ''), [before, after])
  const [open, setOpen] = useState<ReadonlySet<number>>(new Set())
  if (!rows.some(row => row.kind !== 'same')) return <div className={styles.diffEmpty}>草稿和原始流程一致，还没有改动。</div>
  const shown: (Row | { kind: 'fold'; at: number; hidden: number })[] = []
  for (let index = 0; index < rows.length;) {
    let end = index
    while (end < rows.length && rows[end]!.kind === 'same') end++
    const run = end - index
    if (run > 2 * CONTEXT + 2 && !open.has(index)) {
      const head = index === 0 ? 0 : CONTEXT
      const tail = end === rows.length ? 0 : CONTEXT
      shown.push(...rows.slice(index, index + head), { kind: 'fold', at: index, hidden: run - head - tail }, ...rows.slice(end - tail, end))
      index = end
    } else if (run > 0) {
      shown.push(...rows.slice(index, end))
      index = end
    } else {
      shown.push(rows[index]!)
      index++
    }
  }
  return <div className={styles.codeScroll}>
    <pre className={styles.codeLines}>{shown.map((row, index) => row.kind === 'fold'
      ? <button key={`fold-${row.at}`} type="button" className={styles.diffFold} onClick={() => setOpen(current => new Set([...current, row.at]))}>展开 {row.hidden} 行未改动的代码</button>
      : <div key={index} className={`${styles.codeLine} ${row.kind === 'add' ? styles.diffAdd : row.kind === 'del' ? styles.diffDel : ''}`}>
        <span className={styles.lineNumber}>{row.old ?? ''}</span>
        <span className={styles.lineNumber}>{row.now ?? ''}</span>
        <span className={styles.diffMark}>{row.kind === 'add' ? '+' : row.kind === 'del' ? '−' : ' '}</span>
        <span className={styles.lineText}>{row.text || ' '}</span>
      </div>)}</pre>
  </div>
}
