import assert from 'node:assert/strict'
import test from 'node:test'
import { buildPrompt, buildReportContext, draftLines, sessionLine, sessionLines } from '../src/builtin_plugins/daily-report/prompt.ts'

const session = (id, title, project, files = []) => ({
  task_id: id, title, status: 'finished', created_at: 1, project, files,
})

const draft = (projects, extra = {}) => ({
  report_date: '2026-11-26',
  projects,
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
  ...extra,
})

const activity = {
  date: '2026-11-26',
  projects: [{ project: 'codex-workspace-mcp', workspace: '/w', sessions: [], files: ['src/a.rs', 'src/b.rs'], commits: ['abc123 日报页重做'] }],
  totals: { sessions: 1, files: 2, commits: 1 },
  warnings: [],
}

test('a session line always carries the id the AI has to follow', () => {
  assert.equal(sessionLine(session('t9', '日报页重做', 'codex-workspace-mcp', ['a', 'b'])), '- 日报页重做（项目 codex-workspace-mcp，今天改了 2 个文件）task_id=t9')
  assert.match(sessionLines([]), /没有勾选会话/)
})

test('the draft prompt names the date, the picked sessions and the tool that stores the draft', () => {
  const text = buildPrompt({
    kind: 'draft',
    date: '2026-11-26',
    draft: draft([{ project_id: 1, project_name: '数科运营项目', work_content: '1. 日报页\n2. 改完' }]),
    sessions: [session('t1', '日报页重做', 'codex-workspace-mcp'), session('t2', '代码评审', 'docx-editor-engine')],
    activity,
  })
  assert.match(text, /今天是 2026-11-26/)
  assert.match(text, /task_id=t1/)
  assert.match(text, /task_id=t2/)
  assert.match(text, /daily_report__read_guide/)
  assert.match(text, /daily_report__prepare_draft/)
  assert.match(text, /report_date=2026-11-26/)
  assert.match(text, /abc123 日报页重做/)
  assert.doesNotMatch(text, /daily_report__submit/, 'the AI must never submit on its own')
})

test('the review prompt shows the draft as it stands and asks for advice only', () => {
  const text = buildPrompt({
    kind: 'review',
    date: '2026-11-26',
    draft: draft([{ project_id: null, project_name: '数科运营项目', work_content: '1. 改下拉\n2. 完成' }], { problems: '下拉太长', action: 'update' }),
    sessions: [],
  })
  assert.match(text, /提交会覆盖今天已有的日报/)
  assert.match(text, /数科运营项目/)
  assert.match(text, /遇到的问题是|遇到的问题：下拉太长/)
  assert.match(text, /只给修改建议/)
})

test('the custom prompt keeps the user words and clips absurd input', () => {
  const text = buildPrompt({ kind: 'custom', date: '2026-11-26', draft: draft([]), sessions: [], text: 'x'.repeat(4000) })
  assert.ok(text.includes('x'.repeat(1999) + '…'), 'long text is clipped, not dropped')
  assert.match(buildPrompt({ kind: 'custom', date: '2026-11-26', draft: draft([]), sessions: [], text: '今天主要在评审' }), /今天主要在评审/)
})

test('the attached context is a snapshot the model can follow without the UI', () => {
  const text = buildReportContext({
    date: '2026-11-26',
    draft: draft([{ project_id: null, project_name: '数科运营项目', work_content: '1. 改下拉' }]),
    sessions: [session('t1', '日报页重做', 'codex-workspace-mcp')],
    activity,
  })
  assert.match(text, /日报日期：2026-11-26/)
  assert.match(text, /数科运营项目/)
  assert.match(text, /task_id=t1/)
  assert.match(draftLines(draft([])), /表单还是空的/)
})
