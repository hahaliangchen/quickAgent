import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/visual.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { foldImageData } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('request view folds image bytes without modifying the real request body', () => {
  const body = { messages: [{ role: 'user', content: [{ type: 'image_url', image_url: { url: 'data:image/png;base64,AAAA' } }] }] }
  assert.match(foldImageData(body).messages[0].content[0].image_url.url, /图片数据已折叠/)
  assert.equal(body.messages[0].content[0].image_url.url, 'data:image/png;base64,AAAA')
})
