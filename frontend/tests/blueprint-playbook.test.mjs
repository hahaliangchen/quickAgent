import test from 'node:test'
import assert from 'node:assert/strict'
import {
  buildDistillPlaybookPrompt,
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

test('buildDistillPlaybookPrompt references session ids, existing playbooks, and upsert_playbook', () => {
  const prompt = buildDistillPlaybookPrompt(
    [{ task_id: 'task-abc', title: '排查打包与重启' }],
    [{ id: 'release-path', title: '打包发版最短路径' }],
    '只保留构建和验证步骤',
  )
  assert.match(prompt, /task-abc/)
  assert.match(prompt, /排查打包与重启/)
  assert.match(prompt, /release-path/)
  assert.match(prompt, /只保留构建和验证步骤/)
  assert.match(prompt, /blueprint\.inspect_session/)
  assert.match(prompt, /session\.read_session_history/)
  assert.match(prompt, /blueprint\.upsert_playbook/)
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
  assert.match(okPrompt, /锚点已同步/)
  assert.match(okPrompt, /blueprint\.read_playbook/)

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
  assert.match(syncPrompt, /blueprint\.upsert_playbook/)
})
