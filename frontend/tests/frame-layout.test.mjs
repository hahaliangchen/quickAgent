import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/frameLayout.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { layoutFrameTree } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('calls sit one row below their caller, side by side in call order, the caller centred over them', () => {
  const placed = layoutFrameTree([
    { id: 'request', caller: 'n0' },
    { id: 'first', caller: 'request' },
    { id: 'second', caller: 'request' },
    { id: 'nested', caller: 'second' },
  ])
  const at = id => placed.get(id)
  assert.equal(at('request').y, 0)
  assert.equal(at('first').y, at('second').y)
  assert.ok(at('nested').y > at('second').y)
  assert.ok(at('first').x < at('second').x)
  assert.equal(at('request').x, (at('first').x + at('second').x) / 2)
  assert.equal(at('nested').x, at('second').x)
})

test('a broken caller chain still places every frame once', () => {
  const placed = layoutFrameTree([{ id: 'a', caller: 'b' }, { id: 'b', caller: 'a' }, { id: 'c', caller: null }])
  assert.equal(placed.size, 3)
  for (const point of placed.values()) assert.ok(Number.isFinite(point.x) && Number.isFinite(point.y))
})
