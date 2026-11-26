/**
 * Persistent recent-files store and path helpers for Office Studio's WPS-style File menu.
 */

export interface OfficeRecentFile {
  readonly id: string
  readonly path?: string | undefined
  readonly filename: string
  readonly dir: string
  readonly slotId: string
  readonly fileUrl: string
  readonly openedAt: number
}

export interface UpsertOfficeRecentInput {
  readonly path?: string | undefined
  readonly filename: string
  readonly slotId: string
  readonly fileUrl: string
  readonly openedAt?: number | undefined
}

const STORAGE_KEY = 'quickagent_office_recent_files_v1'
export const MAX_RECENT_OFFICE_FILES = 15

const EXTENSION_TO_SLOT: Readonly<Record<string, string>> = {
  md: 'markdown',
  markdown: 'markdown',
  docx: 'word',
  xlsx: 'excel',
  pptx: 'ppt',
  pdf: 'pdf',
}

export const OFFICE_FILE_EXTENSIONS = ['md', 'markdown', 'docx', 'xlsx', 'pptx', 'pdf'] as const

export function inferOfficeSlotId(filename: string): string | undefined {
  const clean = filename.trim()
  const dot = clean.lastIndexOf('.')
  if (dot <= 0 || dot === clean.length - 1) return undefined
  const ext = clean.slice(dot + 1).toLowerCase()
  return EXTENSION_TO_SLOT[ext]
}

export function basenameOfPath(filePath: string): string {
  const normalized = filePath.replaceAll('\\', '/').replace(/\/+$/u, '')
  const idx = normalized.lastIndexOf('/')
  return idx >= 0 ? normalized.slice(idx + 1) : normalized
}

export function parentDirOfPath(filePath: string): string {
  const normalized = filePath.replaceAll('\\', '/').replace(/\/+$/u, '')
  const idx = normalized.lastIndexOf('/')
  if (idx < 0) return ''
  if (idx === 0) return '/'
  const dir = normalized.slice(0, idx)
  if (/^[A-Za-z]:$/u.test(dir)) return `${dir}/`
  return dir
}

export function officeLocalFileUrl(absPath: string): string {
  return `/agent/office/local-file?${new URLSearchParams({ path: absPath }).toString()}`
}

export function upsertRecentOfficeFile(
  list: readonly OfficeRecentFile[],
  input: UpsertOfficeRecentInput,
  limit = MAX_RECENT_OFFICE_FILES,
): OfficeRecentFile[] {
  const cleanPath = input.path?.trim() || undefined
  const filename = input.filename.trim() || (cleanPath ? basenameOfPath(cleanPath) : '')
  if (!filename || !input.fileUrl.trim() || !input.slotId.trim()) return [...list]
  const id = cleanPath ?? input.fileUrl
  const next: OfficeRecentFile = {
    id,
    ...(cleanPath ? { path: cleanPath } : {}),
    filename,
    dir: cleanPath ? parentDirOfPath(cleanPath) : '',
    slotId: input.slotId,
    fileUrl: input.fileUrl,
    openedAt: input.openedAt ?? Date.now(),
  }
  const filtered = list.filter(item => item.id !== id && (!cleanPath || item.path !== cleanPath) && item.fileUrl !== input.fileUrl)
  return [next, ...filtered].slice(0, Math.max(1, limit))
}

export function removeRecentOfficeFile(
  list: readonly OfficeRecentFile[],
  id: string,
): OfficeRecentFile[] {
  return list.filter(item => item.id !== id)
}

export function loadRecentOfficeFiles(storage?: Pick<Storage, 'getItem'>): OfficeRecentFile[] {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    const raw = store?.getItem(STORAGE_KEY)
    if (!raw) return []
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    const valid: OfficeRecentFile[] = []
    for (const item of parsed) {
      if (!item || typeof item !== 'object') continue
      const rec = item as Record<string, unknown>
      if (
        typeof rec.id === 'string' &&
        typeof rec.filename === 'string' &&
        typeof rec.slotId === 'string' &&
        typeof rec.fileUrl === 'string' &&
        typeof rec.openedAt === 'number'
      ) {
        valid.push({
          id: rec.id,
          ...(typeof rec.path === 'string' && rec.path.trim() ? { path: rec.path } : {}),
          filename: rec.filename,
          dir: typeof rec.dir === 'string' ? rec.dir : '',
          slotId: rec.slotId,
          fileUrl: rec.fileUrl,
          openedAt: rec.openedAt,
        })
      }
    }
    return valid.slice(0, MAX_RECENT_OFFICE_FILES)
  } catch {
    return []
  }
}

export function saveRecentOfficeFiles(
  list: readonly OfficeRecentFile[],
  storage?: Pick<Storage, 'setItem'>,
): void {
  try {
    const store = storage ?? (typeof localStorage !== 'undefined' ? localStorage : undefined)
    store?.setItem(STORAGE_KEY, JSON.stringify(list.slice(0, MAX_RECENT_OFFICE_FILES)))
  } catch {
    // Ignore quota / private-mode failures.
  }
}

export function formatRelativeOpenedAt(openedAt: number, now = Date.now()): string {
  const diffSec = Math.max(0, Math.floor((now - openedAt) / 1000))
  if (diffSec < 60) return '刚刚'
  const diffMin = Math.floor(diffSec / 60)
  if (diffMin < 60) return `${diffMin} 分钟前`
  const diffHour = Math.floor(diffMin / 60)
  if (diffHour < 24) return `${diffHour} 小时前`
  const diffDay = Math.floor(diffHour / 24)
  if (diffDay === 1) return '昨天'
  if (diffDay < 7) return `${diffDay} 天前`
  const date = new Date(openedAt)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${month}-${day}`
}
