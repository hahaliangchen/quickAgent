import type { Task } from './api.ts'

export interface ShellStatus {
  readonly available: boolean
  readonly pwsh_detected?: boolean
  readonly cache?: { readonly validated: boolean; readonly cached: boolean; readonly checked_at: number | null }
  readonly platform: { readonly os: string; readonly arch: string }
  readonly executable?: string | null
  readonly version?: string | null
  readonly error?: string | null
  readonly install?: { readonly command: string; readonly store_url: string }
}

export interface ShellInstallProgress {
  readonly running: boolean
  readonly status: string
  readonly stdout?: string
  readonly stderr?: string
  readonly exit_code?: number | null
  readonly error?: string | null
}

/** Never probe dependencies while a plugin/chat session or an unloaded task is selected. */
export function isCodeShellContext(taskId: string | undefined, task: Task | undefined, mode: 'chat' | 'code', workspace: string): boolean {
  if (taskId !== undefined) {
    return task?.task_id === taskId && !task.plugin_id
      && (task.kind === 'code' || (task.kind === undefined && !!task.workspace))
  }
  return mode === 'code' && workspace !== ''
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/agent/settings/shell${path}`, init)
  const data: unknown = await response.json()
  if (!response.ok) {
    const error = typeof data === 'object' && data !== null && 'error' in data ? String(data.error) : response.statusText
    throw new Error(error)
  }
  return data as T
}

export const shellSetupApi = {
  status: (signal?: AbortSignal) => request<ShellStatus>('', signal ? { signal } : undefined),
  detect: (path?: string) => request<ShellStatus>('/detect', {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(path === undefined ? {} : { path }),
  }),
  install: () => request<ShellInstallProgress>('/install', { method: 'POST' }),
  progress: (signal?: AbortSignal) => request<ShellInstallProgress>('/install', signal ? { signal } : undefined),
  cancelInstall: () => request<ShellInstallProgress>('/install', { method: 'DELETE' }),
  openStore: () => request<unknown>('/store', { method: 'POST' }),
}
