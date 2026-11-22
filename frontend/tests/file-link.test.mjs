import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'

const source = await readFile(new URL('../src/dsh/ui-primitives/markdown/file-link.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { parseFileLink } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('workspace-relative destinations keep their path and optional line', () => {
  assert.deepEqual(parseFileLink('frontend/src/components/CodeBlock.tsx'), { path: 'frontend/src/components/CodeBlock.tsx' })
  assert.deepEqual(parseFileLink('frontend/src/components/ToolRow.tsx#L203-L214'), { path: 'frontend/src/components/ToolRow.tsx', line: 203 })
  assert.deepEqual(parseFileLink('src/a.rs#L7'), { path: 'src/a.rs', line: 7 })
  // Percent-encoded spaces decode to the real path; the fragment still parses.
  assert.deepEqual(parseFileLink('docs/My%20Notes.md#L3'), { path: 'docs/My Notes.md', line: 3 })
})

test('file:/// destinations resolve to the same local path as a bare path', () => {
  assert.deepEqual(parseFileLink('file:///workspace/src/a.rs'), { path: '/workspace/src/a.rs' })
  assert.deepEqual(parseFileLink('file:///workspace/src/a.rs#L10-L20'), { path: '/workspace/src/a.rs', line: 10 })
  assert.deepEqual(parseFileLink('file:///C:/drop/a.ts'), { path: 'C:/drop/a.ts' })
  assert.deepEqual(parseFileLink('file://localhost/drop/a.ts#L2-L4'), { path: '/drop/a.ts', line: 2 })
  // A remote file host names no local file.
  assert.equal(parseFileLink('file://server/share/a.ts'), undefined)
  assert.equal(parseFileLink('file://'), undefined)
})

test('non-file URLs, fragments, queries and malformed escapes are refused', () => {
  assert.equal(parseFileLink('https://example.com/a.ts#L10-L20'), undefined)
  assert.equal(parseFileLink('mailto:a@b.c'), undefined)
  assert.equal(parseFileLink('#L10'), undefined)
  assert.equal(parseFileLink(''), undefined)
  assert.equal(parseFileLink('//host/share/a.ts'), undefined)
  assert.equal(parseFileLink('a.ts?raw=1'), undefined)
  assert.equal(parseFileLink('a%zz.ts'), undefined)
  assert.equal(parseFileLink('src/a.rs#L20-L10'), undefined, 'reversed range')
  assert.equal(parseFileLink('src/a.rs#L0'), undefined, 'lines are 1-based')
  assert.equal(parseFileLink('src/a.rs#nope'), undefined)
})
