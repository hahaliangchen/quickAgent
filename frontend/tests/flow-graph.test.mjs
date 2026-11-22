import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/flowGraph.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { buildFlow, flowTurns, layoutFlow, toneOf, FLOW_INDENT, FLOW_WIDTH, FLOW_HEIGHT } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

const step = (over = {}) => ({ kind: 'assistant', key: 'a' + (over.seqId ?? ''), text: '', reasoning: '', time: 0, streaming: false, turn: 1, step: 0, frame: 'n0', ...over })
const tool = (over = {}) => ({ kind: 'tool', key: 't' + (over.name ?? ''), callId: over.callId ?? 'c', name: over.name ?? 'read_file', arguments: '{}', time: 0, result: { text: 'ok', isError: false }, frame: 'n0', step: 0, ...over })
const program = (event, text = '') => ({ kind: 'program', key: 'p' + event, event, text, time: 0 })
const user = text => ({ kind: 'user', key: 'u', text, time: 0 })
const end = (reason = { kind: 'completed' }, durationMs = 65000) => ({ kind: 'turn-end', key: 'e', reason, durationMs })

test('a step and the tools it called are one card, not three', () => {
  const items = [
    user('修复流程图'),
    step({ key: 's1', step: 0, reasoning: '**先看布局**\n要读文件' }),
    tool({ name: 'read_file', callId: 'c1', step: 0 }),
    tool({ name: 'exec_command', callId: 'c2', step: 0 }),
    step({ key: 's2', step: 1, reasoning: '**改代码**' }),
    tool({ name: 'replace_range', callId: 'c3', step: 1 }),
    step({ key: 's3', step: 2, text: '已经修好了' }),
    end(),
  ]
  const { nodes, edges } = buildFlow(items, 1)
  // One card per step: the request, two steps of work, the reply and the ending.
  assert.deepEqual(nodes.map(n => n.kind), ['request', 'step', 'step', 'reply', 'end'])
  // The tools are inside the step that called them, in the order they ran.
  assert.deepEqual(nodes[1].items.map(item => item.kind), ['assistant', 'tool', 'tool'])
  assert.equal(nodes[1].work.tools, 2)
  assert.deepEqual(nodes[1].work.names, [{ name: 'read_file', tone: 'read' }, { name: 'exec_command', tone: 'run' }])
  assert.equal(nodes[1].work.label, '读取 · 运行')
  assert.deepEqual(nodes[2].items.map(item => item.kind), ['assistant', 'tool'])
  // The closing reply is drawn once: the last step becomes the reply, named by its answer.
  assert.equal(nodes.at(-2).kind, 'reply')
  assert.equal(nodes.at(-2).title, '已经修好了')
  assert.equal(nodes.at(-2).items[0], items[6])
  assert.equal(nodes.at(-1).meta, '1 分 5 s')
  const pairs = edges.map(edge => [nodes.findIndex(node => node.id === edge.source), nodes.findIndex(node => node.id === edge.target)])
  // One chain down the turn, with no branch for the folded tools.
  assert.deepEqual(pairs, [[0, 1], [1, 2], [2, 3], [3, 4]])
  assert.deepEqual(edges.map(edge => edge.branch), [false, false, false, false])
})

test('the step card counts what it did by family and says when something failed', () => {
  const items = [
    user('改两个文件再跑测试'),
    step({ key: 's1', reasoning: '**改文件**' }),
    tool({ name: 'write_file', callId: 'c1' }),
    tool({ name: 'replace_range', callId: 'c2' }),
    tool({ name: 'exec_command', callId: 'c3', result: { text: 'boom', isError: true } }),
    end(),
  ]
  const [stepNode] = buildFlow(items, 1).nodes.filter(node => node.kind === 'step')
  assert.equal(stepNode.work.tools, 3)
  assert.equal(stepNode.work.failed, 1)
  assert.deepEqual(stepNode.work.groups, [{ tone: 'edit', count: 2 }, { tone: 'run', count: 1 }])
  assert.equal(stepNode.work.label, '修改 ×2 · 运行')
  // A failed call inside the step fails the step, so the card shows it shut.
  assert.equal(stepNode.status, 'failed')
})

test('a program run folds into the step that started it', () => {
  const items = [user('写程序'), step({ key: 's1', reasoning: '**先写程序**', program: { status: 'started' } }), program('started', '程序开始运行'), end()]
  const { nodes } = buildFlow(items, 1)
  assert.deepEqual(nodes.map(n => n.kind), ['request', 'step', 'end'])
  const [stepNode] = nodes.filter(node => node.kind === 'step')
  assert.equal(stepNode.work.programs, 1)
  assert.deepEqual(stepNode.work.groups, [{ tone: 'run', count: 1 }])
  assert.deepEqual(stepNode.work.names, [{ name: '运行程序', tone: 'run' }])
})

test('a run of tools nobody\'s step claims is one card that counts them', () => {
  const items = [user('跑测试'), tool({ name: 'verify_test', callId: 'c1', step: undefined }), tool({ name: 'verify_test', callId: 'c2', step: undefined }), tool({ name: 'read_file', callId: 'c3', step: undefined }), end()]
  const { nodes } = buildFlow(items, 1)
  const steps = nodes.filter(node => node.kind === 'step')
  assert.equal(steps.length, 1)
  assert.equal(steps[0].title, '工具调用 ×3')
  assert.equal(steps[0].work.tools, 3)
  assert.equal(steps[0].meta, '')
})

test('a call the program made keeps a card of its own, with its steps below it', () => {
  const call = { kind: 'call', key: 'k1', callId: 'c1', name: 'generateApple', description: '生成一个苹果', module: false, line: 12, status: 'done', result: '已生成', time: 0, steps: [step({ key: 'cs1', frame: 'n1', step: 0, reasoning: '**画苹果**' }), tool({ name: 'write_file', callId: 'c9', frame: 'n1', step: 0 })] }
  const items = [user('生成水果'), step({ key: 's1', reasoning: '**写程序**' }), call, end()]
  const { nodes, edges } = buildFlow(items, 1)
  const outer = nodes.find(node => node.kind === 'call')
  assert.equal(outer.title, 'generateApple()')
  assert.equal(outer.meta, '虚函数 · 第 12 行')
  // The step does not carry the call twice: it is its own card below.
  const stepNode = nodes.find(node => node.kind === 'step')
  assert.deepEqual(stepNode.items.map(item => item.kind), ['assistant'])
  const inner = nodes.filter(node => node.parent === outer.id)
  assert.deepEqual(inner.map(node => node.kind), ['step'])
  // The call's own tool folds into the step that ran inside the call.
  assert.deepEqual(inner[0].items.map(item => item.kind), ['assistant', 'tool'])
  assert.ok(edges.some(edge => edge.source === stepNode.id && edge.target === outer.id && edge.branch))
  const placed = layoutFlow(nodes)
  assert.equal(placed.get(stepNode.id).x, 0)
  assert.equal(placed.get(outer.id).x, FLOW_INDENT)
  assert.equal(placed.get(inner[0].id).x, 2 * FLOW_INDENT)
})

test('cards never overlap and a card is as tall as the layout reserved', () => {
  const items = [user('长一点的轮次')]
  for (let index = 0; index < 8; index += 1) {
    items.push(step({ key: 's' + index, step: index, reasoning: '**第 ' + String(index) + ' 步**' }))
    items.push(tool({ name: 'read_file', callId: 'c' + index, step: index }))
  }
  items.push(end())
  const { nodes } = buildFlow(items, 1)
  const placed = layoutFlow(nodes)
  const rows = nodes.map(node => placed.get(node.id))
  for (let index = 1; index < rows.length; index += 1) assert.ok(rows[index].y >= rows[index - 1].y + rows[index - 1].height)
  for (const node of nodes) {
    assert.equal(placed.get(node.id).height, FLOW_HEIGHT[node.kind])
    assert.equal(placed.get(node.id).width, FLOW_WIDTH)
  }
})

test('deep chains stay one narrow column', () => {
  let parent = null
  const nodes = Array.from({ length: 12 }, (_, index) => { const node = { id: 'n' + index, parent, kind: 'call', title: '', summary: '', meta: '', status: 'done', turn: 1, items: [], work: { tools: 0, programs: 0, failed: 0, groups: [], names: [], label: '' } }; parent = node.id; return node })
  const xs = [...layoutFlow(nodes).values()].map(place => place.x)
  assert.equal(Math.max(...xs), 4 * FLOW_INDENT)
  assert.ok(Math.max(...xs) < FLOW_WIDTH)
})

test('a tool is coloured by what it does, not by how it is spelled elsewhere', () => {
  assert.equal(toneOf('read_file'), 'read')
  assert.equal(toneOf('exec_command'), 'run')
  assert.equal(toneOf('run_readonly_command'), 'run')
  assert.equal(toneOf('write_file'), 'edit')
  assert.equal(toneOf('replace_range'), 'edit')
  assert.equal(toneOf('grep'), 'search')
  assert.equal(toneOf('spawn_agent'), 'agent')
  assert.equal(toneOf('read_session_history'), 'context')
  assert.equal(toneOf('verify_test'), 'neutral')
})

test('each turn is offered with what it asked and how it ended', () => {
  const items = [
    user('第一个请求'), step({ key: 's1', text: '答一' }), end({ kind: 'completed' }, 1000),
    { kind: 'turn', key: 'turn2', turn: 2 },
    user('第二个请求'), step({ key: 's2', text: '答二', turn: 2 }), end({ kind: 'error', message: '模型不可用' }, 2000),
  ]
  const turns = flowTurns(items)
  assert.deepEqual(turns.map(entry => [entry.turn, entry.title, entry.status, entry.durationMs]), [[1, '第一个请求', 'done', 1000], [2, '第二个请求', 'failed', 2000]])
  // An unknown turn draws nothing rather than the wrong turn.
  assert.deepEqual(buildFlow(items, 9).nodes, [])
})

test('an unfinished turn keeps a running step and shows its waiting call', () => {
  const items = [user('还在跑'), step({ key: 's1', streaming: true, reasoning: '**准备**' }), tool({ name: 'exec_command', callId: 'c1', result: undefined })]
  const { nodes } = buildFlow(items, 1)
  assert.equal(nodes[0].status, 'running')
  assert.equal(nodes[1].status, 'running')
  // The waiting call is inside the running step: one card that is still working.
  assert.equal(nodes[1].work.tools, 1)
  assert.deepEqual(nodes[1].items.map(item => item.kind), ['assistant', 'tool'])
})

test('a card names its message, not the markup it was written in', () => {
  const items = [
    user('- 第一条要求\n### 真的标题\n把流程图合并一下'),
    step({ key: 's1', step: 0, text: '' }),
    step({
      key: 's2', step: 1,
      text: '### 已完成\n- 节点合并成一张卡\n- 抽屉改成 Markdown\n\n正文第一段：**要点**与\`code\` 都留在正文里。',
    }),
    end(),
  ]
  const { nodes } = buildFlow(items, 1)
  // A request that opens with a bullet is named by its first sentence.
  assert.equal(nodes[0].title, '第一条要求')
  // A reply that opens with a section heading is named by the paragraph under it.
  assert.equal(nodes.at(-2).title, '节点合并成一张卡')
  // The source itself is untouched: only the preview is flattened.
  assert.equal(nodes.at(-2).items[0].text.startsWith('### 已完成'), true)
})

test('a card preview keeps a link label and drops its destination', () => {
  const items = [
    user('看这里'),
    step({ key: 's1', step: 0, text: '改好了：[Composer.tsx](frontend/src/components/Composer.tsx) 里的 draftKey。' }),
    end(),
  ]
  const { nodes } = buildFlow(items, 1)
  assert.equal(nodes.at(-2).title, '改好了：Composer.tsx 里的 draftKey。')
})
