import type { DraftProject, ReportRow } from './api.ts'

export interface WorkContentParts {
  task: string
  today: string
  progress: string
}

const WORK_CONTENT_PATTERN =
  /^\s*1[.、]\s*具体任务或功能\s*[：:]\s*(.*?)\n\s*2[.、]\s*今日完成工作\s*[：:]\s*(.*?)\n\s*3[.、]\s*进度\s*[：:]?\s*(\d{1,3}|[^\n%]*?)\s*%?(?:\n\s*4[.、]\s*(?:明日计划|下一步计划|后续计划)\s*[：:]\s*([\s\S]*?))?\s*$/s
const WORK_CONTENT_GROUP_HEADER = /^\s*(?:\*\*)?【功能\s*\d+】(?:\*\*)?[ \t]*([^\n]*)\n([\s\S]*)$/
const WORK_CONTENT_GROUP_SEPARATOR = /\n+(?=\s*(?:\*\*)?【功能\s*\d+】)/

export function createEmptyWorkContentParts(): WorkContentParts {
  return { task: '', today: '', progress: '' }
}

export function formatWorkContent({ task, today, progress }: WorkContentParts): string {
  return `1. 具体任务或功能：${task}\n2. 今日完成工作：${today}\n3. 进度 ${progress} %`
}

export function createEmptyWorkContent(): string {
  return formatWorkContent(createEmptyWorkContentParts())
}

function parseStructuredSection(rawSection: string): WorkContentParts | null {
  const normalized = rawSection.replace(/\r\n/g, '\n').trim()
  let title = ''
  let body = normalized
  const headerMatch = normalized.match(WORK_CONTENT_GROUP_HEADER)
  if (headerMatch) {
    title = (headerMatch[1] ?? '').replace(/^\*\*|\*\*$/g, '').trim()
    body = (headerMatch[2] ?? '').trim()
  }
  const match = body.match(WORK_CONTENT_PATTERN)
  if (!match) return null

  const rawTask = (match[1] ?? '').trim()
  const rawToday = (match[2] ?? '').trim()
  const rawProgress = (match[3] ?? '').trim()
  const rawNextPlan = (match[4] ?? '').trim()

  const task = title
    ? rawTask
      ? rawTask.includes(title) ? rawTask : `${title}：${rawTask}`
      : title
    : rawTask
  const today = rawNextPlan
    ? rawToday
      ? `${rawToday}\n明日计划：${rawNextPlan}`
      : `明日计划：${rawNextPlan}`
    : rawToday

  return {
    task,
    today,
    progress: rawProgress,
  }
}

export function parseWorkContent(value?: string | null): WorkContentParts {
  const content = String(value ?? '').replace(/\r\n/g, '\n')
  const parsed = parseStructuredSection(content)
  if (parsed) return parsed
  return { task: content, today: '', progress: '' }
}

export function formatWorkContentGroups(groups: readonly WorkContentParts[]): string {
  if (groups.length <= 1) return formatWorkContent(groups[0] ?? createEmptyWorkContentParts())
  return groups.map((group, index) => `【功能 ${index + 1}】\n${formatWorkContent(group)}`).join('\n\n')
}

export function parseWorkContentGroups(value?: string | null): WorkContentParts[] {
  const content = String(value ?? '').replace(/\r\n/g, '\n').trim()
  if (!content) return [createEmptyWorkContentParts()]
  const sections = content.split(WORK_CONTENT_GROUP_SEPARATOR).map(s => s.trim()).filter(Boolean)
  if (sections.length > 1) {
    const parsed = sections.map(parseStructuredSection)
    if (parsed.every((item): item is WorkContentParts => item !== null)) {
      return parsed
    }
  }
  const single = parseStructuredSection(content)
  if (single) return [single]
  return [parseWorkContent(content)]
}

export function isStructuredWorkContent(value?: string | null): boolean {
  const content = String(value ?? '').replace(/\r\n/g, '\n').trim()
  if (!content) return true
  const sections = content.split(WORK_CONTENT_GROUP_SEPARATOR).map(s => s.trim()).filter(Boolean)
  if (sections.length > 1) {
    return sections.every(section => parseStructuredSection(section) !== null)
  }
  return parseStructuredSection(content) !== null
}

export function validateWorkContentParts({ task, today, progress }: WorkContentParts): string | null {
  if (!task.trim()) return '请填写具体任务或功能'
  if (!today.trim()) return '请填写今日完成工作'
  const cleanProgress = progress.trim()
  if (!/^\d{1,3}$/.test(cleanProgress)) return '请填写 0 到 100 之间的整数进度'
  const num = Number(cleanProgress)
  if (num < 0 || num > 100) return '进度只能填写 0 到 100 之间的整数'
  return null
}

export function validateWorkContent(value?: string | null): string | null {
  const groups = parseWorkContentGroups(value)
  for (let i = 0; i < groups.length; i += 1) {
    const prefix = groups.length > 1 ? `功能 ${i + 1}：` : ''
    const err = validateWorkContentParts(groups[i]!)
    if (err) return `${prefix}${err}`
  }
  return null
}

/** Non-blocking quality tips modeled on the team's daily-report guide. */
export function qualityWarnings(groups: readonly WorkContentParts[]): string[] {
  const notes: string[] = []
  groups.forEach((group, index) => {
    const prefix = groups.length > 1 ? `功能 ${index + 1}：` : ''
    const task = group.task.trim()
    const done = group.today.trim()
    if (task && done && task === done) {
      notes.push(`${prefix}「今日完成工作」与「具体任务」完全相同，建议补充今天的具体结果或变化量`)
    } else if (done && done.length < 10) {
      notes.push(`${prefix}「今日完成工作」较短，建议写明动作与结果`)
    }
  })
  return notes
}

/** Pull unfinished (< 100%) feature blocks from a prior report so today's form starts from real carry-over work. */
export function extractUnfinishedProjects(row: ReportRow): DraftProject[] {
  const result: DraftProject[] = []
  for (const project of row.projects ?? []) {
    const name = String(project.projectName ?? '').trim()
    const raw = String(project.workContent ?? '').trim()
    if (!name || !raw) continue
    const groups = parseWorkContentGroups(raw)
    const unfinished = groups
      .filter(group => {
        const pct = Number(group.progress.trim())
        return !Number.isFinite(pct) || pct < 100
      })
      .map(group => ({
        task: group.task.trim(),
        today: '',
        progress: group.progress.trim(),
      }))
    if (unfinished.length === 0) continue
    result.push({
      project_id: project.projectId === undefined || project.projectId === null ? null : Number(project.projectId),
      project_name: name,
      work_content: formatWorkContentGroups(unfinished),
    })
  }
  return result
}

export interface CalendarCell {
  readonly date: string | null
  readonly day: number
  readonly status: 'empty' | 'filled' | 'missed' | 'weekend' | 'future'
  readonly overtimeHours: number
  readonly leaveHours: number
  readonly row?: ReportRow | undefined
}

export interface MonthCalendar {
  readonly yearMonth: string
  readonly cells: readonly CalendarCell[]
  readonly filledDays: number
  readonly missedWorkdays: number
  readonly overtimeHours: number
  readonly leaveHours: number
  readonly reimbursement: number
}

/** Monday-first monthly calendar built directly from the user's own reports in that month. */
export function buildMonthCalendar(yearMonth: string, myRows: readonly ReportRow[], todayStr: string): MonthCalendar {
  const [yRaw, mRaw] = yearMonth.split('-')
  const year = Number(yRaw)
  const month = Number(mRaw)
  if (!Number.isInteger(year) || !Number.isInteger(month) || month < 1 || month > 12) {
    return { yearMonth, cells: [], filledDays: 0, missedWorkdays: 0, overtimeHours: 0, leaveHours: 0, reimbursement: 0 }
  }
  const byDate = new Map<string, ReportRow>()
  let overtimeHours = 0
  let leaveHours = 0
  let reimbursement = 0
  for (const row of myRows) {
    const date = String(row.reportDate ?? '').slice(0, 10)
    if (!date.startsWith(yearMonth + '-')) continue
    byDate.set(date, row)
    overtimeHours += Number(row.overtimeHours ?? 0) || 0
    leaveHours += Number(row.leaveHours ?? 0) || 0
    reimbursement += Number(row.reimbursement ?? 0) || 0
  }

  const firstWeekday = (new Date(year, month - 1, 1).getDay() + 6) % 7 // Mon=0 .. Sun=6
  const daysInMonth = new Date(year, month, 0).getDate()
  const cells: CalendarCell[] = []
  for (let i = 0; i < firstWeekday; i += 1) {
    cells.push({ date: null, day: 0, status: 'empty', overtimeHours: 0, leaveHours: 0 })
  }

  let filledDays = 0
  let missedWorkdays = 0
  for (let day = 1; day <= daysInMonth; day += 1) {
    const date = `${year}-${String(month).padStart(2, '0')}-${String(day).padStart(2, '0')}`
    const weekday = (firstWeekday + day - 1) % 7
    const isWeekend = weekday >= 5
    const row = byDate.get(date)
    let status: CalendarCell['status']
    if (row) {
      status = 'filled'
      filledDays += 1
    } else if (date > todayStr) {
      status = 'future'
    } else if (isWeekend) {
      status = 'weekend'
    } else {
      status = 'missed'
      if (date < todayStr) missedWorkdays += 1
    }
    cells.push({
      date,
      day,
      status,
      overtimeHours: Number(row?.overtimeHours ?? 0) || 0,
      leaveHours: Number(row?.leaveHours ?? 0) || 0,
      row,
    })
  }
  while (cells.length % 7 !== 0) {
    cells.push({ date: null, day: 0, status: 'empty', overtimeHours: 0, leaveHours: 0 })
  }
  return { yearMonth, cells, filledDays, missedWorkdays, overtimeHours, leaveHours, reimbursement }
}
