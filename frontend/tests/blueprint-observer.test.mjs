import test from 'node:test'
import assert from 'node:assert/strict'
import {
  OBSERVER_TITLE_PREFIX,
  buildObserverInspectPrompt,
  buildObserverUserChatPrompt,
  detectObserverTrigger,
  extractLatestAssistantMessage,
  getObserverSessionId,
  isObserverOwnTask,
  loadObserverPrefs,
  parseObserverSuggestion,
  sanitizeObserverEventsForDisplay,
  saveObserverPrefs,
  setObserverSessionId,
  stripObserverSuggestionFence,
  stripObserverUserPromptMeta,
} from '../src/experimental/blueprintObserver/observerProtocol.ts'

function memoryStorage() {
  const map = new Map()
  return {
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => { map.set(k, v) },
  }
}

test('detectObserverTrigger prioritizes turn_failed and tool/compile errors', () => {
  const errEvents = [
    { seq: 1, time: 1, type: 'tool/call', data: { name: 'read_file' } },
    { seq: 2, time: 2, type: 'tool/result', data: { message: { isError: true, content: [{ text: 'old_text not found' }] } } },
  ]
  const trigger = detectObserverTrigger(errEvents, 0, true)
  assert.equal(trigger?.kind, 'error')
  assert.equal(trigger?.latestSeq, 2)
  assert.match(trigger?.detail ?? '', /old_text not found/)

  // Already inspected seq >= 2 yields no duplicate trigger.
  assert.equal(detectObserverTrigger(errEvents, 2, true), null)

  const failEvents = [
    ...errEvents,
    { seq: 3, time: 3, type: 'turn/end', data: { reason: { kind: 'error', message: 'HTTP 503 overloaded' } } },
  ]
  const failTrigger = detectObserverTrigger(failEvents, 2, false)
  assert.equal(failTrigger?.kind, 'turn_failed')
  assert.match(failTrigger?.detail ?? '', /503/)
})

test('detectObserverTrigger fires on semantic plan phase transitions and turn completion without step thresholds', () => {
  const plainTools = Array.from({ length: 20 }, (_, i) => ({
    seq: i + 1,
    time: i + 1,
    type: 'tool/call',
    data: { name: 'exec_command' },
  }))
  // Pure tool calls with no error/plan change do not trigger arbitrary step-count polling.
  assert.equal(detectObserverTrigger(plainTools, 0, true), null)

  const planEvents = [
    ...plainTools,
    { seq: 21, time: 21, type: 'plan/updated', data: { steps: [{ id: 's1', title: '验证线上服务', status: 'in_progress' }] } },
  ]
  const planTrigger = detectObserverTrigger(planEvents, 20, true)
  assert.equal(planTrigger?.kind, 'plan_phase')
  assert.equal(planTrigger?.detail, '验证线上服务')

  const doneEvents = [
    ...planEvents,
    { seq: 22, time: 22, type: 'turn/end', data: { reason: { kind: 'completed' } } },
  ]
  const doneTrigger = detectObserverTrigger(doneEvents, 21, false)
  assert.equal(doneTrigger?.kind, 'turn_completed')
})

test('parseObserverSuggestion supports interject, stop, stop_and_continue, and NO_ACTION', () => {
  const actionable = [
    '我通过 `blueprint__inspect_session` 检查了数据库记录，发现目录下有旧产物。',
    '```observer-suggestion',
    JSON.stringify({
      should_interject: true,
      action: 'interject',
      severity: 'blocker',
      title: '先清空旧中间产物',
      reason: '目录里残留上午的 publish.json，主会话可能误读旧结果。',
      interject: '请先删除 tmp/chunk_v2_verify/*.json 再验证，避免读到上午旧产物。',
    }),
    '```',
  ].join('\n')

  const parsed = parseObserverSuggestion(actionable, 42)
  assert.equal(parsed?.shouldInterject, true)
  assert.equal(parsed?.action, 'interject')
  assert.equal(parsed?.severity, 'blocker')
  assert.equal(parsed?.title, '先清空旧中间产物')

  const stopOnly = '```observer-suggestion\n{"should_interject": true, "action": "stop", "severity": "blocker", "title": "正在死循环重试", "reason": "连续 4 次同样报错", "interject": ""}\n```'
  const parsedStop = parseObserverSuggestion(stopOnly, 43)
  assert.equal(parsedStop?.shouldInterject, true)
  assert.equal(parsedStop?.action, 'stop')

  const stopAndContinue = '```observer-suggestion\n{"should_interject": true, "action": "stop_and_continue", "severity": "warn", "title": "停掉重跑", "reason": "方向偏了", "interject": "改用方案 B"}\n```'
  const parsedBoth = parseObserverSuggestion(stopAndContinue, 44)
  assert.equal(parsedBoth?.shouldInterject, true)
  assert.equal(parsedBoth?.action, 'stop_and_continue')
  assert.equal(parsedBoth?.interject, '改用方案 B')

  const noop = '```observer-suggestion\n{"should_interject": false, "severity": "tip", "reason": "一切正常"}\n```'
  const parsedNoop = parseObserverSuggestion(noop, 45)
  assert.equal(parsedNoop?.shouldInterject, false)
  assert.equal(parsedNoop?.interject, '')
})

test('buildObserverInspectPrompt and buildObserverUserChatPrompt include SQLite inspection & action schema', () => {
  const prompt = buildObserverInspectPrompt({
    watchedTaskId: 'task_abc123',
    watchedTitle: 'rag-go 分块验证',
    watchedStatus: 'running',
    workspaceRoot: '/home/goodtime/project/rag-go',
    trigger: { kind: 'manual', label: '用户手动触发巡检', latestSeq: 15 },
  })
  assert.match(prompt, /blueprint\.inspect_session/)
  assert.match(prompt, /recent_tail/)
  assert.match(prompt, /task_abc123/)
  assert.match(prompt, /stop_and_continue/)
  assert.match(prompt, /严格只读/)

  const chatPrompt = buildObserverUserChatPrompt({
    watchedTaskId: 'task_abc123',
    watchedStatus: 'running',
    userText: '帮我确认要不要先停掉它',
    isFirstMessage: true,
  })
  assert.match(chatPrompt, /task_abc123/)
  assert.match(chatPrompt, /blueprint\.inspect_session/)
  assert.match(chatPrompt, /帮我确认要不要先停掉它/)
})

test('prefs, session map, and anti-recursion check persist cleanly', () => {
  const store = memoryStorage()
  assert.equal(loadObserverPrefs(store).enabled, false)

  saveObserverPrefs({ enabled: true, autoInspect: false, width: 450, reasoningEffort: 'high', fastMode: true }, store)
  const loaded = loadObserverPrefs(store)
  assert.equal(loaded.enabled, true)
  assert.equal(loaded.autoInspect, false)
  assert.equal(loaded.width, 450)
  assert.equal(loaded.reasoningEffort, 'high')
  assert.equal(loaded.fastMode, true)

  setObserverSessionId('task_main', 'task_obs_1', store)
  assert.equal(getObserverSessionId('task_main', store), 'task_obs_1')
  assert.equal(isObserverOwnTask({ task_id: 'task_obs_1', title: '任意标题' }, store), true)
  assert.equal(isObserverOwnTask({ task_id: 'task_other', title: `${OBSERVER_TITLE_PREFIX}测试` }, store), true)
  assert.equal(isObserverOwnTask({ task_id: 'task_main', title: '普通任务' }, store), false)
})

test('extractLatestAssistantMessage reads durable message.content blocks and sanitize hides internal meta & JSON fence', () => {
  const userWrapped = buildObserverUserChatPrompt({
    watchedTaskId: 'task_99a83',
    watchedStatus: 'running',
    userText: '观察一下当前会话任务，注意看他有没有跑偏',
    isFirstMessage: true,
  })
  const assistantRaw = [
    '截至当前快照，主会话出现轻微扩散。',
    '```observer-suggestion',
    '{"should_interject":true,"action":"interject","severity":"medium","title":"收敛到目标文件","reason":"正在读取无关文件","interject":"请直接回到 anydoc_image_vlm.go 完成修复"}',
    '```',
  ].join('\n')

  const events = [
    { seq: 1, time: 1, type: 'user/message', data: { message: { role: 'user', content: [{ type: 'text', text: userWrapped }] } } },
    { seq: 2, time: 2, type: 'tool/call', data: { name: 'blueprint__inspect_session' } },
    { seq: 3, time: 3, type: 'assistant/message', data: { message: { role: 'assistant', content: [{ type: 'reasoning', text: 'thinking' }, { type: 'text', text: assistantRaw }] } } },
  ]

  const latest = extractLatestAssistantMessage(events)
  assert.equal(latest?.seq, 3)
  const suggestion = parseObserverSuggestion(latest?.text ?? '', latest?.seq)
  assert.equal(suggestion?.shouldInterject, true)
  assert.equal(suggestion?.severity, 'warn')
  assert.equal(suggestion?.title, '收敛到目标文件')

  const display = sanitizeObserverEventsForDisplay(events)
  assert.equal(display[0].data.message.content[0].text, '观察一下当前会话任务，注意看他有没有跑偏')
  assert.equal(display[2].data.message.content[1].text, '截至当前快照，主会话出现轻微扩散。')

  // Legacy already-persisted wrapper without HTML delimiters is also stripped cleanly.
  const legacy = '[观察者上下文] 你是专属于主会话 `task_1` 的只读蓝图观察者。\n方便用户一键确认执行。\n\n帮我看看有没有跑偏'
  assert.equal(stripObserverUserPromptMeta(legacy), '帮我看看有没有跑偏')
  assert.equal(stripObserverSuggestionFence(assistantRaw), '截至当前快照，主会话出现轻微扩散。')
})
