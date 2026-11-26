/** The daily-report plugin's HTTP surface. Everything runs through the local
 * backend: the bearer token and the company address never leave the host. */
export interface DraftProject {
  project_id: number | null
  project_name: string
  work_content: string
}

export interface DailyDraft {
  report_date: string
  projects: DraftProject[]
  problems: string
  remark: string
  overtime_hours: number
  leave_hours: number
  reimbursement: number
  reimbursement_reason: string
  action: 'create' | 'update'
  report_id?: string | null
  origin: string
  updated_at: string
}

export interface DailySession {
  logged_in: boolean
  base_url: string
  user_id: string
  user_name: string
  nick_name: string
  updated_at: string
}

export interface DailyStatus {
  session: DailySession
  guide_dir: string
  guide_exists: boolean
  draft: DailyDraft | null
}

export interface ProjectOption {
  project_id: number | string | null
  project_name: string
}

export interface ReportRow {
  id?: number | string
  userId?: number | string
  reportDate?: string
  userName?: string
  nickName?: string
  problems?: string
  remark?: string
  overtimeHours?: number
  leaveHours?: number
  reimbursement?: number
  reimbursementReason?: string
  projects?: { projectId?: number | string; projectName?: string; workContent?: string }[]
  [key: string]: unknown
}

/** One session started today, with the id the AI follows back into its history. */
export interface TodaySession {
  task_id: string
  title: string
  status: string
  created_at: number
  updated_at?: number
  project: string
  workspace?: string
  plugin_id?: string | null
  files?: string[]
}

export interface TodayProject {
  project: string
  workspace: string
  sessions: TodaySession[]
  files: string[]
  commits: string[]
}

export interface TodayActivity {
  date: string
  projects: TodayProject[]
  /** The same sessions, ungrouped: the page offers them as AI context. */
  sessions?: TodaySession[]
  totals: { sessions: number; files: number; commits: number }
  warnings: string[]
}

/** Every session of the day, whichever grouping the server sent. */
export function todaySessions(activity: TodayActivity | undefined): TodaySession[] {
  if (!activity) return []
  if (activity.sessions && activity.sessions.length > 0) return [...activity.sessions]
  return activity.projects.flatMap(entry => entry.sessions ?? [])
}

/** A failed call keeps the backend's reason, and says whether it was the login. */
export class DailyError extends Error {
  readonly authExpired: boolean
  constructor(message: string, authExpired: boolean) {
    super(message)
    this.name = 'DailyError'
    this.authExpired = authExpired
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = init?.body === undefined ? undefined : { 'Content-Type': 'application/json' }
  const response = await fetch('/api/daily-report' + path, { ...init, ...(headers ? { headers } : {}) })
  const body = await response.json().catch(() => null) as { error?: string } | null
  if (!response.ok) {
    throw new DailyError(body?.error ?? '日报请求失败（' + response.status + '）', response.status === 401)
  }
  return body as T
}

/** The AI tool finished a draft; the page behind it has been opened already. */
export const DRAFT_READY = 'daily-report:draft-ready'

export function announceDraftReady() {
  window.dispatchEvent(new CustomEvent(DRAFT_READY))
}

export function onDraftReady(handler: () => void): () => void {
  window.addEventListener(DRAFT_READY, handler)
  return () => window.removeEventListener(DRAFT_READY, handler)
}

export const dailyApi = {
  status: () => request<DailyStatus>('/status'),
  login: (body: { base_url: string; username?: string; password?: string; token?: string; client_id?: string; tenant_id?: string }) =>
    request<DailyStatus>('/login', { method: 'POST', body: JSON.stringify(body) }),
  logout: () => request<{ logged_out: boolean }>('/logout', { method: 'POST' }),
  projects: () => request<{ projects: ProjectOption[] }>('/projects'),
  reports: (query: Record<string, string> = {}) => {
    const search = new URLSearchParams(Object.entries(query).filter(([, value]) => value.trim() !== ''))
    return request<{ rows: ReportRow[]; total: number }>('/reports?' + search)
  },
  submit: (draft: DailyDraft) => request<{ action: string; answer: unknown }>(
    draft.action === 'update' ? '/reports/update' : '/reports',
    { method: draft.action === 'update' ? 'PUT' : 'POST', body: JSON.stringify(draft) },
  ),
  remove: (id: string) => request<unknown>('/reports/' + encodeURIComponent(id), { method: 'DELETE' }),
  draft: () => request<{ draft: DailyDraft | null }>('/draft'),
  saveDraft: (draft: DailyDraft) => request<{ draft: DailyDraft }>('/draft', { method: 'PUT', body: JSON.stringify(draft) }),
  dropDraft: () => request<{ cleared: boolean }>('/draft', { method: 'DELETE' }),
  guide: () => request<{ content: string; path: string; guide_dir: string }>('/guide'),
  saveGuide: (content: string) => request<{ saved: boolean; bytes: number }>('/guide', { method: 'PUT', body: JSON.stringify({ content }) }),
  today: (workspaceRoot: string, date?: string) => {
    const search = new URLSearchParams({ workspace_root: workspaceRoot, ...(date ? { date } : {}) })
    return request<TodayActivity>('/today-activity?' + search)
  },
}

/** The local calendar day, which is what every report field is keyed by. */
export function today(now = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`
}

export function emptyDraft(date = today()): DailyDraft {
  return {
    report_date: date,
    projects: [{ project_id: null, project_name: '', work_content: '' }],
    problems: '',
    remark: '',
    overtime_hours: 0,
    leave_hours: 0,
    reimbursement: 0,
    reimbursement_reason: '',
    action: 'create',
    report_id: null,
    origin: 'manual',
    updated_at: '',
  }
}
