import type { AgentEvent, SkillPackage } from './api.ts'
import { buildStudioCatalog } from './officeStudio.ts'

export const TEXT_PREVIEW_LIMIT = 512 * 1024
export type PreviewTheme = 'light' | 'dark'
export type OpenFilePreview = (path: string, options?: { line?: number; url?: string }) => void
export type PendingOpen = { slotId: string; fileUrl: string; filename: string; seq: number }
export type PreviewFile = { url: string; path: string; name: string; line?: number; attachment: boolean }

const images = new Set('png jpg jpeg webp gif svg avif bmp ico'.split(' '))
const text = new Set('txt log csv tsv ts tsx js jsx mjs cjs css scss html htm xml json jsonc jsonl yaml yml toml ini cfg conf env rs go py sh bash zsh fish ps1 bat c cpp h hpp java kt kts cs swift rb php sql lua vue svelte diff patch gradle proto graphql lock makefile dockerfile gitignore gitattributes editorconfig'.split(' '))
const office = new Map([['pdf','pdf'], ['xlsx','excel'], ['docx','word'], ['pptx','ppt']])
const extension = (name: string) => name.toLowerCase().split('.').at(-1) ?? ''

export function previewKind(name: string) {
  const ext = extension(name)
  if (ext === 'md' || ext === 'markdown') return 'markdown'
  if (images.has(ext)) return 'image'
  if (office.has(ext)) return 'plugin'
  return text.has(ext) ? 'code' : 'unsupported'
}

export function previewModule(name: string, packages: readonly SkillPackage[]) {
  const slot = previewKind(name) === 'markdown' ? 'markdown' : office.get(extension(name))
  if (!slot) return undefined
  const card = buildStudioCatalog(packages).find(card => card.slotId === slot && card.ready)
  const pkg = packages.find(item => item.id === card?.owner)
  if (!card?.owner || !card.page || !pkg || pkg.remote || !pkg.pages.some(page => page.id === card.page && page.renderer === 'html')) return undefined
  return { slotId: slot, owner: card.owner, page: card.page }
}

export function workspacePath(path: string, root = '') {
  if (path.startsWith('file://')) {
    const match = /^file:\/\/(?:localhost)?(\/.*)$/u.exec(path)
    if (match?.[1]) {
      try { path = decodeURIComponent(match[1]) } catch { return path }
      if (/^\/[A-Za-z]:\//u.test(path)) path = path.slice(1)
    }
  }
  if (/^[A-Za-z]:[\\/]/u.test(root)) {
    root = root.replaceAll('\\', '/')
    path = path.replaceAll('\\', '/')
  }
  root = root.replace(/\/+$/u, '')
  if (root && path.startsWith(root + '/')) path = path.slice(root.length + 1)
  return path.replace(/^(?:\.\/)+/u, '')
}

export function previewFile(task: string, path: string, options?: { line?: number; url?: string }, root = ''): PreviewFile | undefined {
  const base = '/agent/tasks/' + encodeURIComponent(task)
  const attachment = options?.url !== undefined
  const relative = attachment ? path : workspacePath(path, root)
  const prefix = base + '/attachments/'
  const url = options?.url ?? (base + '/workspace-file?' + new URLSearchParams({ path: relative }))
  if (!relative || (attachment && (!url.startsWith(prefix) || !/^[A-Za-z0-9_:%.-]+$/u.test(url.slice(prefix.length))))) return undefined
  return { url, path: relative, name: relative.split(/[\\/]/u).pop() || relative,
    ...(options?.line !== undefined ? { line: options.line } : {}), attachment }
}

export function studioUrl(owner: string, page: string, mode?: 'edit' | 'preview', file?: string, name?: string, theme?: PreviewTheme) {
  const base = '/agent/skill-packages/' + encodeURIComponent(owner) + '/pages/' + encodeURIComponent(page) + '/index'
  const query = new URLSearchParams()
  if (mode) query.set('mode', mode)
  if (file) query.set('file', file)
  if (name) query.set('name', name)
  if (theme) query.set('theme', theme)
  return query.size ? base + '?' + query : base
}

export function previewRevision(file: PreviewFile, events: readonly AgentEvent[], root: string) {
  if (file.attachment) return 0
  const summarized = new Set(events.filter(event => event.type === 'workspace/changes').map(event => event.data.turn))
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i]!
    if (event.type === 'turn/end' && !summarized.has(event.data.turn)) return event.seq
    if (event.type === 'workspace/changes' && Array.isArray(event.data.files) && event.data.files.some(item =>
      typeof item?.path === 'string' && workspacePath(item.path, root) === file.path)) return event.seq
  }
  return 0
}

export async function readPreviewText(url: string, signal: AbortSignal) {
  const response = await fetch(url, { signal, headers: { Range: 'bytes=0-' + (TEXT_PREVIEW_LIMIT - 1) }, cache: 'no-store' })
  if (response.status === 416 && response.headers.get('Content-Range') === 'bytes */0') return { text: '', truncated: false }
  if (!response.ok || !response.body) throw new Error('HTTP ' + response.status)
  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let length = 0
  let text = ''
  let truncated = Number(response.headers.get('Content-Range')?.split('/').pop() ?? response.headers.get('Content-Length')) > TEXT_PREVIEW_LIMIT
  try {
    for (;;) {
      const { value, done } = await reader.read()
      if (done) break
      const bytes = value.subarray(0, TEXT_PREVIEW_LIMIT - length)
      text += decoder.decode(bytes, { stream: true })
      length += bytes.length
      if (value.length > bytes.length || (truncated && length === TEXT_PREVIEW_LIMIT)) { truncated = true; break }
    }
  } finally { await reader.cancel().catch(() => {}) }
  return { text: text + decoder.decode(), truncated }
}
