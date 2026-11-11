import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/dsh/ui-primitives/markdown/flow-data.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions:{module:ts.ModuleKind.ESNext,target:ts.ScriptTarget.ES2024} })
const { parseFlowData, layoutFlowData } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('diagram preserves the supplied stages and branch relationships', () => {
  const data = parseFlowData(JSON.stringify({nodes:[{id:'pptx',label:'PPTX 文件'},{id:'parse',label:'解析'},{id:'render',label:'渲染'},{id:'edit',label:'编辑'}],edges:[{source:'pptx',target:'parse'},{source:'parse',target:'render'},{source:'parse',target:'edit'}]}))
  assert.ok(data)
  assert.deepEqual(data.edges.map(e=>[e.source,e.target]),[['pptx','parse'],['parse','render'],['parse','edit']])
  const p=layoutFlowData(data)
  assert.ok(p.get('pptx').y < p.get('parse').y)
  assert.equal(p.get('render').y,p.get('edit').y)
  assert.notEqual(p.get('render').x,p.get('edit').x)
})

test('invalid, broken or oversized diagrams fall back to original code', () => {
  for (const input of ['{', JSON.stringify({nodes:[{id:'a',label:'A'},{id:'a',label:'B'}],edges:[]}), JSON.stringify({nodes:[{id:'a',label:'A'},{id:'b',label:'B'}],edges:[{source:'a',target:'missing'}]}), JSON.stringify({nodes:[{id:'a',label:'A'},{id:'b',label:'B'}],edges:[{source:'a',target:'a'}]}), 'x'.repeat(20001)]) assert.equal(parseFlowData(input),null)
})

test('cyclic diagrams remain finite and keep all nodes', () => {
  const data=parseFlowData(JSON.stringify({nodes:[{id:'a',label:'A'},{id:'b',label:'B'}],edges:[{source:'a',target:'b'},{source:'b',target:'a'}]}))
  assert.equal(layoutFlowData(data).size,2)
})

// 2026-11-11: four wrapped labels fed one check box; the fixed 85px rows put
// the box under them and the edges took the long way round.
test('layers clear the tallest wrapped label above them', () => {
  const label = n => `agent-generation.yml: model_capabilities.qwen-local (${n} 行)`
  const data = parseFlowData(JSON.stringify({nodes:[{id:'a',label:label(20)},{id:'b',label:label(58)},{id:'c',label:'短'},{id:'e',label:'YAML 重新解析校验'}],edges:[{source:'a',target:'e'},{source:'b',target:'e'},{source:'c',target:'e'}]}))
  const p = layoutFlowData(data)
  for (const id of ['a','b','c']) assert.ok(p.get(id).y + p.get(id).height < p.get('e').y, `${id} ends above the check`)
  assert.ok(p.get('a').height > p.get('c').height, 'a wrapped label is taller')
  assert.equal(p.get('e').level - p.get('a').level, 1)
})
