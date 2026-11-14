import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/frameLayout.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { layoutFrameTree, FRAME_WIDTH, FRAME_ROW, FRAME_INDENT } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('frames read top to bottom, each call indented under the caller that made it', () => {
  const placed = layoutFrameTree([
    { id: 'request', caller: 'n0' },
    { id: 'first', caller: 'request' },
    { id: 'second', caller: 'request' },
    { id: 'nested', caller: 'second' },
  ])
  const at = id => placed.get(id)
  assert.equal(at('request').y, 0)
  assert.equal(at('request').x, 0)
  // Calls of one frame stack downwards in call order instead of sitting side by side.
  assert.ok(at('first').y < at('second').y)
  assert.equal(at('first').x, at('second').x)
  assert.ok(at('first').x > at('request').x)
  assert.ok(at('nested').y > at('second').y)
  assert.equal(at('nested').x, at('second').x + FRAME_INDENT)
  // No two frames share a row: the picture is one column, not a horizontal band.
  assert.equal(new Set([...placed.values()].map(point => point.y)).size, placed.size)
  assert.equal(new Set([...placed.values()].map(point => point.y)).size, 4)
  assert.equal(at('second').y, 2 * FRAME_ROW)
})

test('frames that start their own run stack on the left edge, not side by side', () => {
  const placed = layoutFrameTree([{ id: 'a', caller: null }, { id: 'b', caller: undefined }])
  assert.equal(placed.get('a').x, 0)
  assert.equal(placed.get('b').x, 0)
  assert.equal(placed.get('a').y, 0)
  assert.equal(placed.get('b').y, FRAME_ROW)
})

test('a long call chain stays one narrow column', () => {
  const chain = Array.from({ length: 20 }, (_, index) => ({ id: `f${index}`, caller: index === 0 ? null : `f${index - 1}` }))
  const placed = layoutFrameTree(chain)
  assert.equal(placed.size, 20)
  const xs = [...placed.values()].map(point => point.x)
  // Indentation is capped, so the deepest frame is still narrower than one node.
  assert.equal(Math.max(...xs), 8 * FRAME_INDENT)
  assert.ok(Math.max(...xs) < FRAME_WIDTH)
  assert.equal(placed.get('f19').y, 19 * FRAME_ROW)
})

test('a broken caller chain still places every frame once', () => {
  const placed = layoutFrameTree([{ id: 'a', caller: 'b' }, { id: 'b', caller: 'a' }, { id: 'c', caller: null }])
  assert.equal(placed.size, 3)
  for (const point of placed.values()) assert.ok(Number.isFinite(point.x) && Number.isFinite(point.y))
  assert.equal(new Set([...placed.values()].map(point => point.y)).size, 3)
})
