import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/composerDraft.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { draftFor, draftOf, keepDraft, forgetDrafts } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

/** A composer's box: the session it belongs to, and the text it holds. */
function box(key) {
  let session = key
  let held = { key: session, text: draftOf(session) }
  return {
    get text() { return draftFor(session, held).text },
    type(next) { keepDraft(session, next); held = { key: session, text: next } },
    /** The page swaps in another session's composer. */
    open(nextKey) { session = nextKey },
    /** The composer remounts: a fresh box, the same store. */
    remount() { held = { key: session, text: draftOf(session) } },
  }
}

test('a half-written message is still there after a look at another chat', () => {
  forgetDrafts()
  const draft = box('task-a')
  draft.type('请把流程图的节点合并一下')
  draft.open('task-b')
  assert.equal(draft.text, '')
  assert.equal(draftOf('task-a'), '请把流程图的节点合并一下')
  draft.open('task-a')
  assert.equal(draft.text, '请把流程图的节点合并一下')
})

test('two sessions keep their own text, and writing in one never moves the other', () => {
  forgetDrafts()
  const draft = box('a')
  draft.type('A 的草稿')
  draft.open('b')
  draft.type('B 的草稿')
  assert.equal(draftOf('a'), 'A 的草稿')
  assert.equal(draftOf('b'), 'B 的草稿')
  draft.open('a')
  assert.equal(draft.text, 'A 的草稿')
})

test('sending or emptying the box forgets the draft, so a new chat starts clean', () => {
  forgetDrafts()
  const draft = box('a')
  draft.type('发出去的那句')
  draft.type('')
  assert.equal(draftOf('a'), '')
  draft.open('b')
  draft.open('a')
  assert.equal(draft.text, '')
})

test('a composer that remounts finds its own text, not another session\'s', () => {
  forgetDrafts()
  const draft = box('a')
  draft.type('排到一半的队')
  draft.remount()
  assert.equal(draft.text, '排到一半的队')
  draft.open('c')
  draft.remount()
  assert.equal(draft.text, '')
})

test('a held box is not asked to read the store again', () => {
  forgetDrafts()
  keepDraft('a', '原文')
  const held = { key: 'a', text: '刚打的字' }
  // The box already belongs to this session: what it holds wins, so a keystroke
  // is never undone by a stale store.
  assert.equal(draftFor('a', held).text, '刚打的字')
  assert.deepEqual(draftFor('b', held), { key: 'b', text: '' })
})
