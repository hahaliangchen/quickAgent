import { useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import { renderPreviewDiagram } from './PreviewDiagram.tsx'
import { FileTypeIcon, MarkdownText, writeClipboard } from '@deepseek-ai/dsh-client-ui-primitives'
import type { AgentEvent, SkillPackage } from '../api.ts'
import { useAgentApi } from '../cordis/react.tsx'
import { markdownLabels } from '../dsh/markdown-labels.ts'
import { languageForPath, useCodeHighlighter, type HighlightSpan } from '../dsh/ui-primitives/code-highlighting.ts'
import { t } from '../i18n.ts'
import { previewKind, previewModule, previewRevision, readPreviewText, studioUrl, type PreviewFile, type PreviewTheme } from '../filePreview.ts'
import { buildInlineCodeDiff, findSessionFileChange, type InlineDiffResult } from '../filePreviewDiff.ts'
import { usePluginPages } from '../plugins/react.tsx'
import css from './FilePreviewPane.module.css'

const labels = markdownLabels(t)
const HIGHLIGHT_LIMIT = 200 * 1024

interface DiffBaseline {
  readonly status: 'idle' | 'loading' | 'ready' | 'none'
  readonly oldText: string | null
  readonly source?: 'session' | 'git' | undefined
}

interface TextPreviewMeta {
  readonly diffStatus: DiffBaseline['status']
  readonly hasChanges: boolean
  readonly added: number
  readonly deleted: number
  readonly hunkCount: number
  readonly source?: 'session' | 'git' | undefined
}

function renderCodeSpans(spans: readonly HighlightSpan[] | undefined, fallback: string) {
  if (!spans || spans.length === 0) return fallback || ' '
  return spans.map((span, idx) => <span key={idx} style={span.style}>{span.text}</span>)
}

function TextPreview({
  taskId,
  file,
  revision,
  markdown,
  events,
  root,
  packages,
  diffEnabled,
  hunkCursor,
  onMeta,
}: {
  taskId?: string | undefined
  file: PreviewFile
  revision: string
  markdown: boolean
  events: readonly AgentEvent[]
  root: string
  packages: readonly SkillPackage[]
  diffEnabled: boolean
  hunkCursor: { index: number; seq: number }
  onMeta: (meta: TextPreviewMeta) => void
}) {
  const api = useAgentApi()
  const [content, setContent] = useState<Awaited<ReturnType<typeof readPreviewText>>>()
  const [error, setError] = useState('')
  const [baseline, setBaseline] = useState<DiffBaseline>({ status: 'idle', oldText: null })
  const highlightedLineRef = useRef<HTMLDivElement | null>(null)
  const hunkRefs = useRef<Map<number, HTMLDivElement>>(new Map())
  const didAutoScrollHunkRef = useRef('')

  const sessionChange = useMemo(
    () => (file.attachment ? undefined : findSessionFileChange(events, file.path, root)),
    [file.attachment, events, file.path, root],
  )
  const hasGitTool = useMemo(
    () => packages.some(pkg => pkg.id === 'git' && pkg.enabled && pkg.tools.includes('read_git')),
    [packages],
  )

  useEffect(() => {
    const controller = new AbortController()
    void readPreviewText(file.url, controller.signal).then(value => {
      if (!controller.signal.aborted) { setContent(value); setError('') }
    }, reason => {
      if (!controller.signal.aborted) setError(String(reason))
    })
    return () => controller.abort()
  }, [file.url, revision])

  // Fetch diff baseline: prefer session workspace/changes snapshot, then fall back to Git HEAD.
  useEffect(() => {
    if (file.attachment) {
      setBaseline({ status: 'none', oldText: null })
      return
    }
    let cancelled = false
    setBaseline({ status: 'loading', oldText: null })

    const loadBaseline = async () => {
      if (sessionChange && taskId && typeof api.changeDiff === 'function') {
        try {
          const diff = await api.changeDiff(taskId, sessionChange.earliestTurn, 0, sessionChange.entryPath)
          if (cancelled) return
          if (!diff.binary && !diff.unavailable) {
            setBaseline({ status: 'ready', oldText: diff.oldText, source: 'session' })
            return
          }
        } catch {
          // Fall through to Git HEAD fallback if snapshot lookup fails.
        }
      }

      if (hasGitTool && root && typeof api.skillTool === 'function') {
        try {
          const gitFile = await api.skillTool<{ content?: string | null; binary?: boolean; too_large?: boolean }>(
            'git',
            'read_git',
            { workspace_root: root, view: 'file', path: file.path, commit: 'HEAD' },
          )
          if (cancelled) return
          if (!gitFile.binary && !gitFile.too_large && typeof gitFile.content === 'string') {
            setBaseline({ status: 'ready', oldText: gitFile.content, source: 'git' })
            return
          }
        } catch {
          // Not a git repo or file untracked at HEAD.
        }
      }

      if (!cancelled) setBaseline({ status: 'none', oldText: null })
    }

    void loadBaseline()
    return () => { cancelled = true }
  }, [api, file.attachment, file.path, hasGitTool, revision, root, sessionChange, taskId])

  const effectiveText = content?.text ?? (error && baseline.status === 'ready' && sessionChange?.removed ? '' : undefined)
  const diffResult = useMemo<InlineDiffResult | undefined>(() => {
    if (effectiveText === undefined || baseline.status !== 'ready') return undefined
    return buildInlineCodeDiff(baseline.oldText, effectiveText)
  }, [baseline.oldText, baseline.status, effectiveText])

  const showingDiff = diffEnabled && Boolean(diffResult?.hasChanges)

  useEffect(() => {
    onMeta({
      diffStatus: baseline.status,
      hasChanges: Boolean(diffResult?.hasChanges),
      added: diffResult?.added ?? 0,
      deleted: diffResult?.deleted ?? 0,
      hunkCount: diffResult?.hunkIndices.length ?? 0,
      source: baseline.source,
    })
  }, [baseline.source, baseline.status, diffResult, onMeta])

  const highlight = useCodeHighlighter(languageForPath(file.name))
  const newHighlighted = useMemo(() => {
    if (!effectiveText || effectiveText.length > HIGHLIGHT_LIMIT) return undefined
    return highlight(effectiveText.replace(/\r\n/gu, '\n').replace(/\r/gu, '\n'))
  }, [effectiveText, highlight])

  const oldHighlighted = useMemo(() => {
    if (!showingDiff || !baseline.oldText || baseline.oldText.length > HIGHLIGHT_LIMIT) return undefined
    return highlight(baseline.oldText.replace(/\r\n/gu, '\n').replace(/\r/gu, '\n'))
  }, [baseline.oldText, highlight, showingDiff])

  // Scroll to requested `:line` in both normal code and inline diff views.
  useEffect(() => {
    if (file.line !== undefined) {
      highlightedLineRef.current?.scrollIntoView({ block: 'center', inline: 'nearest' })
      return
    }
    if (showingDiff && diffResult && diffResult.hunkIndices.length > 0) {
      const scrollKey = `${file.url}:${revision}:diff`
      if (didAutoScrollHunkRef.current !== scrollKey) {
        didAutoScrollHunkRef.current = scrollKey
        const firstIdx = diffResult.hunkIndices[0]
        if (firstIdx !== undefined) {
          hunkRefs.current.get(firstIdx)?.scrollIntoView({ block: 'center', inline: 'nearest' })
        }
      }
    }
  }, [content, diffResult, file.line, file.url, revision, showingDiff])

  // Explicit hunk jump via toolbar ↑ / ↓ buttons.
  useEffect(() => {
    if (hunkCursor.seq === 0 || !showingDiff || !diffResult || diffResult.hunkIndices.length === 0) return
    const targetRow = diffResult.hunkIndices[hunkCursor.index % diffResult.hunkIndices.length]
    if (targetRow !== undefined) {
      hunkRefs.current.get(targetRow)?.scrollIntoView({ block: 'center', inline: 'nearest' })
    }
  }, [diffResult, hunkCursor, showingDiff])

  if (error && effectiveText === undefined) return <p className={css.notice} role="alert">无法读取文件 · {error}</p>
  if (effectiveText === undefined) return <p className={css.notice} role="status">正在读取…</p>

  if (showingDiff && diffResult) {
    hunkRefs.current.clear()
    const hunkSet = new Set(diffResult.hunkIndices)
    return <div className={css.scroller}>
      {content?.truncated && <p className={css.notice}>仅显示前 512 KiB</p>}
      <div className={css.diffCode} aria-label="代码变更对比" data-diff-view="true">
        {diffResult.rows.map((row, index) => {
          const selected = file.line !== undefined && row.newLine === file.line
          const isHunkStart = hunkSet.has(index)
          const spans = row.kind === 'del'
            ? (row.oldLine !== null ? oldHighlighted?.[row.oldLine - 1] : undefined)
            : (row.newLine !== null ? newHighlighted?.[row.newLine - 1] : undefined)
          return (
            <div
              key={index}
              ref={el => {
                if (selected) highlightedLineRef.current = el
                if (isHunkStart) {
                  if (el) hunkRefs.current.set(index, el)
                  else hunkRefs.current.delete(index)
                }
              }}
              className={clsx(
                css.diffRow,
                row.kind === 'add' && css.diffAdd,
                row.kind === 'del' && css.diffDel,
              )}
              data-line={row.newLine ?? undefined}
              data-old-line={row.oldLine ?? undefined}
              data-diff-kind={row.kind}
              data-highlighted={selected || undefined}
            >
              <span className={css.diffGutter} aria-hidden="true">
                <span className={css.diffLineNo}>{row.oldLine ?? ''}</span>
                <span className={css.diffLineNo}>{row.newLine ?? ''}</span>
                <span className={css.diffSign}>{row.kind === 'add' ? '+' : row.kind === 'del' ? '−' : ' '}</span>
              </span>
              <code>{renderCodeSpans(spans, row.text)}</code>
            </div>
          )
        })}
      </div>
    </div>
  }

  return <div className={css.scroller}>
    {content?.truncated && <p className={css.notice}>仅显示前 512 KiB</p>}
    {markdown ? <div className={css.markdown}><MarkdownText text={effectiveText} labels={labels} renderDiagram={renderPreviewDiagram} /></div>
      : <div className={css.code} aria-label="只读代码">{effectiveText.split(/\r?\n/u).map((text, index) => {
        const selected = file.line === index + 1
        return <div key={index} ref={selected ? highlightedLineRef : undefined} data-line={index + 1} data-highlighted={selected || undefined}>
          <span className={css.lineNo} aria-hidden="true">{index + 1}</span>
          <code>{renderCodeSpans(newHighlighted?.[index], text)}</code>
        </div>
      })}</div>}
  </div>
}

function OfficeFrame({ file, owner, page, theme, revision }: {
  file: PreviewFile; owner: string; page: string; theme: PreviewTheme; revision: string
}) {
  const frame = useRef<HTMLIFrameElement>(null)
  const [src] = useState(() => studioUrl(owner, page, 'preview', file.url, file.name, theme))
  const loadedRevision = useRef(revision)
  const [ready, setReady] = useState(false)
  const [error, setError] = useState('')
  const [unsupported, setUnsupported] = useState(false)
  useEffect(() => {
    if (ready) return
    const timer = window.setTimeout(() => setUnsupported(true), 15_000)
    return () => window.clearTimeout(timer)
  }, [ready])
  useEffect(() => {
    const receive = (event: MessageEvent) => {
      if (event.origin !== location.origin || event.source !== frame.current?.contentWindow) return
      if (event.data?.type === 'quickagent:preview-ready') { setReady(true); setError('') }
      if (event.data?.type === 'quickagent:preview-error' && typeof event.data.message === 'string') { setReady(true); setError(event.data.message) }
    }
    window.addEventListener('message', receive)
    return () => window.removeEventListener('message', receive)
  }, [])
  useEffect(() => {
    if (ready) frame.current?.contentWindow?.postMessage({ type: 'quickagent:theme', theme }, location.origin)
  }, [ready, theme])
  useEffect(() => {
    if (!ready || revision === loadedRevision.current) return
    loadedRevision.current = revision
    frame.current?.contentWindow?.postMessage({ type: 'quickagent:reload-file', file: file.url, name: file.name }, location.origin)
  }, [ready, revision, file.url, file.name])
  return <>
    {error ? <p className={css.notice} role="alert">{error}</p> : !ready && <p className={css.notice} role="status">{unsupported ? '此版本的办公插件未响应只读预览' : '正在加载文档预览…'}</p>}
    <iframe ref={frame} className={css.frame} title={file.name} src={src} sandbox="allow-scripts allow-same-origin"
      aria-hidden={!ready || !!error} tabIndex={!ready || error ? -1 : undefined} style={!ready || error ? { position: 'absolute', width: 1, height: 1, visibility: 'hidden' } : undefined} />
  </>
}

function ImagePreview({ file, revision }: { file: PreviewFile; revision: string }) {
  const [zoom, setZoom] = useState(100)
  const [failed, setFailed] = useState(false)
  const src = file.url + (file.url.includes('?') ? '&' : '?') + 'v=' + revision
  return <div className={css.scroller}>
    <div className={css.zoom} aria-label="图片缩放">
      <button type="button" aria-label="缩小" disabled={zoom <= 25} onClick={() => setZoom(z => z - 25)}>−</button>
      <button type="button" aria-label="重置缩放" onClick={() => setZoom(100)}>{zoom}%</button>
      <button type="button" aria-label="放大" disabled={zoom >= 400} onClick={() => setZoom(z => z + 25)}>+</button>
    </div>
    {failed ? <p className={css.notice} role="alert">无法读取图片</p>
      : <div className={css.image}><img src={src} alt={file.name} style={{ width: zoom + '%' }} onError={() => setFailed(true)} /></div>}
  </div>
}

export function FilePreviewPane({ taskId, files, active, hidden, theme, events, root, onSelect, onClose, onEdit }: {
  taskId?: string | undefined
  files: readonly PreviewFile[]; active: string | undefined; hidden: boolean; theme: PreviewTheme
  events: readonly AgentEvent[]; root: string
  onSelect: (url: string) => void; onClose: (url: string) => void; onEdit: (slot: string, file: PreviewFile) => void
}) {
  const { packages } = usePluginPages()
  const [refresh, setRefresh] = useState<Record<string, number>>({})
  const [copied, setCopied] = useState('')
  // Diff mode is enabled by default for code/text previews; toggling off shows clean normal code.
  const [diffEnabled, setDiffEnabled] = useState(true)
  const [metaByUrl, setMetaByUrl] = useState<Record<string, TextPreviewMeta>>({})
  const [hunkCursorByUrl, setHunkCursorByUrl] = useState<Record<string, { index: number; seq: number }>>({})

  const jumpHunk = (url: string, hunkCount: number, delta: number) => {
    if (hunkCount <= 0) return
    setHunkCursorByUrl(state => {
      const current = state[url]
      const nextIndex = current
        ? (current.index + delta + hunkCount) % hunkCount
        : (delta < 0 ? hunkCount - 1 : 0)
      return { ...state, [url]: { index: nextIndex, seq: (current?.seq ?? 0) + 1 } }
    })
  }

  return <div className={css.root} hidden={hidden}>
    {!files.length && <p className={css.notice}>没有打开的文件</p>}
    {files.length > 0 && <div className={css.tabs} role="tablist" aria-label="打开的文件">
      {files.map(file => <div key={file.url} className={css.tab} data-active={file.url === active || undefined}>
        <button type="button" role="tab" aria-selected={file.url === active} title={file.path} onClick={() => onSelect(file.url)}>
          <FileTypeIcon path={file.name} size={15} /><span>{file.name}</span>
        </button>
        <button type="button" title="关闭标签" aria-label={'关闭 ' + file.name} onClick={() => onClose(file.url)}>×</button>
      </div>)}
    </div>}
    {files.map(file => {
      const selected = file.url === active
      const kind = previewKind(file.name)
      const module = previewModule(file.name, packages)
      const revision = previewRevision(file, events, root) + '-' + (refresh[file.url] ?? 0)
      const isTextLike = kind === 'markdown' || kind === 'code'
      const meta = metaByUrl[file.url]
      const hunkCursor = hunkCursorByUrl[file.url] ?? { index: 0, seq: 0 }
      if (!selected && !(kind === 'plugin' && module)) return null
      return <section key={file.url} role="tabpanel" aria-label={file.name} className={css.viewer} hidden={!selected}>
        <div className={css.toolbar}>
          <span title={file.path}>{file.path}{file.line ? ':' + file.line : ''}</span>
          <div className={css.actions}>
            {isTextLike && !file.attachment && (
              <div className={css.diffControls}>
                {meta?.hasChanges && (
                  <span className={css.diffStats} title={meta.source === 'git' ? '对比 Git HEAD 工作区改动' : '对比本会话改动'}>
                    <span className={css.statAdd}>+{meta.added}</span>
                    <span className={css.statDel}>-{meta.deleted}</span>
                  </span>
                )}
                {diffEnabled && meta && meta.hunkCount > 1 && (
                  <span className={css.hunkNav}>
                    <button type="button" aria-label="上一处变更" title="上一处变更" onClick={() => jumpHunk(file.url, meta.hunkCount, -1)}>↑</button>
                    <button type="button" aria-label="下一处变更" title="下一处变更" onClick={() => jumpHunk(file.url, meta.hunkCount, 1)}>↓</button>
                  </span>
                )}
                <button
                  type="button"
                  role="switch"
                  aria-checked={diffEnabled}
                  className={clsx(css.diffSwitch, diffEnabled && css.diffSwitchOn)}
                  title={diffEnabled
                    ? (meta?.hasChanges ? '当前显示 Diff，点击切换为正常代码' : '已开启 Diff（当前文件暂无变更，显示正常代码），点击关闭')
                    : '当前显示正常代码，点击开启 Diff 对比'}
                  onClick={() => setDiffEnabled(v => !v)}
                >
                  <span className={css.switchTrack} aria-hidden="true"><span className={css.switchThumb} /></span>
                  <span>Diff</span>
                  {diffEnabled && meta && meta.diffStatus !== 'loading' && !meta.hasChanges && (
                    <span className={css.diffNoneTag}>无变更</span>
                  )}
                </button>
              </div>
            )}
            <button type="button" aria-label="刷新文件" title="刷新文件" onClick={() => setRefresh(state => ({ ...state, [file.url]: (state[file.url] ?? 0) + 1 }))}>↻</button>
            <button type="button" aria-label="复制路径" title="复制路径" onClick={() => void writeClipboard(file.path).then(ok => setCopied(ok ? file.url : ''))}>{copied === file.url ? '✓' : '⧉'}</button>
            <a download={file.name} href={file.url} aria-label="下载文件" title="下载文件">↓</a>
            {module && <button type="button" title="在办公创作台编辑" onClick={() => onEdit(module.slotId, file)}>编辑</button>}
          </div>
        </div>
        {kind === 'image' ? <ImagePreview key={revision} file={file} revision={revision} />
          : isTextLike ? (
            <TextPreview
              taskId={taskId}
              file={file}
              revision={revision}
              markdown={kind === 'markdown'}
              events={events}
              root={root}
              packages={packages}
              diffEnabled={diffEnabled}
              hunkCursor={hunkCursor}
              onMeta={next => setMetaByUrl(prev => {
                const cur = prev[file.url]
                if (cur
                  && cur.diffStatus === next.diffStatus
                  && cur.hasChanges === next.hasChanges
                  && cur.added === next.added
                  && cur.deleted === next.deleted
                  && cur.hunkCount === next.hunkCount
                  && cur.source === next.source) return prev
                return { ...prev, [file.url]: next }
              })}
            />
          )
          : kind === 'plugin' && module ? <OfficeFrame key={module.owner + ':' + module.page} file={file} owner={module.owner} page={module.page} theme={theme} revision={revision} />
          : <div className={css.notice}><FileTypeIcon path={file.name} size={28} /><p>{file.name}</p><p>{kind === 'plugin' ? '对应办公插件未启用' : '此文件类型不支持预览'}</p></div>}
      </section>
    })}
  </div>
}
