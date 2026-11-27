import test from 'node:test'
import assert from 'node:assert/strict'
import {
  clipboardHasFileUriList,
  extractClipboardFiles,
  isPasteShortcut,
  readFallbackClipboardFiles,
  readFallbackClipboardImages,
} from '../src/clipboardPaste.ts'

test('extractClipboardFiles reads both clipboardData.files and clipboardData.items without duplicates', () => {
  const png = new File([new Uint8Array([137, 80, 78, 71])], 'shot.png', { type: 'image/png' })
  const unnamed = new File([new Uint8Array([1, 2, 3])], '', { type: 'image/png' })
  const data = {
    files: [png],
    items: [
      { kind: 'file', getAsFile: () => png },
      { kind: 'file', getAsFile: () => unnamed },
      { kind: 'string', getAsFile: () => null },
    ],
  }
  const files = extractClipboardFiles(data)
  assert.equal(files.length, 2)
  assert.equal(files[0].name, 'shot.png')
  assert.match(files[1].name, /^pasted-image-.*\.png$/)
  assert.equal(files[1].type, 'image/png')
})

test('readFallbackClipboardImages fetches local /agent/clipboard-image when status is 200', async () => {
  const bytes = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])
  const files = await readFallbackClipboardImages(async () => new Response(bytes, {
    status: 200,
    headers: { 'content-type': 'image/png' },
  }))
  assert.equal(files.length, 1)
  assert.equal(files[0].type, 'image/png')
  assert.equal(files[0].size, bytes.length)
})

test('readFallbackClipboardImages returns empty array on 204 No Content', async () => {
  const files = await readFallbackClipboardImages(async () => new Response(null, { status: 204 }))
  assert.deepEqual(files, [])
})

test('readFallbackClipboardFiles decodes copied local files like .pptx from /agent/clipboard-files', async () => {
  const b64 = Buffer.from('PK\x03\x04pptx-demo').toString('base64')
  const files = await readFallbackClipboardFiles(async (url) => {
    assert.equal(url, '/agent/clipboard-files')
    return new Response(JSON.stringify({
      files: [{ name: '汇报.pptx', mime: 'application/vnd.openxmlformats-officedocument.presentationml.presentation', base64: b64 }],
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })
  })
  assert.equal(files.length, 1)
  assert.equal(files[0].name, '汇报.pptx')
  assert.equal(files[0].type, 'application/vnd.openxmlformats-officedocument.presentationml.presentation')
  assert.equal(files[0].size, Buffer.from('PK\x03\x04pptx-demo').length)
})

test('clipboardHasFileUriList detects text/uri-list and gnome/kde copied file markers', () => {
  assert.equal(clipboardHasFileUriList({ types: ['text/uri-list', 'text/plain'], getData: () => '' }), true)
  assert.equal(clipboardHasFileUriList({ types: ['text/plain'], getData: () => '' }), false)
})

test('isPasteShortcut matches Ctrl+V / Meta+V only without Shift or Alt', () => {
  assert.equal(isPasteShortcut({ key: 'v', ctrlKey: true, metaKey: false, shiftKey: false, altKey: false }), true)
  assert.equal(isPasteShortcut({ key: 'V', ctrlKey: false, metaKey: true, shiftKey: false, altKey: false }), true)
  assert.equal(isPasteShortcut({ key: 'v', ctrlKey: true, metaKey: false, shiftKey: true, altKey: false }), false)
  assert.equal(isPasteShortcut({ key: 'c', ctrlKey: true, metaKey: false, shiftKey: false, altKey: false }), false)
})
