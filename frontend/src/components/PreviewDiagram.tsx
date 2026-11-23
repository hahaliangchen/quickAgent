import { useEffect, useState, type Key } from 'react'

let diagramId = 0
const loadMermaid = () => import('mermaid').then(({ default: mermaid }) => {
  mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', suppressErrorRendering: true, maxTextSize: 16_384, maxEdges: 100 })
  return mermaid
})
let library: ReturnType<typeof loadMermaid> | undefined

function PreviewDiagram({ source }: { source: string }) {
  const [image, setImage] = useState<{ source: string; url: string }>()
  useEffect(() => {
    if (source.length > 16_384) return
    let cancelled = false
    let url: string | undefined
    void (library ??= loadMermaid()).then(async mermaid => {
      if (cancelled) return
      const { svg } = await mermaid.render('workspace-diagram-' + (++diagramId), source)
      if (cancelled) return
      // SVG displayed as an image cannot run scripts or supply clickable links.
      url = URL.createObjectURL(new Blob([svg], { type: 'image/svg+xml' }))
      setImage({ source, url })
    }).catch(() => { /* Keep invalid or unavailable diagrams readable as source. */ })
    return () => { cancelled = true; if (url) URL.revokeObjectURL(url) }
  }, [source])
  return image?.source === source
    ? <img src={image.url} alt="Mermaid 图表" style={{ maxWidth: '100%', background: '#fff', height: 'auto' }} />
    : <pre><code>{source}</code></pre>
}

export function renderPreviewDiagram(source: string, key: Key) {
  return <PreviewDiagram key={key} source={source} />
}
