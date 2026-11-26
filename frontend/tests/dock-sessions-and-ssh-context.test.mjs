import assert from 'node:assert/strict'
import test from 'node:test'
import { buildSshViewContext } from '../src/builtin_plugins/ssh/serverContext.ts'
import { filterDockSessions } from '../src/components/dockSessions.ts'
import { formatStudioViewContext, splitStudioViewContext } from '../src/plugins/studioContextText.ts'
import { extractDailyReportSyncAction, extractDbSyncAction } from '../src/plugins/studioSync.ts'

test('buildSshViewContext includes active server id, target, SFTP path, and zero secrets', () => {
  assert.equal(buildSshViewContext(undefined), null)
  const ctx = buildSshViewContext(
    {
      id: 'srv-201',
      name: '服务器3 A100',
      host: '10.10.10.201',
      port: 22,
      username: 'root',
      auth_type: 'password',
      env_tier: 'dev',
      require_approval: false,
      default_dir: '/opt/rag-go-kg-validation',
      tags: ['gpu', 'a100'],
      notes: '验证机',
      host_key_fingerprint: 'SHA256:abc',
      has_password: true,
      has_passphrase: false,
    },
    { sftpPath: '/opt/rag-go-kg-validation/tmp', activeTabKind: 'human' },
  )
  assert.ok(ctx)
  assert.equal(ctx.plugin, 'ssh')
  assert.equal(ctx.id, 'ssh:srv-201')
  assert.equal(ctx.badge, '远程服务器 · 服务器3 A100')
  assert.equal(ctx.detail, 'root@10.10.10.201:22 · /opt/rag-go-kg-validation/tmp')
  assert.match(ctx.text, /server_id: srv-201/)
  assert.match(ctx.text, /root@10\.10\.10\.201:22/)
  assert.match(ctx.text, /当前 SFTP 目录: \/opt\/rag-go-kg-validation\/tmp/)
  assert.match(ctx.text, /人工交互终端/)
  assert.doesNotMatch(ctx.text, /SHA256:abc|has_password/)

  // Round-trip through <studio-view-context> so ChatView cleanly separates the badge from user text.
  const wrapped = formatStudioViewContext(ctx) + '\n\n看看这里的日志'
  const parsed = splitStudioViewContext(wrapped)
  assert.equal(parsed.body, '看看这里的日志')
  assert.equal(parsed.context?.label, '远程服务器 · 服务器3 A100 · root@10.10.10.201:22 · /opt/rag-go-kg-validation/tmp')
})

test('filterDockSessions sorts running/pinned/recent, filters scopes & queries, and hides subagents', () => {
  const make = (overrides) => ({
    task_id: 't1',
    prompt: 'hello',
    kind: 'code',
    title: null,
    pinned: false,
    archived: false,
    workspace: '/ws/a',
    model: 'gpt-6.2-sol',
    status: 'completed',
    created_at: 100,
    updated_at: 100,
    parent_task_id: null,
    ...overrides,
  })
  const tasks = [
    make({ task_id: 'old', title: '旧会话', updated_at: 10 }),
    make({ task_id: 'pinned', title: '置顶会话', pinned: true, updated_at: 20 }),
    make({ task_id: 'run', title: '服务器3 A100', status: 'running', workspace: '/ws/b', updated_at: 5 }),
    make({ task_id: 'chat1', title: '通用聊天', kind: 'chat', workspace: null, updated_at: 30 }),
    make({ task_id: 'sub1', title: '子代理', parent_task_id: 'run', updated_at: 99 }),
    make({ task_id: 'arch', title: '已归档', archived: true, updated_at: 99 }),
    make({ task_id: 'empty_draft', prompt: '', status: 'draft', updated_at: 99 }),
  ]

  const all = filterDockSessions(tasks, { scope: 'all', workspace: '/ws/a', query: '' })
  assert.deepEqual(all.map(t => t.task_id), ['run', 'pinned', 'chat1', 'old'])

  const wsOnly = filterDockSessions(tasks, { scope: 'workspace', workspace: '/ws/a', query: '' })
  assert.deepEqual(wsOnly.map(t => t.task_id), ['pinned', 'old'])

  const chatOnly = filterDockSessions(tasks, { scope: 'chat', workspace: '/ws/a', query: '' })
  assert.deepEqual(chatOnly.map(t => t.task_id), ['chat1'])

  const searched = filterDockSessions(tasks, { scope: 'all', workspace: '/ws/a', query: 'a100' })
  assert.deepEqual(searched.map(t => t.task_id), ['run'])

  // Active draft stays visible when currently open.
  const withActiveDraft = filterDockSessions(tasks, {
    scope: 'all',
    workspace: '/ws/a',
    query: '',
    activeTaskId: 'empty_draft',
  })
  assert.ok(withActiveDraft.some(t => t.task_id === 'empty_draft'))
})

test('extractDbSyncAction keeps background db__query silent unless show_in_ui is true', () => {
  const silent = extractDbSyncAction(
    'db__query',
    { connection_id: 'c1', sql: 'SELECT COUNT(*) FROM wr_daily LIMIT 10' },
    { rows: [[300]] },
  )
  assert.equal(silent, undefined)

  const explicitFalse = extractDbSyncAction(
    'db__query',
    { connection_id: 'c1', sql: 'SELECT * FROM wr_daily LIMIT 10', show_in_ui: false },
    { rows: [] },
  )
  assert.equal(explicitFalse, undefined)

  const shown = extractDbSyncAction(
    'db__query',
    { connection_id: 'c1', sql: 'SELECT * FROM wr_daily WHERE user_name = "张三"', show_in_ui: true },
    { editable_table: { schema: 'report_system', table: 'wr_daily' } },
  )
  assert.deepEqual(shown, {
    action: 'query',
    connectionId: 'c1',
    sqlitePath: undefined,
    sql: 'SELECT * FROM wr_daily WHERE user_name = "张三"',
    schema: 'report_system',
    table: 'wr_daily',
  })

  const focused = extractDbSyncAction(
    'db__focus_view',
    { connection_id: 'c1', schema: 'report_system', table: 'wr_daily', column: 3, offset: 400 },
    { focused: true },
  )
  assert.deepEqual(focused, {
    action: 'focus_view',
    connectionId: 'c1',
    sqlitePath: undefined,
    schema: 'report_system',
    table: 'wr_daily',
    column: '3',
    row: undefined,
    offset: 400,
    range: undefined,
    findColumn: undefined,
    findValue: undefined,
  })
})

test('extractDailyReportSyncAction maps daily_report tool calls to live UI sync actions', () => {
  assert.deepEqual(
    extractDailyReportSyncAction(
      'daily_report__prepare_draft',
      { report_date: '2026-11-26', projects: [{ project_name: 'QuickAgent', work_content: 'test' }] },
      { stored: true, report_date: '2026-11-26' },
    ),
    { action: 'prepare_draft', reportDate: '2026-11-26' },
  )
  assert.deepEqual(
    extractDailyReportSyncAction(
      'daily_report__write_guide',
      { content: '# 日报规范' },
      { saved: true },
    ),
    { action: 'write_guide', content: '# 日报规范' },
  )
  assert.deepEqual(
    extractDailyReportSyncAction(
      'daily_report__submit',
      {},
      { submitted: true, report_date: '2026-11-26' },
    ),
    { action: 'submit', reportDate: '2026-11-26' },
  )
  assert.deepEqual(
    extractDailyReportSyncAction(
      'daily_report__today_activity',
      { date: '2026-11-25' },
      { date: '2026-11-25' },
    ),
    { action: 'today_activity', reportDate: '2026-11-25' },
  )
  assert.equal(extractDailyReportSyncAction('daily_report__read_guide', {}, {}), undefined)
})
