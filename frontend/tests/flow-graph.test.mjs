import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/flowGraph.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { buildFlow, flowTurns, layoutFlow, FLOW_INDENT, FLOW_WIDTH } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

const step = (over = {}) => ({ kind: 'assistant', key: 'a' + (over.seqId ?? ''), text: '', reasoning: '', time: 0, streaming: false, turn: 1, step: 0, frame: 'n0', ...over })
const tool = (over = {}) => ({ kind: 'tool', key: 't' + (over.name ?? ''), callId: over.callId ?? 'c', name: over.name ?? 'read_file', arguments: '{}', time: 0, result: { text: 'ok', isError: false }, frame: 'n0', step: 0, ...over })
const user = text => ({ kind: 'user', key: 'u', text, time: 0 })
const end = (reason = { kind: 'completed' }, durationMs = 65000) => ({ kind: 'turn-end', key: 'e', reason, durationMs })

test('a turn reads as request, steps, its tools, the reply and the ending', () => {
  const items = [
    user('修复流程图'),
    step({ key: 's1', step: 0, reasoning: '**先看布局**\n要读文件' }),
    tool({ name: 'read_file', callId: 'c1', step: 0 }),
    step({ key: 's2', step: 1, reasoning: '**改代码**' }),
    step({ key: 's3', step: 2, text: '已经修好了', frame: 'n0' }),
    end(),
  ]
  const { nodes, edges } = buildFlow(items, 1)
  assert.deepEqual(nodes.map(n => n.kind), ['request', 'step', 'tool', 'step', 'reply', 'end'])
  // The tool hangs under the step whose AI called it, not on the turn's own chain.
  const read = nodes.find(n => n.kind === 'tool')
  assert.equal(read.parent, nodes[1].id)
  // The closing reply is drawn once: the last step becomes the reply.
  assert.equal(nodes.at(-2).summary, '已经修好了')
  assert.equal(nodes.at(-1).meta, '1 分 5 s')
  const pairs = edges.map(e => [nodes.findIndex(n => n.id === e.source), nodes.findIndex(n => n.id === e.target)])
  // One chain down the turn: each node hangs under the one before it, tools included.
  assert.deepEqual(pairs, [[0, 1], [1, 2], [2, 3], [3, 4], [4, 5]])
  assert.deepEqual(edges.map(e => e.branch), [false, true, false, false, false])
})

test('two tools of one step stack in a column, the second after the first', () => {
  const items = [
    user('跑两条命令'),
    step({ key: 's1', reasoning: '**先跑一次**' }),
    tool({ name: 'exec_command', callId: 'c1' }),
    tool({ name: 'read_file', callId: 'c2' }),
    end(),
  ]
  const { nodes, edges } = buildFlow(items, 1)
  const [stepNode] = nodes.filter(n => n.kind === 'step')
  const tools = nodes.filter(n => n.kind === 'tool')
  assert.deepEqual(tools.map(one => one.parent), [stepNode.id, stepNode.id])
  // The second call follows the first: it never crosses back over the card above.
  assert.ok(edges.some(e => e.source === tools[0].id && e.target === tools[1].id && e.branch))
  assert.equal(edges.some(e => e.source === stepNode.id && e.target === tools[1].id), false)
  // Both sit at the same indent, one below the other.
  const placed = layoutFlow(nodes)
  assert.equal(placed.get(tools[0].id).x, placed.get(tools[1].id).x)
  assert.ok(placed.get(tools[1].id).y > placed.get(tools[0].id).y)
})

test('a run of tools nobody\'s step claims becomes one node that counts them', () => {
  const items = [user('跑测试'), tool({ name: 'verify_test', callId: 'c1', step: undefined }), tool({ name: 'verify_test', callId: 'c2', step: undefined }), tool({ name: 'read_file', callId: 'c3', step: undefined }), end()]
  const { nodes } = buildFlow(items, 1)
  const tools = nodes.filter(n => n.kind === 'tool')
  assert.equal(tools.length, 1)
  assert.equal(tools[0].title, '工具调用 ×3')
  assert.equal(tools[0].items.length, 3)
})

test('a call the program made nests its own steps under it', () => {
  const call = { kind: 'call', key: 'k1', callId: 'c1', name: 'generateApple', description: '生成一个苹果', module: false, line: 12, status: 'done', result: '已生成', time: 0, steps: [step({ key: 'cs1', frame: 'n1', step: 0, reasoning: '**画苹果**' }), tool({ name: 'write_file', callId: 'c9', frame: 'n1', step: 0 })] }
  const items = [user('生成水果'), step({ key: 's1', reasoning: '**写程序**' }), call, end()]
  const { nodes, edges } = buildFlow(items, 1)
  const outer = nodes.find(n => n.kind === 'call')
  assert.equal(outer.meta, '虚函数 · 第 12 行')
  const inner = nodes.filter(n => n.parent === outer.id)
  assert.deepEqual(inner.map(n => n.kind), ['step'])
  // The call's own tool hangs under the call's step, not under the call.
  const nestedTool = nodes.find(n => n.kind === 'tool')
  assert.equal(nestedTool.parent, inner[0].id)
  assert.ok(edges.some(e => e.source === outer.id && e.target === inner[0].id && e.branch))
  // Nested nodes are indented, and every node keeps its own row.
  const placed = layoutFlow(nodes)
  assert.equal(placed.get(outer.id).x, 0)
  assert.equal(placed.get(inner[0].id).x, FLOW_INDENT)
  assert.equal(placed.get(nestedTool.id).x, 2 * FLOW_INDENT)
  // Rows do not overlap: each node starts below the previous one.
  const rows = nodes.map(n => placed.get(n.id))
  for (let i = 1; i < rows.length; i += 1) assert.ok(rows[i].y >= rows[i - 1].y + rows[i - 1].height)
})

test('deep chains stay one narrow column', () => {
  let parent = null
  const nodes = Array.from({ length: 12 }, (_, index) => { const node = { id: 'n' + index, parent, kind: 'tool', title: '', summary: '', meta: '', status: 'done', turn: 1, items: [] }; parent = node.id; return node })
  const xs = [...layoutFlow(nodes).values()].map(p => p.x)
  assert.equal(Math.max(...xs), 4 * FLOW_INDENT)
  assert.ok(Math.max(...xs) < FLOW_WIDTH)
})

test('each turn is offered with what it asked and how it ended', () => {
  const items = [
    user('第一个请求'), step({ key: 's1', text: '答一' }), end({ kind: 'completed' }, 1000),
    { kind: 'turn', key: 'turn2', turn: 2 },
    user('第二个请求'), step({ key: 's2', text: '答二', turn: 2 }), end({ kind: 'error', message: '模型不可用' }, 2000),
  ]
  const turns = flowTurns(items)
  assert.deepEqual(turns.map(t => [t.turn, t.title, t.status, t.durationMs]), [[1, '第一个请求', 'done', 1000], [2, '第二个请求', 'failed', 2000]])
  // An unknown turn draws nothing rather than the wrong turn.
  assert.deepEqual(buildFlow(items, 9).nodes, [])
})

test('an unfinished turn keeps a running step and its waiting tool', () => {
  const items = [user('还在跑'), step({ key: 's1', streaming: true, reasoning: '**准备**' }), tool({ name: 'exec_command', callId: 'c1', result: undefined })]
  const { nodes } = buildFlow(items, 1)
  assert.equal(nodes[0].status, 'running')
  assert.equal(nodes[1].status, 'running')
  assert.equal(nodes[2].status, 'running')
})
