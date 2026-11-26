import type { DraftProject, ReportRow } from './api.ts'

export interface WorkContentParts {
  task: string
  today: string
  progress: string
}

const WORK_CONTENT_PATTERN = /^1\.\s*具体任务或功能：(.*?)\n2\.\s*今日完成工作：(.*?)\n3\.\s*进度\s*(.*?)\s*%\s*$/s
const WORK_CONTENT_GROUP_PATTERN = /^【功能\s+\d+】\n([\s\S]*)$/
const WORK_CONTENT_GROUP_SEPARATOR = /\n\n(?=【功能\s+\d+】\n)/

export function createEmptyWorkContentParts(): WorkContentParts {
  return { task: '', today: '', progress: '' }
}

export function formatWorkContent({ task, today, progress }: WorkContentParts): string {
  return `1. 具体任务或功能：${task}\n2. 今日完成工作：${today}\n3. 进度 ${progress} %`
}

export function createEmptyWorkContent(): string {
  return formatWorkContent(createEmptyWorkContentParts())
}

export function parseWorkContent(value?: string | null): WorkContentParts {
  const content = String(value ?? '').replace(/\r\n/g, '\n')
  const match = content.match(WORK_CONTENT_PATTERN)
  if (match) {
    return {
      task: match[1] ?? '',
      today: match[2] ?? '',
      progress: match[3] ?? '',
    }
  }
  return { task: content, today: '', progress: '' }
}

export function formatWorkContentGroups(groups: readonly WorkContentParts[]): string {
  if (groups.length <= 1) return formatWorkContent(groups[0] ?? createEmptyWorkContentParts())
  return groups.map((group, index) => `【功能 ${index + 1}】\n${formatWorkContent(group)}`).join('\n\n')
}

export function parseWorkContentGroups(value?: string | null): WorkContentParts[] {
  const content = String(value ?? '').replace(/\r\n/g, '\n').trim()
  if (!content) return [createEmptyWorkContentParts()]
  const sections = content.split(WORK_CONTENT_GROUP_SEPARATOR)
  if (sections.length > 1 && sections.every(section => WORK_CONTENT_GROUP_PATTERN.test(section))) {
    return sections.map(section => {
      const match = section.match(WORK_CONTENT_GROUP_PATTERN)
      return parseWorkContent(match?.[1])
    })
  }
  if (WORK_CONTENT_GROUP_PATTERN.test(content)) {
    const match = content.match(WORK_CONTENT_GROUP_PATTERN)
    return [parseWorkContent(match?.[1])]
  }
  return [parseWorkContent(content)]
}

export function isStructuredWorkContent(value?: string | null): boolean {
  const content = String(value ?? '').replace(/\r\n/g, '\n').trim()
  if (!content) return true
  const sections = content.split(WORK_CONTENT_GROUP_SEPARATOR)
  if (sections.length > 1) {
    return sections.every(section => {
      const match = section.match(WORK_CONTENT_GROUP_PATTERN)
      return !!match?.[1] && WORK_CONTENT_PATTERN.test(match[1])
    })
  }
  const body = WORK_CONTENT_GROUP_PATTERN.test(content)
    ? (content.match(WORK_CONTENT_GROUP_PATTERN)?.[1] ?? '')
    : content
  return WORK_CONTENT_PATTERN.test(body)
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
