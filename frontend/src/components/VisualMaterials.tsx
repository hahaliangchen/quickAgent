import type { VisualRecord } from '../visual.ts'

export function VisualArtifactPreview({ artifact }: { artifact: VisualRecord }) {
  const task = String(artifact.task_id ?? '')
  const id = String(artifact.artifact_id ?? '')
  if (!task || !id) return null
  const url = `/agent/tasks/${encodeURIComponent(task)}/visual-artifacts/${encodeURIComponent(id)}/image`
  return <figure style={{ margin: '8px 0' }}>
    <a href={url} target="_blank" rel="noreferrer"><img src={url} alt={`截图 ${id}`} loading="lazy" style={{ maxWidth: '100%', maxHeight: 240, objectFit: 'contain', borderRadius: 6 }} /></a>
    <figcaption style={{ fontSize: 12, overflowWrap: 'anywhere' }}>{String(artifact.url ?? '')}<br />
      {String(artifact.width)} × {String(artifact.height)} · {Number(artifact.byte_size ?? 0).toLocaleString()} B · {new Date(Number(artifact.captured_at ?? 0)).toLocaleString()} · 页面版本 {String(artifact.page_epoch ?? '?')}<br />
      {id} · SHA256 {String(artifact.content_hash ?? '')}
    </figcaption>
  </figure>
}
