import test from 'node:test'
import assert from 'node:assert/strict'
import { extractClipboardFiles, isPasteShortcut, readFallbackClipboardImages } from '../src/clipboardPaste.ts'

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

test('isPasteShortcut matches Ctrl+V / Meta+V only without Shift or Alt', () => {
  assert.equal(isPasteShortcut({ key: 'v', ctrlKey: true, metaKey: false, shiftKey: false, altKey: false }), true)
  assert.equal(isPasteShortcut({ key: 'V', ctrlKey: false, metaKey: true, shiftKey: false, altKey: false }), true)
  assert.equal(isPasteShortcut({ key: 'v', ctrlKey: true, metaKey: false, shiftKey: true, altKey: false }), false)
  assert.equal(isPasteShortcut({ key: 'c', ctrlKey: true, metaKey: false, shiftKey: false, altKey: false }), false)
})
