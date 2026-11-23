import { createContext } from 'react'

export type Auth = 'agent' | 'key_file' | 'password'
export type Tier = 'dev' | 'staging' | 'prod'
export interface Details { name: string; env_tier: Tier; require_approval: boolean; default_dir: string | null; tags: string[]; notes: string | null }
export interface Server extends Details {
  id: string; host: string; port: number; username: string; auth_type: Auth
  host_key_fingerprint: string | null; has_password: boolean; has_passphrase: boolean
}
export interface Create extends Details { host: string; port: number; username: string; auth_type: Auth; key_path?: string; credential?: string }
export interface Access { task_id?: string; workspace_root?: string; approved?: boolean }
export interface Usage { id: number; task_id: string | null; action: string; summary: string; created_at: number }
export interface Entry { name: string; kind: 'directory' | 'file' | 'symlink' | 'other'; size: number | null }
export interface Failure { error?: string; code?: string; trusted?: string; received?: string }
export const remoteTools = ['ssh__exec', 'ssh__list_dir', 'ssh__read_file', 'ssh__write_file', 'ssh__upload', 'ssh__tail_log'] as const
export type RemoteTool = typeof remoteTools[number]
export interface AiFocus { seq: number; taskId: string; serverId: string; tool: RemoteTool; remotePath?: string }
export interface AiActivity { type: 'activity'; action: string; remote_path: string | null; detail: string }
export interface WorkbenchContext {
  access: Access
  aiFocus?: AiFocus | undefined
  followAi: boolean
  onToggleFollowAi?: (enabled: boolean) => void
  openTask?: (id: string) => void
  titleFor?: (id: string) => string | undefined
  diagnose?: (selection: string) => void
}
export const SshContext = createContext<WorkbenchContext>({ access: {}, followAi: true })

export class SshError extends Error {
  constructor(readonly detail: Failure, status = 0) { super(detail.error ?? (status ? 'SSH 请求失败（' + status + '）' : 'SSH 请求失败')) }
  get keyChanged() { return this.detail.code === 'HOST_KEY_MISMATCH' && !!this.detail.trusted && !!this.detail.received }
}

const root = '/agent/ssh/servers'
const at = (id: string, suffix = '') => root + '/' + encodeURIComponent(id) + suffix
function remote(id: string, operation: string, path: string, access: Access = {}): string {
  const query = new URLSearchParams({ path })
  if (access.approved) query.set('approved', 'true')
  if (access.task_id) query.set('task_id', access.task_id)
  if (access.workspace_root) query.set('workspace_root', access.workspace_root)
  return at(id, '/sftp/' + operation) + '?' + query
}

async function request<T>(url: string, method = 'GET', data?: unknown, signal?: AbortSignal): Promise<T> {
  const result = await fetch(url, { method, ...(signal ? { signal } : {}), ...(data === undefined ? {} : { body: JSON.stringify(data), headers: { 'content-type': 'application/json' } }) })
  if (!result.ok) throw new SshError(await result.json().catch(() => ({})), result.status)
  return result.status === 204 ? undefined as T : await result.json() as T
}

function stream(id: string, endpoint: string, ticket: string): WebSocket {
  const url = new URL(at(id, endpoint), location.origin)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  url.searchParams.set('ticket', ticket)
  const socket = new WebSocket(url)
  socket.binaryType = 'arraybuffer'
  return socket
}

export const ssh = {
  list: () => request<Server[]>(root),
  create: (input: Create) => request<Server>(root, 'POST', input),
  update: (id: string, input: Details) => request<Server>(at(id), 'PATCH', input),
  delete: (id: string) => request<void>(at(id), 'DELETE'),
  credential: (id: string, value: string | null) => request<void>(at(id, '/credential'), 'PUT', { value }),
  test: (id: string) => request<Server>(at(id, '/test'), 'POST'),
  trust: (id: string, detail: Failure) => request<void>(at(id, '/host-key'), 'PUT', { trusted: detail.trusted, received: detail.received }),
  history: (id: string, signal?: AbortSignal) => request<{ history: Usage[] }>(at(id, '/history'), 'GET', undefined, signal),
  ticket: (id: string, access: Access, signal: AbortSignal) => request<{ ticket: string }>(at(id, '/terminal-ticket'), 'POST', access, signal),
  terminal: (id: string, ticket: string) => stream(id, '/terminal', ticket),
  aiStreamTicket: (id: string, taskId: string, signal: AbortSignal) => request<{ ticket: string }>(at(id, '/ai-stream-ticket'), 'POST', { task_id: taskId }, signal),
  aiStream: (id: string, ticket: string) => stream(id, '/ai-stream', ticket),
  entries: (id: string, path: string, signal?: AbortSignal) => request<{ path: string; entries: Entry[] }>(remote(id, 'list', path), 'GET', undefined, signal),
  preview: (id: string, path: string, access: Access) => request<{ text: string; truncated: boolean }>(remote(id, 'preview', path, access)),
  download: (id: string, path: string, access: Access) => remote(id, 'download', path, access),
  mutate: (id: string, operation: string, path: string, access: Access, destination?: string) => request<void>(at(id, '/sftp/mutate'), 'POST', { ...access, operation, path, destination }),
  upload: (id: string, path: string, file: File, access: Access, signal: AbortSignal, progress: (ratio: number) => void) => new Promise<void>((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    const abort = () => xhr.abort()
    if (signal.aborted) { reject(new DOMException('Upload cancelled', 'AbortError')); return }
    xhr.open('PUT', remote(id, 'upload', path, access))
    xhr.responseType = 'json'
    xhr.onload = () => xhr.status >= 200 && xhr.status < 300 ? resolve() : reject(new SshError(xhr.response ?? {}, xhr.status))
    xhr.onerror = () => reject(new SshError({ error: '上传连接中断' }))
    xhr.onabort = () => reject(new DOMException('Upload cancelled', 'AbortError'))
    xhr.onloadend = () => signal.removeEventListener('abort', abort)
    xhr.upload.onprogress = event => { if (event.lengthComputable && event.total > 0) progress(event.loaded / event.total) }
    signal.addEventListener('abort', abort, { once: true })
    xhr.send(file)
  }),
}

export function message(error: unknown): string { return error instanceof Error ? error.message : String(error) }
export function confirmAccess(server: Server, action: string): boolean {
  return !server.require_approval || window.confirm(server.name + ' · ' + server.username + '@' + server.host + ':' + server.port + '\n\n' + action)
}
export function joinRemote(dir: string, name: string): string { return dir === '/' ? '/' + name : dir.replace(/\/$/, '') + '/' + name }
