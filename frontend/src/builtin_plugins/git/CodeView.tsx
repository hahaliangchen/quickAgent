import { useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import { Icon } from '../../components/WorkbenchIcon.tsx'
import { grammarLoadCount, highlightLines, subscribeGrammarLoaded } from '../../dsh/ui-primitives/markdown/highlight.ts'
import css from './style.module.css'

export interface GitFileText { path: string; commit: string | null; bytes: number; too_large: boolean; binary?: boolean; content?: string | null }

/** Highlighting a huge file blocks the page; past this it renders plain. */
const HIGHLIGHT_LIMIT = 200 * 1024

/** A read-only file, GitHub style: breadcrumb, line numbers, highlighting. */
export function CodeView({ load, path, commit, changed, onShowDiff, onClose }: {
  load: (path: string, commit: string | null) => Promise<GitFileText>
  path: string; commit: string | null; changed: boolean
  onShowDiff: () => void; onClose: () => void
}) {
  const [file, setFile] = useState<GitFileText>()
  const [error, setError] = useState('')
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    let live = true
    setFile(undefined); setError('')
    load(path, commit).then(next => { if (live) setFile(next) }, reason => { if (live) setError(String(reason instanceof Error ? reason.message : reason)) })
    return () => { live = false }
  }, [path, commit])

  // A grammar loads lazily on first use; re-render once it is in.
  const grammars = useSyncExternalStore(subscribeGrammarLoaded, grammarLoadCount)
  const text = file?.content ?? ''
  const lines = useMemo(() => text.replace(/\n$/, '').split('\n'), [text])
  const spans = useMemo(() => {
    if (!text || text.length > HIGHLIGHT_LIMIT) return undefined
    const ext = path.slice(path.lastIndexOf('.') + 1)
    return highlightLines(text, ext)
  }, [text, path, grammars])

  const parts = path.split('/')
  return <section className={css.codeView} aria-label={`文件 ${path}`}>
    <header className={css.codeHead}>
      <nav className={css.crumbs}>
        {parts.map((part, i) => <span key={i} className={i === parts.length - 1 ? css.crumbLast : undefined}>{part}{i < parts.length - 1 && <span className={css.crumbSep}>›</span>}</span>)}
      </nav>
      <span className={css.readonlyBadge}>{commit ? `只读 · ${commit.slice(0, 7)}` : '只读'}</span>
      <span className={css.grow} />
      {file?.content != null && <span className={css.muted}>{lines.length} 行 · {formatBytes(file.bytes)}</span>}
      {changed && !commit && <button type="button" className={css.ghost} onClick={onShowDiff}>查看改动</button>}
      {file?.content != null && <button type="button" className={css.iconButton} title="复制全部内容"
        onClick={() => { void navigator.clipboard.writeText(text).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1200) }) }}>
        <Icon name={copied ? 'check' : 'copy'} size={15} /></button>}
      <button type="button" className={css.iconButton} title="关闭文件" onClick={onClose}><Icon name="close" size={15} /></button>
    </header>
    <div className={css.codeBody}>
      {error && <p role="alert" className={css.error}>{error}</p>}
      {!file && !error && <p className={css.empty}>正在读取…</p>}
      {file?.too_large && <p className={css.empty}>文件有 {formatBytes(file.bytes)}，超过 1 MB，不在这里显示。</p>}
      {file?.binary && <p className={css.empty}>二进制文件，无法以文本显示。</p>}
      {file?.content != null && <div className={css.codeLines}>
        {lines.map((line, i) => <div key={i} className={css.codeLine}>
          <span className={css.lineNo}>{i + 1}</span>
          <code>{spans?.[i] ? spans[i]!.map((span, j) => <span key={j} style={span.style}>{span.text}</span>) : line || ' '}</code>
        </div>)}
      </div>}
    </div>
  </section>
}

function formatBytes(bytes: number) {
  return bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)} KB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`
}
