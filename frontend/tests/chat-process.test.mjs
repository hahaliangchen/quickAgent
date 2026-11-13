import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/model.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { chatItems, splitTurnItems, processEntries, reasoningHeadline, flattenItems, failedToolNames, runningCallState } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

let seq = 0
const event = (type, data) => ({ seq: ++seq, time: 1000 + seq, type, data: { turn: 1, ...data } })
const frame = (type, node_id, status, metadata, result) => event(type, { node_id, task_node_id: node_id, status, metadata, ...(result ? { result } : {}) })
const reply = (frameId, step, content, reasoning, type = 'assistant/message') =>
  event(type, { step, task_node_id: frameId, message: { role: 'assistant', content, reasoning_content: reasoning } })
const call = (frameId, callId, name, args, extra = {}) =>
  event('tool/call', { task_node_id: frameId, callId, name, arguments: JSON.stringify(args), ...extra })
const result = (frameId, callId, text, isError = false) =>
  event('tool/result', { task_node_id: frameId, message: { role: 'tool', toolCallId: callId, isError, content: [{ type: 'text', text }] } })
const program = '```js\nimport * as io from "host:io";\nconst cli = await io.read({path:"cli.js"});\nawait implementWatch(cli);\n\n/** @virtual */\nasync function implementWatch(cli) {\n  // Add --watch to the CLI.\n}\n```'

test('tool argument progress grows without exposing arguments and closes when the message arrives', () => {
  seq = 0
  const events = [event('assistant/delta', { step: 1, task_node_id: 'main', tool_progress: [{ name: 'write_file', argument_bytes: 10 }] }),
    event('assistant/delta', { step: 1, task_node_id: 'main', tool_progress: [{ name: 'write_file', argument_bytes: 120 }] })]
  const live = chatItems(events)
  assert.equal(live.length, 1)
  assert.deepEqual(live[0].toolProgress, [{ name: 'write_file', argumentBytes: 120 }])
  assert.equal(live[0].streaming, true)
  const done = chatItems([...events, reply('main', 1, null, '')])
  assert.equal(done[0].streaming, false)
})

test('completed turns can report tool failures without claiming they were not recovered', () => {
  seq = 0
  const items = chatItems([call('main', 'c1', 'browser', {}), result('main', 'c1', 'unavailable', true),
    call('main', 'c2', 'list_dir', {}), result('main', 'c2', '{}')])
  assert.deepEqual(failedToolNames(items), ['browser'])
})

const shellRows = events => flattenItems(chatItems(events)).filter(item => item.kind === 'tool')

test('shell end settles earlier running rows without replaying or mutating their output', () => {
  seq = 0
  const launch = { session_id: 's1', running: true, exit_code: null, stdout: 'initial output', stderr: '', truncated: true, output_asset: { asset_id: 'initial' } }
  const events = [call('module', 'exec', 'exec_command', { cmd: 'npm run dev' }),
    result('module', 'exec', JSON.stringify(launch)),
    call('module', 'poll', 'write_stdin', { session_id: 's1', chars: '' }),
    result('module', 'poll', JSON.stringify({ session_id: 's1', running: true, stdout: 'poll output', stderr: 'poll diagnostic' })),
    event('shell/end', { session_id: 's1', exit_code: 9, termination_reason: 'process_exit', output_asset: { asset_id: 'final' }, errors: ['capture diagnostic'], stdout: 'must not replay' })]
  events[1].data.meta = { durationMs: 12 }
  const original = JSON.stringify(events)
  const rows = shellRows(events)
  assert.equal(rows.length, 2)
  const [exec, poll] = rows.map(row => JSON.parse(row.result.text))
  for (const value of [exec, poll]) {
    assert.equal(value.running, false)
    assert.equal(value.exit_code, 9)
    assert.equal(value.termination_reason, 'process_exit')
    assert.deepEqual(value.output_asset, { asset_id: 'final' })
    assert.deepEqual(value.errors, ['capture diagnostic'])
  }
  assert.equal(exec.stdout, 'initial output')
  assert.equal(exec.stderr, '')
  assert.equal(exec.truncated, true, 'the launch row still holds a truncated capture')
  assert.equal(poll.stdout, 'poll output')
  assert.equal(poll.stderr, 'poll diagnostic')
  assert.equal(rows[0].result.durationMs, 12)
  assert.equal(rows[0].result.time, events[1].time)
  assert.equal(JSON.stringify(events), original, 'the persisted log remains unchanged')
  assert.deepEqual(shellRows(JSON.parse(original).reverse()), rows, 'history reload sorts by seq before projecting final state')
})

test('a completed poll or kill settles the launching row without a shell end event', () => {
  for (const name of ['write_stdin', 'kill_session']) {
    for (const session_id of ['s1', null, undefined]) {
      seq = 0
      const events = [call('module', 'exec', 'exec_command', { cmd: 'server' }),
        result('module', 'exec', JSON.stringify({ session_id: 's1', running: true, stdout: 'launch' })),
        call('other-frame', 'finish', name, { session_id: 's1', chars: '' }),
        result('other-frame', 'finish', JSON.stringify({ session_id, running: false, exit_code: 0, stdout: 'final output', stderr: 'final diagnostic', output_asset: 'final-asset' }))]
      const rows = shellRows(events)
      const first = JSON.parse(rows[0].result.text)
      assert.equal(first.running, false, `${name}: ${session_id}`)
      assert.equal(first.exit_code, 0)
      assert.equal(first.output_asset, 'final-asset')
      assert.equal(first.stdout, 'launch')
      assert.equal(first.stderr, undefined)
      assert.equal(JSON.parse(rows[1].result.text).stdout, 'final output')
    }
  }
})

test('latest terminal metadata wins while later running snapshots cannot resurrect a session', () => {
  seq = 0
  const events = [call('module', 'exec', 'exec_command', { cmd: 'server' }),
    result('module', 'exec', JSON.stringify({ session_id: 's1', running: true, stdout: 'launch' })),
    event('shell/end', { session_id: 's1', exit_code: 1, errors: ['first'] }),
    call('module', 'finish', 'kill_session', { session_id: 's1' }),
    result('module', 'finish', JSON.stringify({ session_id: 's1', running: false, exit_code: 2, stdout: 'kill output' })),
    event('shell/end', { session_id: 's1', exit_code: null, termination_reason: 'requested_stop', output_asset: 'last-asset', errors: [] }),
    call('module', 'late', 'write_stdin', { session_id: 's1', chars: '' }),
    result('module', 'late', JSON.stringify({ session_id: 's1', running: true, stdout: 'late capture' }))]
  for (const row of shellRows(events)) {
    const value = JSON.parse(row.result.text)
    assert.equal(value.running, false)
    assert.equal(value.exit_code, null, 'a killed session does not invent an exit code')
    assert.equal(value.termination_reason, 'requested_stop')
    assert.equal(value.output_asset, 'last-asset')
    assert.deepEqual(value.errors, [])
  }
})

test('shell completion stays scoped to its session and shell tool calls', () => {
  seq = 0
  const live = JSON.stringify({ session_id: 's2', running: true, stdout: 'keep running' })
  const unrelated = JSON.stringify({ session_id: 's1', running: true, stdout: 'unrelated tool' })
  const events = [call('module', 'one', 'exec_command', { cmd: 'first' }),
    result('module', 'one', '{"session_id":"s1","running":true}'),
    call('module', 'two', 'exec_command', { cmd: 'second' }), result('module', 'two', live),
    call('module', 'read', 'read_file', { path: 'data.json' }), result('module', 'read', unrelated),
    call('module', 'error', 'write_stdin', { session_id: 's1' }), result('module', 'error', 'session scope denied', true),
    call('module', 'pending', 'exec_command', { cmd: 'pending' }),
    event('shell/end', { session_id: 's1', exit_code: 0 })]
  const rows = shellRows(events)
  assert.equal(JSON.parse(rows[0].result.text).running, false)
  assert.equal(rows[1].result.text, live)
  assert.equal(rows[2].result.text, unrelated)
  assert.equal(rows[3].result.text, 'session scope denied')
  assert.equal(rows[3].result.isError, true)
  assert.equal(rows[4].result, undefined)
})

test('a turn ending alone does not settle a live shell', () => {
  seq = 0
  const raw = '{"session_id":"s1","running":true,"exit_code":null,"stdout":"still alive"}'
  const events = [call('module', 'exec', 'exec_command', { cmd: 'server' }), result('module', 'exec', raw),
    event('turn/end', { reason: { kind: 'completed' } })]
  assert.equal(shellRows(events)[0].result.text, raw)
})

test('numeric shell ids and structured call arguments can recover a finished session', () => {
  seq = 0
  const finish = call('module', 'finish', 'write_stdin', {})
  finish.data.arguments = { session_id: 42, chars: '' }
  const events = [call('module', 'exec', 'exec_command', { cmd: 'server' }),
    result('module', 'exec', '{"session_id":42,"running":true,"stdout":"initial"}'), finish,
    result('module', 'finish', '{"session_id":null,"running":false,"exit_code":0,"stdout":"finished"}'),
    event('shell/end', { session_id: '', exit_code: 9 })]
  assert.equal(JSON.parse(shellRows(events)[0].result.text).running, false)
  assert.equal(JSON.parse(shellRows(events)[0].result.text).exit_code, 0)
})

test('a later opaque tool error is not replaced by an older shell snapshot', () => {
  seq = 0
  const events = [call('module', 'poll', 'write_stdin', { session_id: 's1' }),
    result('module', 'poll', '{"session_id":"s1","running":true,"stdout":"old"}'),
    result('module', 'poll', 'latest error', true), event('shell/end', { session_id: 's1', exit_code: 0 })]
  const row = shellRows(events)[0]
  assert.equal(row.result.text, 'latest error')
  assert.equal(row.result.isError, true)
})

/** The shape of a real turn: the main AI looks around, writes a program, the program reads a file and calls a virtual function. */
function turn() {
  seq = 0
  return [
    event('turn/start', {}),
    event('user/message', { message: { content: '加上 --watch' } }),
    frame('task/node_planned', 'main', 'pending', { role: 'main', turn: 1 }),
    frame('task/node_started', 'main', 'running', { role: 'main', turn: 1 }),
    reply('main', 1, null, '**Inspecting project files**'),
    call('main', 'c1', 'list_dir', { path: '.' }, { step: 1 }),
    result('main', 'c1', '{"entries":[]}'),
    reply('main', 2, program, '**Writing the program**'),
    event('program/start', { task_node_id: 'main', function: null, source: program }),
    call('main', 'p1', 'read_file', { path: 'cli.js' }, { step: 3, program_location: { line: 3, column: 13 } }),
    result('main', 'p1', '{"content":"…"}'),
    call('main', 'program-abc', 'create_task', { goal: 'implementWatch', briefing: 'Add --watch to the CLI.',
      program_task: { name: 'implementWatch', description: 'Add --watch to the CLI.\nReturns null.', role: 'leaf' }, call_site: { line: 4, column: 7 } }, { step: 3 }),
    frame('task/node_planned', 'child', 'pending', { role: 'temp', caller: { task: 'main', call_id: 'program-abc' }, call_site: { line: 4, column: 7 } }),
    frame('task/node_started', 'child', 'running', { role: 'temp', caller: { task: 'main', call_id: 'program-abc' } }),
    event('assistant/delta', { step: 1, task_node_id: 'child', reasoning: '**Reading the CLI**' }),
    reply('child', 1, null, '**Reading the CLI**'),
    call('child', 'k1', 'read_file', { path: 'cli.js' }, { step: 1 }),
    result('child', 'k1', '{"content":"…"}'),
    reply('child', 2, 'null', '**Done editing**', 'assistant/draft'),
    frame('task/node_returned', 'child', 'done', { role: 'temp', caller: { task: 'main', call_id: 'program-abc' } }, { result: 'null', assets: [] }),
    result('main', 'program-abc', JSON.stringify({ task: 'implementWatch', node_id: 'child', result: 'null', assets: [] })),
    reply('main', 4, '程序已完成。', ''),
    event('turn/end', { reason: { kind: 'completed' } }),
  ]
}
const shape = item => item.kind === 'assistant' ? `assistant:${item.text === '' ? item.reasoning : item.text.slice(0, 8)}`
  : item.kind === 'tool' ? `tool:${item.name}${item.line ? `@${item.line}` : ''}`
    : item.kind === 'call' ? `call:${item.name}@${item.line}:${item.status}` : item.kind

test('a turn shows what the AI is doing, the program it wrote, and each call with its own steps inside', () => {
  const items = chatItems(JSON.parse(JSON.stringify(turn()))).filter(item => !['user', 'turn', 'turn-end'].includes(item.kind))
  assert.deepEqual(items.map(shape), [
    'assistant:**Inspecting project files**', 'tool:list_dir', 'assistant:```js\nim', 'tool:read_file@3',
    'call:implementWatch@4:done', 'assistant:程序已完成。',
  ])
  assert.equal(items[2].program.status, 'started', 'the reply knows its program ran')
  const watch = items[4]
  assert.equal(watch.description, 'Add --watch to the CLI.\nReturns null.')
  assert.equal(watch.child, 'child')
  assert.equal(watch.result, 'null')
  // The streamed thought and the final reply are one step; the closing reply shows its thought, its value is the call's.
  assert.deepEqual(watch.steps.map(shape), ['assistant:**Reading the CLI**', 'tool:read_file', 'assistant:**Done editing**'])
  assert.equal(watch.steps[0].streaming, false)
  assert.equal(flattenItems(items).filter(item => item.kind === 'tool').length, 3)
  const { process, conclusion } = splitTurnItems(items)
  assert.equal(conclusion.text, '程序已完成。')
  assert.deepEqual(processEntries(process).map(entry => entry.kind === 'step' ? `step:${entry.tools.length}` : entry.kind),
    ['step:1', 'step:0', 'tools', 'item'], 'a program call is not part of the reply that wrote the program')
})

test('a running call reports failure from what its function gave up with', () => {
  const events = turn().slice(0, 18)
  const running = chatItems(events).find(item => item.kind === 'call')
  assert.equal(running.status, 'running')
  assert.equal(running.result, undefined)
  const failed = [...events,
    frame('task/node_returned', 'child', 'failed', { role: 'temp', caller: { task: 'main', call_id: 'program-abc' } }, { result: '{"$task_error":{"message":"cli.js is missing"}}', assets: [] }),
    event('program/error', { task_node_id: 'main', error: { message: 'cli.js is missing', location: { line: 4, column: 7 } } })]
  const items = chatItems(failed)
  const call = items.find(item => item.kind === 'call')
  assert.equal(call.status, 'failed')
  assert.equal(call.error, 'cli.js is missing')
  assert.deepEqual(items.filter(item => item.kind === 'program').map(item => [item.event, item.line, item.text]), [['failed', 4, 'cli.js is missing']])
})

test('a program that does not compile marks its reply, and a reply without a frame id still ends its stream', () => {
  seq = 0
  const items = chatItems([
    reply('main', 1, program, ''),
    event('program/source_invalid', { task_node_id: 'main', diagnostic: { error: 'bad', diagnostics: [{ line: 2, column: 1, message: 'TS_PROGRAM 2:1: bad import' }] } }),
    event('assistant/delta', { turn: 2, step: 1, text: 'Hel' }),
    event('assistant/message', { turn: 2, step: 1, task_node_id: 'main', message: { content: 'Hello' } }),
  ])
  assert.deepEqual(items[0].program, { status: 'invalid', error: 'TS_PROGRAM 2:1: bad import', line: 2 })
  assert.equal(items.length, 2)
  assert.equal(items[1].text, 'Hello')
  assert.equal(items[1].streaming, false)
})

test('a call made inside a module nests under the module call', () => {
  seq = 0
  const items = chatItems([
    frame('task/node_started', 'main', 'running', { role: 'main' }),
    call('main', 'program-m', 'create_task', { goal: 'buildAll', program_task: { name: 'buildAll', description: 'Build.', module: true, role: 'implement' }, call_site: { line: 2, column: 1 } }),
    frame('task/node_started', 'mod', 'running', { role: 'temp', caller: { task: 'main', call_id: 'program-m' } }),
    event('module/loaded', { task_node_id: 'mod', function: 'buildAll', contract: 'k', version: 3 }),
    call('mod', 'program-i', 'create_task', { goal: 'judge', program_task: { name: 'judge', description: 'Judge.', role: 'leaf' }, call_site: { line: 9, column: 3 } }),
    frame('task/node_started', 'inner', 'running', { role: 'temp', caller: { task: 'mod', call_id: 'program-i' } }),
    reply('inner', 1, null, '**Comparing outputs**'),
  ])
  assert.deepEqual(items.map(shape), ['call:buildAll@2:running'])
  assert.equal(items[0].module, true)
  assert.deepEqual(items[0].steps.map(item => item.kind === 'program' ? `${item.event}:${item.text}` : shape(item)), ['module_loaded:第 3 版', 'call:judge@9:running'])
  assert.deepEqual(items[0].steps[1].steps.map(shape), ['assistant:**Comparing outputs**'])
})

test('the headline of a reasoning summary is its latest thought', () => {
  assert.equal(reasoningHeadline('**Designing early export mapping**\n\n**Building early export map**\n\n**Preparing export tests**'), 'Preparing export tests')
  assert.equal(reasoningHeadline('**Checking tests**\n\nI will run npm test first.'), 'Checking tests')
  assert.equal(reasoningHeadline('First line of a plain thought\nmore detail'), 'First line of a plain thought')
  assert.equal(reasoningHeadline(''), '')
  // Chat Completions through a converting proxy delivers summary parts back to back.
  assert.equal(reasoningHeadline('**Checking project state****Checking file access details****Reviewing program guidance**'), 'Reviewing program guidance')
  seq = 0
  const [item] = chatItems([reply('main', 1, null, '**Defining virtual translator****Checking file access contracts**')])
  assert.equal(item.reasoning, '**Defining virtual translator**\n\n**Checking file access contracts**')
})

test('a running call names what it waits on: a command with its elapsed time, code, or an AI', () => {
  const tool = (name, args, time, result) => ({ kind: 'tool', key: name + time, callId: name + time, name, arguments: JSON.stringify(args), time,
    result: result === undefined ? undefined : { text: JSON.stringify(result), isError: false, durationMs: undefined, childTaskId: undefined, time } })
  const call = (steps, module = true) => ({ kind: 'call', key: 'c', callId: 'c', name: 'shell', description: '', module, status: 'running', time: 0, steps })
  const started = tool('exec_command', { cmd: 'cargo test' }, 10_000, { session_id: 's1', running: true })
  const polling = tool('write_stdin', { session_id: 's1', chars: '' }, 40_000)
  assert.equal(runningCallState(call([started, polling]), 140_000), '等待命令 · 已运行 2分10秒', 'timed from the command, not the latest poll')
  assert.equal(runningCallState(call([tool('exec_command', { cmd: 'ls' }, 5_000)]), 8_000), '等待命令 · 已运行 3秒')
  assert.equal(runningCallState(call([tool('read_file', { path: 'a' }, 5_000)]), 8_000), '等待工具')
  assert.equal(runningCallState(call([started]), 20_000), '运行中', 'module code between steps')
  assert.equal(runningCallState(call([started], false), 20_000), 'AI 处理中', 'a virtual helper thinks between tools')
  assert.equal(runningCallState(call([]), 0), '运行中')
})
