const MIME_EXT: Record<string, string> = {
  'image/png': 'png',
  'image/jpeg': 'jpg',
  'image/webp': 'webp',
  'image/gif': 'gif',
}

function normalizeFile(file: File, index: number): File {
  if (file.name && file.name.trim() !== '') return file
  const ext = MIME_EXT[file.type] ?? 'png'
  return new File([file], `pasted-image-${Date.now()}-${index}.${ext}`, {
    type: file.type || 'image/png',
    lastModified: Date.now(),
  })
}

/**
 * Extract files from both `clipboardData.files` and `clipboardData.items`.
 * WebKitGTK and several screenshot tools expose bitmap images only via `items`.
 */
export function extractClipboardFiles(data: DataTransfer | null | undefined): File[] {
  if (!data) return []
  const collected: File[] = []
  const seen = new Set<string>()
  const push = (raw: File | null | undefined) => {
    if (!raw) return
    const key = `${raw.name}:${raw.size}:${raw.type}`
    if (seen.has(key)) return
    seen.add(key)
    collected.push(normalizeFile(raw, collected.length + 1))
  }
  if (data.files) {
    for (const file of Array.from(data.files)) push(file)
  }
  if (data.items) {
    for (const item of Array.from(data.items)) {
      if (item.kind === 'file') push(item.getAsFile())
    }
  }
  return collected
}

/**
 * Fallback for Linux WebKitGTK on Wayland, where pasting a bitmap-only
 * clipboard into a `<textarea>` yields empty `clipboardData` or suppresses the
 * `paste` event altogether.
 */
export async function readFallbackClipboardImages(
  fetchImpl: typeof fetch = fetch,
): Promise<File[]> {
  try {
    if (typeof navigator !== 'undefined' && navigator.clipboard && typeof navigator.clipboard.read === 'function') {
      const items = await navigator.clipboard.read()
      const files: File[] = []
      for (const item of items) {
        const mime = item.types.find(type => type.startsWith('image/'))
        if (!mime) continue
        const blob = await item.getType(mime)
        if (blob.size > 0) {
          const ext = MIME_EXT[mime] ?? 'png'
          files.push(new File([blob], `clipboard-${Date.now()}-${files.length + 1}.${ext}`, { type: mime }))
        }
      }
      if (files.length > 0) return files
    }
  } catch {
    // WebKitGTK / non-secure contexts may deny navigator.clipboard.read(); fall through to local host helper.
  }

  try {
    const response = await fetchImpl('/agent/clipboard-image', { cache: 'no-store' })
    if (response.status !== 200) return []
    const mime = ((response.headers.get('content-type') ?? '').split(';')[0] ?? '').trim().toLowerCase()
    if (!mime.startsWith('image/')) return []
    const blob = await response.blob()
    if (blob.size === 0) return []
    const ext = MIME_EXT[mime] ?? 'png'
    return [new File([blob], `clipboard-${Date.now()}.${ext}`, { type: mime })]
  } catch {
    return []
  }
}

export interface PasteEventLike {
  readonly clipboardData: DataTransfer | null | undefined
  preventDefault(): void
}

export interface KeyEventLike {
  readonly key: string
  readonly ctrlKey: boolean
  readonly metaKey: boolean
  readonly shiftKey: boolean
  readonly altKey: boolean
}

export function isPasteShortcut(event: KeyEventLike): boolean {
  return (event.ctrlKey || event.metaKey)
    && !event.shiftKey
    && !event.altKey
    && event.key.toLowerCase() === 'v'
}
