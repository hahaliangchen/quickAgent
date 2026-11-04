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
