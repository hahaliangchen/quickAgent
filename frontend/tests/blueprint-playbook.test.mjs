import test from 'node:test'
import assert from 'node:assert/strict'
import {
  buildDistillPlaybookPrompt,
  buildMergeObserverSubmissionPrompt,
  buildRunPlaybookPrompt,
  buildSyncPlaybookPrompt,
  filterWorkspaceSessions,
} from '../src/builtin_plugins/blueprint/playbookPrompts.ts'

test('filterWorkspaceSessions filters by normalized workspace and sorts newest first', () => {
  const tasks = [
    { task_id: 'old', title: 'Old task', workspace: '/repo/a/', updated_at: 100 },
    { task_id: 'other', title: 'Other repo', workspace: '/repo/b', updated_at: 300 },
    { task_id: 'new', title: 'Build desktop release', workspace: '/repo/a', updated_at: 200 },
  ]
  const onlyA = filterWorkspaceSessions(tasks, '/repo/a')
  assert.deepEqual(onlyA.map(t => t.task_id), ['new', 'old'])

  const queried = filterWorkspaceSessions(tasks, '/repo/a', 'desktop')
  assert.deepEqual(queried.map(t => t.task_id), ['new'])

  const all = filterWorkspaceSessions(tasks, '/repo/a', '', true)
  assert.deepEqual(all.map(t => t.task_id), ['other', 'new', 'old'])
})

test('buildDistillPlaybookPrompt emits concise factual trigger without repeating SKILL.md SOP walls', () => {
  const prompt = buildDistillPlaybookPrompt(
    [{ task_id: 'task-abc', title: '排查打包与重启' }],
    [{ id: 'release-path', title: '打包发版最短路径' }],
    '只保留构建和验证步骤',
  )
  assert.match(prompt, /task-abc/)
  assert.match(prompt, /排查打包与重启/)
  assert.match(prompt, /release-path/)
  assert.match(prompt, /只保留构建和验证步骤/)
  assert.ok(prompt.length < 240, `prompt should stay concise, got ${prompt.length} chars`)
})

test('buildRunPlaybookPrompt warns when anchor files are drifted or missing', () => {
  const pb = {
    id: 'release-path',
    title: '发版路径',
    trigger: '修改后端代码后',
    summary: '最短构建验证路径',
    code: '// step 1',
    anchor_files: ['src/main.rs'],
    related_entities: [],
    related_graphs: [],
    verify_commands: ['cargo test'],
    pitfalls: [],
    source_tasks: [],
    updated_at: '',
  }
  const okPrompt = buildRunPlaybookPrompt(pb, { status: 'ok', missing_files: [], changed_files: [] })
  assert.match(okPrompt, /release-path/)
  assert.doesNotMatch(okPrompt, /drifted/)

  const driftedPrompt = buildRunPlaybookPrompt(pb, {
    status: 'drifted',
    missing_files: [],
    changed_files: ['src/main.rs'],
  })
  assert.match(driftedPrompt, /drifted/)
  assert.match(driftedPrompt, /src\/main\.rs/)

  const syncPrompt = buildSyncPlaybookPrompt(pb, {
    status: 'missing',
    missing_files: ['src/old.rs'],
    changed_files: [],
  })
  assert.match(syncPrompt, /src\/old\.rs/)
  assert.match(syncPrompt, /release-path/)
})

test('buildMergeObserverSubmissionPrompt carries concise submission reference without SOP walls', () => {
  const prompt = buildMergeObserverSubmissionPrompt(
    {
      id: 'obs-12345678',
      source_task_id: 'task-battle-9',
      source_task_title: '201 端口升级',
      title: '201 :3002 升级清单同步',
      trigger: '更新 201 QuickAgent 时',
      summary: '替换二进制时必须同步内置插件清单',
      anchor_files: ['src/server/skill_packages.rs'],
      environment: ['201 :3002 验证端口'],
      pitfalls: ['不要漏更新 builtin_plugins 清单'],
      verify_commands: ['curl -s http://127.0.0.1:3002/agent/skill-packages'],
      status: 'pending',
      created_at: '2026-11-27T05:00:00Z',
      updated_at: '2026-11-27T05:00:00Z',
    },
    [{ id: 'deploy-201', title: '201 部署最短路径' }],
    [{ id: 'plugin-host', title: '插件宿主画布' }],
  )
  assert.match(prompt, /obs-12345678/)
  assert.match(prompt, /task-battle-9/)
  assert.match(prompt, /deploy-201/)
  assert.match(prompt, /plugin-host/)
  assert.ok(prompt.length < 260, `prompt should stay concise, got ${prompt.length} chars`)
})
