import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'

async function transpile(relative) {
  const source = await readFile(new URL(relative, import.meta.url), 'utf8')
  return ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } }).outputText
}
const dataUrl = code => 'data:text/javascript;base64,' + Buffer.from(code).toString('base64')
const randomIdUrl = dataUrl(await transpile('../src/randomId.ts'))
const storeCode = (await transpile('../src/composerAttachments.ts')).replace("'./randomId.ts'", JSON.stringify(randomIdUrl))
const {
  attachmentsOf, addAttachments, removeAttachment, clearAttachments, forgetAttachments, ATTACHMENT_LIMIT,
} = await import(dataUrl(storeCode))

/** A stand-in for the browser's object URLs, so a released preview is provable. */
const alive = new Set()
let seq = 0
URL.createObjectURL = () => { const url = 'blob:preview-' + String(++seq); alive.add(url); return url }
URL.revokeObjectURL = url => { alive.delete(url) }

const picture = (name, type = 'image/png') => new File([new Uint8Array(8)], name, { type })
const names = key => attachmentsOf(key).map(item => item.file.name)

test('a picture pasted into one chat is still there after a look at another', () => {
  forgetAttachments()
  assert.equal(addAttachments('a', [picture('截图.png')]), '')
  assert.deepEqual(names('a'), ['截图.png'])
  assert.deepEqual(names('b'), [])
  const [held] = attachmentsOf('a')
  assert.match(held.preview, /^blob:preview-/)
  // The preview is still alive: nothing released it while the reader was away.
  assert.equal(alive.has(held.preview), true)
  assert.deepEqual(names('a'), ['截图.png'])
})

test('two chats keep their own pictures, and adding in one never moves the other', () => {
  forgetAttachments()
  addAttachments('a', [picture('甲.png')])
  addAttachments('b', [picture('乙.png'), picture('丙.jpg', 'image/jpeg')])
  assert.deepEqual(names('a'), ['甲.png'])
  assert.deepEqual(names('b'), ['乙.png', '丙.jpg'])
  assert.equal(alive.size, 3)
})

test('a send that ends after a switch forgets the sender, not the chat on screen', () => {
  forgetAttachments()
  addAttachments('a', [picture('发出去的.png')])
  addAttachments('b', [picture('正在写的.png')])
  const sentPreview = attachmentsOf('a')[0].preview
  // The reader switched to b while the upload ran; the send still names a.
  clearAttachments('a')
  assert.deepEqual(names('a'), [])
  assert.deepEqual(names('b'), ['正在写的.png'])
  assert.equal(alive.has(sentPreview), false)
})

test('removing one file releases only its preview', () => {
  forgetAttachments()
  addAttachments('a', [picture('留着.png'), picture('删掉.png')])
  const [kept, gone] = attachmentsOf('a')
  removeAttachment('a', gone.id)
  assert.deepEqual(names('a'), ['留着.png'])
  assert.equal(alive.has(gone.preview), false)
  assert.equal(alive.has(kept.preview), true)
})

test('a file a chat cannot take is reported instead of vanishing', () => {
  forgetAttachments()
  const complaint = addAttachments('a', [picture('notes.txt', 'text/plain'), new File([], '空的.png', { type: 'image/png' })], ['png', 'jpg'])
  assert.match(complaint, /不支持 notes\.txt/)
  assert.match(complaint, /空的\.png 必须非空/)
  assert.deepEqual(names('a'), [])
  assert.equal(alive.size, 0)
})

test('a full message drops its oldest picture and releases that preview', () => {
  forgetAttachments()
  const many = Array.from({ length: ATTACHMENT_LIMIT + 2 }, (_, index) => picture(String(index) + '.png'))
  addAttachments('a', many)
  assert.equal(attachmentsOf('a').length, ATTACHMENT_LIMIT)
  // The message keeps the twenty it took first; the two it could not take gave
  // their previews back instead of pinning them in memory.
  assert.deepEqual(names('a')[0], '0.png')
  assert.equal(names('a').length, ATTACHMENT_LIMIT)
  assert.equal(alive.size, ATTACHMENT_LIMIT)
})

test('clearing an empty chat changes nothing and releases nothing', () => {
  forgetAttachments()
  addAttachments('a', [picture('留住.png')])
  const preview = attachmentsOf('a')[0].preview
  clearAttachments('b')
  clearAttachments('nobody')
  assert.equal(alive.has(preview), true)
  assert.deepEqual(names('a'), ['留住.png'])
})
