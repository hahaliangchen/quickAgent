import assert from 'node:assert/strict'
import test from 'node:test'
import {
  buildMonthCalendar,
  extractUnfinishedProjects,
  formatWorkContentGroups,
  isStructuredWorkContent,
  parseWorkContentGroups,
  qualityWarnings,
  validateWorkContent,
} from '../src/builtin_plugins/daily-report/workContent.ts'

test('workContent single and multi-group formatting round-trips cleanly', () => {
  const single = formatWorkContentGroups([{ task: '日报表单重构', today: '改为三段结构化输入并接入月历', progress: '90' }])
  assert.equal(single, '1. 具体任务或功能：日报表单重构\n2. 今日完成工作：改为三段结构化输入并接入月历\n3. 进度 90 %')
  assert.equal(isStructuredWorkContent(single), true)
  assert.deepEqual(parseWorkContentGroups(single), [{ task: '日报表单重构', today: '改为三段结构化输入并接入月历', progress: '90' }])
  assert.equal(validateWorkContent(single), null)

  const multi = formatWorkContentGroups([
    { task: '前端联调', today: '完成表单校验', progress: '100' },
    { task: '日历补填', today: '接入月度查询接口', progress: '60' },
  ])
  assert.match(multi, /^【功能 1】\n1\. 具体任务或功能：前端联调/)
  assert.match(multi, /\n\n【功能 2】\n1\. 具体任务或功能：日历补填/)
  assert.equal(isStructuredWorkContent(multi), true)
  assert.deepEqual(parseWorkContentGroups(multi), [
    { task: '前端联调', today: '完成表单校验', progress: '100' },
    { task: '日历补填', today: '接入月度查询接口', progress: '60' },
  ])
  assert.equal(validateWorkContent(multi), null)
})

test('validateWorkContent rejects missing fields and out-of-range progress', () => {
  assert.match(validateWorkContent(formatWorkContentGroups([{ task: '', today: '做了测试', progress: '80' }])) ?? '', /具体任务或功能/)
  assert.match(validateWorkContent(formatWorkContentGroups([{ task: '测试', today: '', progress: '80' }])) ?? '', /今日完成工作/)
  assert.match(validateWorkContent(formatWorkContentGroups([{ task: '测试', today: '完成回归', progress: '120' }])) ?? '', /0 到 100/)
  assert.equal(qualityWarnings([{ task: '全量数据入库处理', today: '全量数据入库处理', progress: '50' }]).length, 1)
})

test('extractUnfinishedProjects keeps only <100% feature blocks and clears today delta', () => {
  const carried = extractUnfinishedProjects({
    reportDate: '2026-11-25',
    projects: [
      {
        projectId: 101,
        projectName: '多agent协同开发',
        workContent: formatWorkContentGroups([
          { task: '已上线功能', today: '完成上线验收', progress: '100' },
          { task: '日报结构化编辑', today: '完成前端解析', progress: '70' },
        ]),
      },
      {
        projectId: 102,
        projectName: '已完成项目',
        workContent: formatWorkContentGroups([{ task: '收尾', today: '全部交付', progress: '100' }]),
      },
    ],
  })
  assert.equal(carried.length, 1)
  assert.equal(carried[0]?.project_id, 101)
  assert.equal(carried[0]?.project_name, '多agent协同开发')
  assert.deepEqual(parseWorkContentGroups(carried[0]?.work_content), [
    { task: '日报结构化编辑', today: '', progress: '70' },
  ])
})

test('buildMonthCalendar marks filled, missed weekdays, weekends, and sums hours', () => {
  const cal = buildMonthCalendar(
    '2026-11',
    [
      { id: 1, reportDate: '2026-11-02', overtimeHours: 2, leaveHours: 0, reimbursement: 50 },
      { id: 2, reportDate: '2026-11-03', overtimeHours: 0, leaveHours: 4, reimbursement: 0 },
    ],
    '2026-11-05',
  )
  assert.equal(cal.filledDays, 2)
  assert.equal(cal.missedWorkdays, 1) // 2026-11-04 (Wed) missed; 11-05 is today so not yet overdue
  assert.equal(cal.overtimeHours, 2)
  assert.equal(cal.leaveHours, 4)
  assert.equal(cal.reimbursement, 50)
  const byDate = new Map(cal.cells.filter(c => c.date).map(c => [c.date, c]))
  assert.equal(byDate.get('2026-11-01')?.status, 'weekend') // Sun
  assert.equal(byDate.get('2026-11-02')?.status, 'filled')
  assert.equal(byDate.get('2026-11-04')?.status, 'missed')
  assert.equal(byDate.get('2026-11-06')?.status, 'future')
})
