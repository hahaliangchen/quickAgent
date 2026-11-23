import { useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import type { PluginPageProps } from './protocol.ts'
import { createPluginHost } from './host.ts'
import { useAgentApi } from '../cordis/react.tsx'
import { studioUrl } from '../filePreview.ts'
import css from './html-page.module.css'

/** HTML is one implementation adapter for the common page contribution. */
export function HtmlPluginPage({ owner, workspaceRoot, pageId, pendingOpen, theme }: PluginPageProps) {
  const frame = useRef<HTMLIFrameElement>(null)
  const api = useAgentApi()
  const host = useMemo(() => createPluginHost(api, owner), [api, owner])
  const [loaded, setLoaded] = useState(false)
  const pageTitle = owner.pages.find(page => page.id === pageId)?.title ?? owner.title ?? pageId
  const initial = useRef(owner.remote ? undefined : pendingOpen)
  const sent = useRef(initial.current?.seq)
  const [src] = useState(() => studioUrl(owner.id, pageId, initial.current ? 'edit' : undefined,
    initial.current?.fileUrl, initial.current?.filename, owner.remote ? undefined : theme))
  useEffect(() => {
    if (!loaded || owner.remote || !theme) return
    frame.current?.contentWindow?.postMessage({ type: 'quickagent:theme', theme }, location.origin)
  }, [loaded, owner.remote, theme])
  useEffect(() => {
    if (!loaded || owner.remote || !pendingOpen || pendingOpen.seq === sent.current) return
    sent.current = pendingOpen.seq
    frame.current?.contentWindow?.postMessage({ type: 'quickagent:open-file', file: pendingOpen.fileUrl, name: pendingOpen.filename }, location.origin)
  }, [loaded, owner.remote, pendingOpen])
  useEffect(() => {
    const receive = (event: MessageEvent) => {
      if (event.source !== frame.current?.contentWindow || event.data?.type !== 'skill-package/request') return
      const { id, method, name, arguments: args } = event.data as Record<string, unknown>
      if (typeof id !== 'string' || id.length > 128) return
      const source = event.source as Window
      const reply = (result: unknown, error?: string) => {
        if (source === frame.current?.contentWindow) source.postMessage({ type: 'skill-package/response', id, result, error }, '*')
      }
      if (method === 'session' || method === 'service') {
        const operation = event.data.operation
        if (typeof operation !== 'string' || !args || typeof args !== 'object' || Array.isArray(args)) { reply(undefined, '请求格式错误。'); return }
        void host[method](operation, args as Record<string, unknown>).then(value => reply(value)).catch(reason => reply(undefined, String(reason)))
        return
      }
      if (method === 'workspace' && !owner.remote) { reply({ workspace_root: workspaceRoot }); return }
      if (method !== 'tool' || typeof name !== 'string' || !owner.tools.includes(name) || !args || typeof args !== 'object' || Array.isArray(args)) { reply(undefined, '该页面只能调用所属插件声明的工具。'); return }
      void host.callTool(name, owner.remote ? args as Record<string, unknown> : { workspace_root: workspaceRoot, ...args }).then(result => reply(result)).catch(reason => reply(undefined, String(reason)))
    }
    window.addEventListener('message', receive)
    return () => window.removeEventListener('message', receive)
  }, [owner, host, workspaceRoot])
  const sandbox = owner.remote
    ? 'allow-scripts'
    : 'allow-scripts allow-same-origin allow-downloads allow-modals allow-popups'
  return (
    <div className={css.host}>
      {!loaded && (
        <div role="status" aria-live="polite" className={css.loader}>
          <span aria-hidden="true" className={css.ring} />
          <span>正在加载 {pageTitle}…</span>
        </div>
      )}
      <iframe
        title={pageTitle}
        sandbox={sandbox}
        ref={frame}
        src={src}
        onLoad={() => setLoaded(true)}
        className={clsx(css.frame, loaded && css.frameReady)}
      />
    </div>
  )
}
