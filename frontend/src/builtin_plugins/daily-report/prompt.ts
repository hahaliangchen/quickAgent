import type { DailyDraft, TodayActivity, TodayProject, TodaySession } from './api.ts'

/** What the page asks the AI to do; each one is a different question about the same day. */
export type AskKind = 'draft' | 'summary' | 'review' | 'custom'

export interface AskInput {
  readonly kind: AskKind
  readonly date: string
  readonly draft: DailyDraft
  readonly sessions: readonly TodaySession[]
  readonly activity?: TodayActivity | undefined
  /** The user's own words, for `custom`. */
  readonly text?: string | undefined
}

const QUOTE = '「'
const CLOSE = '」'

/** Text the model can read without the UI; long values are clipped, not dropped. */
function clip(text: string, limit: number): string {
  const flat = text.replace(/\r\n/g, '\n').trim()
  if (flat.length <= limit) return flat
  return flat.slice(0, limit - 1) + '…'
}

/** One session, named with the id the AI must follow back into its history. */
export function sessionLine(session: TodaySession): string {
  const files = session.files?.length ?? 0
  const project = session.project || '未归属项目'
  const detail = files > 0 ? `，今天改了 ${files} 个文件` : ''
  return `- ${session.title || '（没有标题）'}（项目 ${project}${detail}）task_id=${session.task_id}`
}

/** The picked sessions, or an explicit note that nothing was picked. */
export function sessionLines(sessions: readonly TodaySession[]): string {
  if (sessions.length === 0) return '- （没有勾选会话：需要细节时先问我，或让我用 daily_report__today_activity 汇总）'
  return sessions.map(sessionLine).join('\n')
}

/** The report as it stands on screen, so the AI reviews what the user sees. */
export function draftLines(draft: DailyDraft): string {
  const rows = draft.projects
    .filter(project => project.project_name.trim() || project.work_content.trim())
    .map((project, index) => {
      const name = project.project_name.trim() || '（未选项目）'
      const content = clip(project.work_content, 600) || '（没有写工作内容）'
      return `${index + 1}. 项目 ${QUOTE}${name}${CLOSE}\n${content.split('\n').map(line => '   ' + line).join('\n')}`
    })
  const extras: string[] = []
  if (draft.problems.trim()) extras.push(`遇到的问题：${clip(draft.problems, 300)}`)
  if (draft.remark.trim()) extras.push(`备注：${clip(draft.remark, 300)}`)
  if (draft.overtime_hours > 0) extras.push(`加班：${draft.overtime_hours} 小时`)
  if (draft.leave_hours > 0) extras.push(`请假：${draft.leave_hours} 小时`)
  if (draft.reimbursement > 0) extras.push(`报销：${draft.reimbursement} 元（${clip(draft.reimbursement_reason, 120) || '没有写事由'}）`)
  return [rows.join('\n\n') || '1. （表单还是空的）', ...extras].join('\n')
}

/** The day's facts as the local aggregation sees them: no interpretation added. */
export function activityLines(activity: TodayActivity | undefined): string {
  if (!activity || activity.projects.length === 0) return '- （今天没有自动汇总到工作记录）'
  const projects: readonly TodayProject[] = activity.projects
  return projects.map(entry => {
    const parts = [`${entry.project}：${entry.sessions.length} 个会话`]
    if (entry.commits.length > 0) parts.push(`${entry.commits.length} 次提交`)
    if (entry.files.length > 0) parts.push(`${entry.files.length} 个改动文件`)
    const files = entry.files.slice(0, 6).join('、')
    const more = entry.files.length > 6 ? ' 等' : ''
    const lines = [`- ${parts.join('，')}${files ? `（${files}${more}）` : ''}`]
    // A commit subject is the day's most quotable fact, so it travels with the prompt.
    for (const commit of entry.commits.slice(0, 3)) lines.push(`    · 提交 ${commit}`)
    return lines.join('\n')
  }).join('\n')
}

/** The prompt each button sends. Facts first, then exactly what to do with them. */
export function buildPrompt(input: AskInput): string {
  const { kind, date, draft, sessions, activity } = input
  const picked = sessionLines(sessions)
  const head = `今天是 ${date}。`
  if (kind === 'summary') {
    return [
      `${head}请读下面这些会话，按项目归纳我今天做了什么，每件事一行、说清结果。`,
      '',
      '会话（用 session.read_session_history 逐个读全文，不要只看标题）：',
      picked,
      '',
      '本地汇总（可以直接引用）：',
      activityLines(activity),
      '',
      '只写能核实的事情，不要编造；也不要写日报格式、不要提交任何东西。',
    ].join('\n')
  }
  if (kind === 'review') {
    return [
      `${head}这是我正在填的日报草稿（日期 ${date}，${draft.action === 'update' ? '提交会覆盖今天已有的日报' : '今天还没有提交过'}）：`,
      '',
      draftLines(draft),
      '',
      '请对照写作规范和下面这些会话检查这份草稿：',
      picked,
      '',
      '指出：不可核实或夸大的表述、漏掉的今天做完的事、该合并或拆开的项目段、措辞问题。',
      '规范用 daily_report__read_guide 读。只给修改建议，不要提交，也不要直接改我的表单。',
    ].join('\n')
  }
  if (kind === 'custom') {
    const text = clip(input.text ?? '', 2000)
    return [
      `${head}关于我的日报，我想说：`,
      text || '（没有输入内容）',
      '',
      '供参考的会话（需要细节时用 session.read_session_history，不要凭标题猜）：',
      picked,
      '',
      '我当前的草稿：',
      draftLines(draft),
      '',
      '需要改草稿时用 daily_report__prepare_draft 保存（report_date=' + date + '），我会在页面上确认后自己提交。',
    ].join('\n')
  }
  return [
    `${head}请帮我起草今天的团队日报。`,
    '',
    '我勾选的会话（用 session.read_session_history 逐个读，不要凭标题猜内容）：',
    picked,
    '',
    '今日事实（本地聚合，可直接引用）：',
    activityLines(activity),
    '',
    '按这个顺序做：',
    '1. 用 daily_report__read_guide 读写作规范；规范是空的就先问我口径，不要自己编一条。',
    '2. 读上面的会话，挑出今天真正完成的事，按项目分段。',
    `3. 用 daily_report__prepare_draft 保存草稿：report_date=${date}，每个项目的工作内容按「1. 具体任务或功能：…\n2. 今日完成工作：…」写。`,
    '4. 存好之后把草稿摘要发给我，我在页面上确认后再点提交。',
    '',
    '只写能核实的内容；不确定的地方直接问我，不要留占位符。',
  ].join('\n')
}

/** The snapshot the chat dock offers as attached context, so a follow-up needs no repetition. */
export function buildReportContext(input: Omit<AskInput, 'kind'>): string {
  const { date, draft, sessions, activity } = input
  const filled = draft.projects.filter(project => project.project_name.trim() || project.work_content.trim())
  return [
    `日报日期：${date}`,
    `动作：${draft.action === 'update' ? '更新今天已有的日报' : '新建今天的日报'}`,
    `表单里的项目：${filled.length > 0 ? filled.map(project => project.project_name.trim() || '（未选项目）').join('、') : '（还没有）'}`,
    '',
    '当前草稿：',
    draftLines(draft),
    '',
    '今日事实：',
    activityLines(activity),
    '',
    `勾选的会话（${sessions.length} 个）：`,
    sessionLines(sessions),
    '',
    '界面联动能力：调用 daily_report.prepare_draft 会实时填充左侧日报表单（项目、工作内容、问题、加班/请假/报销、备注），调用 daily_report.write_guide 会实时切换并更新「写作规范」页签，调用 daily_report.submit 经用户确认提交后会自动刷新历史日报。',
  ].join('\n')
}
