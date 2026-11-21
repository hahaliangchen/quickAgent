export type VisualRecord = Record<string, unknown>
/** The full request remains available through explicit copy/export. */
export function foldImageData(value: unknown): unknown {
  if (typeof value === 'string' && value.startsWith('data:image/')) return `[图片数据已折叠，${value.length} 字符；实际发送体可复制核对]`
  if (Array.isArray(value)) return value.map(foldImageData)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, foldImageData(item)]))
  return value
}

/** Where a saved visual artifact's pixels are served from. */
export function visualArtifactUrl(taskId: string, artifactId: string): string {
  return `/agent/tasks/${encodeURIComponent(taskId)}/visual-artifacts/${encodeURIComponent(artifactId)}/image`
}

/** One picture a tool call produced, read back from its result payload. */
export interface VisualEvidence {
  /** Stable identity, so a call repeated on the same picture shows once. */
  readonly key: string
  readonly taskId: string
  readonly artifactId: string
  /** The tool whose call produced or consumed the picture. */
  readonly tool: string
  readonly width?: number | undefined
  readonly height?: number | undefined
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : undefined
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value : undefined
}

function size(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : undefined
}

/** The picture a record points at: an artifact id, or a saved artifact object. */
function artifactOf(value: unknown, taskId: string, tool: string): VisualEvidence | undefined {
  const direct = text(value)
  if (direct !== undefined) {
    return taskId === '' ? undefined : { key: `${taskId}/${direct}`, taskId, artifactId: direct, tool }
  }
  const record = asRecord(value)
  if (record === undefined) return undefined
  const id = text(record.artifact_id) ?? text(record.id)
  if (id === undefined) return undefined
  // Outside a conversation there is no route to the pixels, so show nothing
  // rather than a broken picture.
  const owner = text(record.task_id) ?? taskId
  if (owner === '') return undefined
  return {
    key: `${owner}/${id}`,
    taskId: owner,
    artifactId: id,
    tool,
    width: size(record.width) ?? size(record.width_px),
    height: size(record.height) ?? size(record.height_px),
  }
}

/**
 * Every picture a run of tool calls put in front of the user: a screenshot the
 * browser took, an image a model generated, or a picture the vision model was
 * asked about. Tool results carry these as a saved artifact or as artifact ids,
 * so this reads both shapes and drops repeats from calls on the same picture.
 *
 * @param results - the tool calls of one turn, in order.
 * @param taskId - the conversation serving the artifacts, for payloads naming none.
 * @returns the pictures in the order they first appeared.
 */
export function visualEvidenceOf(
  results: readonly { readonly name: string; readonly result: { readonly text: string } | undefined }[],
  taskId: string,
): readonly VisualEvidence[] {
  const found: VisualEvidence[] = []
  const seen = new Set<string>()
  for (const call of results) {
    if (call.result === undefined) continue
    let payload: Record<string, unknown> | undefined
    try {
      payload = asRecord(JSON.parse(call.result.text))
    } catch {
      payload = undefined // plain-text output names no picture
    }
    if (payload === undefined) continue
    const candidates = [
      artifactOf(payload.visual_artifact, taskId, call.name),
      artifactOf(payload.image_artifact_id, taskId, call.name),
      ...(Array.isArray(payload.artifact_ids) ? payload.artifact_ids.map(id => artifactOf(id, taskId, call.name)) : []),
    ]
    for (const candidate of candidates) {
      if (candidate === undefined || seen.has(candidate.key)) continue
      seen.add(candidate.key)
      found.push(candidate)
    }
  }
  return found
}
