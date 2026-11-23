import { useEffect, useRef, useState } from 'react'
import { renderPreviewDiagram } from './PreviewDiagram.tsx'
import { FileTypeIcon, MarkdownText, writeClipboard } from '@deepseek-ai/dsh-client-ui-primitives'
import type { AgentEvent } from '../api.ts'
import { markdownLabels } from '../dsh/markdown-labels.ts'
import { t } from '../i18n.ts'
import { previewKind, previewModule, previewRevision, readPreviewText, studioUrl, type PreviewFile, type PreviewTheme } from '../filePreview.ts'
import { usePluginPages } from '../plugins/react.tsx'
import css from './FilePreviewPane.module.css'

const labels = markdownLabels(t)

function TextPreview({ file, revision, markdown }: { file: PreviewFile; revision: string; markdown: boolean }) {
  const [content, setContent] = useState<Awaited<ReturnType<typeof readPreviewText>>>()
  const [error, setError] = useState('')
  const line = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const controller = new AbortController()
    void readPreviewText(file.url, controller.signal).then(value => {
      if (!controller.signal.aborted) { setContent(value); setError('') }
    }, reason => {
      if (!controller.signal.aborted) setError(String(reason))
    })
    return () => controller.abort()
  }, [file.url, revision])
  useEffect(() => {
    line.current?.scrollIntoView({ block: 'center', inline: 'nearest' })
  }, [content, file.line])
  if (error) return <p className={css.notice} role="alert">无法读取文件 · {error}</p>
  if (!content) return <p className={css.notice} role="status">正在读取…</p>
  return <div className={css.scroller}>
    {content.truncated && <p className={css.notice}>仅显示前 512 KiB</p>}
    {markdown ? <div className={css.markdown}><MarkdownText text={content.text} labels={labels} renderDiagram={renderPreviewDiagram} /></div>
      : <div className={css.code} aria-label="只读代码">{content.text.split(/\r?\n/u).map((text, index) => {
        const selected = file.line === index + 1
        return <div key={index} ref={selected ? line : undefined} data-line={index + 1} data-highlighted={selected || undefined}>
          <span aria-hidden="true">{index + 1}</span><code>{text || ' '}</code>
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

export function FilePreviewPane({ files, active, hidden, theme, events, root, onSelect, onClose, onEdit }: {
  files: readonly PreviewFile[]; active: string | undefined; hidden: boolean; theme: PreviewTheme
  events: readonly AgentEvent[]; root: string
  onSelect: (url: string) => void; onClose: (url: string) => void; onEdit: (slot: string, file: PreviewFile) => void
}) {
  const { packages } = usePluginPages()
  const [refresh, setRefresh] = useState<Record<string, number>>({})
  const [copied, setCopied] = useState('')
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
      if (!selected && !(kind === 'plugin' && module)) return null
      return <section key={file.url} role="tabpanel" aria-label={file.name} className={css.viewer} hidden={!selected}>
        <div className={css.toolbar}>
          <span title={file.path}>{file.path}{file.line ? ':' + file.line : ''}</span>
          <div className={css.actions}>
            <button type="button" aria-label="刷新文件" title="刷新文件" onClick={() => setRefresh(state => ({ ...state, [file.url]: (state[file.url] ?? 0) + 1 }))}>↻</button>
            <button type="button" aria-label="复制路径" title="复制路径" onClick={() => void writeClipboard(file.path).then(ok => setCopied(ok ? file.url : ''))}>{copied === file.url ? '✓' : '⧉'}</button>
            <a download={file.name} href={file.url} aria-label="下载文件" title="下载文件">↓</a>
            {module && <button type="button" title="在办公创作台编辑" onClick={() => onEdit(module.slotId, file)}>编辑</button>}
          </div>
        </div>
        {kind === 'image' ? <ImagePreview key={revision} file={file} revision={revision} />
          : kind === 'markdown' || kind === 'code' ? <TextPreview file={file} revision={revision} markdown={kind === 'markdown'} />
          : kind === 'plugin' && module ? <OfficeFrame key={module.owner + ':' + module.page} file={file} owner={module.owner} page={module.page} theme={theme} revision={revision} />
          : <div className={css.notice}><FileTypeIcon path={file.name} size={28} /><p>{file.name}</p><p>{kind === 'plugin' ? '对应办公插件未启用' : '此文件类型不支持预览'}</p></div>}
      </section>
    })}
  </div>
}
