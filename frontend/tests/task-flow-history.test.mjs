import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/model.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { chatItems, splitTurnItems } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('progress messages and tools stay in order while the last reply remains the conclusion', () => {
  const events = [
    event('turn/start', { turn: 1 }),
    event('assistant/message', { turn: 1, step: 1, message: { content: '先检查。' } }),
    event('tool/call', { turn: 1, callId: 'one', name: 'read_file', arguments: '{}' }),
    event('assistant/message', { turn: 1, step: 2, message: { content: '发现问题，继续修复。' } }),
    event('tool/call', { turn: 1, callId: 'two', name: 'edit_file', arguments: '{}' }),
    event('assistant/message', { turn: 1, step: 3, message: { content: '已经完成。' } }),
  ]
  const items = chatItems(events).filter(item => item.kind === 'assistant' || item.kind === 'tool')
  const split = splitTurnItems(items)
  assert.deepEqual(split.process.map(item => item.kind === 'assistant' ? item.text : item.name), ['先检查。', 'read_file', '发现问题，继续修复。', 'edit_file'])
  assert.equal(split.conclusion.text, '已经完成。')
  const pending = splitTurnItems(items.slice(0, -1))
  assert.equal(pending.conclusion, undefined)
  assert.equal(pending.process.length, 4)
})
const event = (type, data) => ({ type, time: 0, data })

test('execution result is pending acceptance and the original specific gap is visible', () => {
  const events = [event('assistant/message', { acceptance_pending: true, message: { content: [{ type: 'text', text: '执行结果' }] } }),
    event('observer/retrospective', { status: 'completed', observer_return: { action: 'continue', summary: '缺少第 2 页截图',
      acceptance: { assessment: 'needs_work', requirements: [{ requirement: '保存第 2 页截图', result: 'unmet', explanation: '只有第 1 页图片' }] } } })]
  const items = chatItems(events)
  assert.match(items[0].text, /提交 Observer 验收/)
  assert.match(items[1].text, /需要补齐/)
  assert.match(items[1].text, /只有第 1 页图片/)
  assert.match(items[1].text, /交回 Organizer/)
})

test('ended runs retain only their explicit assessment without inferring goal completion', () => {
  const events = [event('turn/start', { turn: 1 }),
    event('turn/end', { turn: 1, reason: { kind: 'completed', goal_achieved: false, unresolved: ['视觉验收未完成'] } })]
  const reason = chatItems(JSON.parse(JSON.stringify(events))).find(item => item.kind === 'turn-end').reason
  assert.equal(reason.kind, 'completed')
  assert.equal(reason.goalAchieved, false)
  assert.deepEqual(reason.unresolved, ['视觉验收未完成'])
  const next = [...events, event('turn/start', { turn: 2 }),
    event('turn/end', { turn: 2, reason: { kind: 'completed', goal_achieved: true, unresolved: [] } })]
  assert.equal(chatItems(next).filter(item => item.kind === 'turn-end')[1].reason.goalAchieved, true)
  const unknown = [...events, event('turn/start', { turn: 3 }), event('user/message', { message: { content: '下一项任务' } })]
  assert.equal(chatItems(unknown, { status: 'completed', task_id: 'task' }).at(-1).reason.goalAchieved, undefined,
    'a previous turn assessment does not establish completion of the new goal')
})

test('Observer retrospective is visible after the answer and survives history reload', () => {
  const events = [event('turn/start', { turn: 1 }),
    event('user/message', { message: { content: [{ type: 'text', text: '运行示例' }] } }),
    event('assistant/message', { turn: 1, message: { content: [{ type: 'text', text: '已加载示例' }] } }),
    event('turn/end', { turn: 1, reason: { kind: 'completed' } }),
    event('observer/retrospective', { status: 'completed', summary: 'short host projection',
      observer_return: { summary: '加载成功，视觉失败已说明。', path_review: '重复探测没有增加证据。',
        shortening_opportunities: ['复用已有服务。'] } })]
  const task = { task_id: 'task', status: 'completed', created_at: 0, updated_at: 1 }
  const items = chatItems(JSON.parse(JSON.stringify(events)), task)
  const review = items.find(item => item.kind === 'retrospective')
  assert.match(review.text, /加载成功，视觉失败已说明/)
  assert.match(review.text, /重复探测/)
  assert.match(review.text, /复用已有服务/)
  assert.ok(items.indexOf(review) > items.findIndex(item => item.kind === 'assistant'))
  assert.equal(items.filter(item => item.kind === 'turn-end').length, 1)
})

test('six tool calls keep their results and failures after a completed history is reloaded', () => {
  const tools = ['http_probe', 'browser_open', 'browser_upload', 'browser_read', 'browser_screenshot', 'browser_diagnostics']
  const events = [event('turn/start', { turn: 1 })]
  for (let i = 0; i < tools.length; i++) {
    events.push(event('tool/call', { turn: 1, callId: `call_${i}`, name: tools[i], arguments: '{}' }))
    events.push(event('tool/result', { turn: 1, toolCallId: `call_${i}`, message: { toolCallId: `call_${i}`, isError: i === 5, content: [{ type: 'text', text: i === 5 ? 'original diagnostic error' : 'actual tool result' }] } }))
  }
  events.push(event('turn/end', { turn: 1, reason: { kind: 'completed' } }))
  const chat = chatItems(JSON.parse(JSON.stringify(events)))
  assert.deepEqual(chat.filter(item => item.kind === 'tool').map(item => item.name), tools)
  assert.equal(chat.find(item => item.kind === 'tool' && item.callId === 'call_5').result.text, 'original diagnostic error')
  assert.equal(chat.find(item => item.kind === 'tool' && item.callId === 'call_5').result.isError, true)
})


test('ordinary work messages survive history reload and image input records stay out of the transcript', () => {
  const limitation = '模型图像能力未知，未配置备用视觉服务；截图已保存，本次没有收到图像输入。'
  const events = [event('turn/start', { turn: 1 }),
    event('assistant/message', { turn: 1, step: 1, message: { content: '已经加载完成，继续读取页码。' } }),
    event('visual/input_result', { turn: 1, actor: 'worker', request_trace_id: 'visual-1', message: { role: 'system', content: limitation } }),
    event('turn/end', { turn: 1, reason: { kind: 'completed' } })]
  const restored = JSON.parse(JSON.stringify(events))
  const items = chatItems(restored)
  assert.equal(items.filter(item => item.kind === 'assistant').length, 1)
  assert.ok(!items.some(item => item.kind === 'assistant' && item.text === limitation), 'the image input record is model input, shown with the request context')
})
