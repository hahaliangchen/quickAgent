import test from 'node:test'
import assert from 'node:assert/strict'
import {
  basenameOfPath,
  formatRelativeOpenedAt,
  inferOfficeSlotId,
  loadRecentOfficeFiles,
  officeLocalFileUrl,
  parentDirOfPath,
  removeRecentOfficeFile,
  saveRecentOfficeFiles,
  upsertRecentOfficeFile,
} from '../src/officeRecentFiles.ts'

test('infers Office Studio slot id for supported extensions', () => {
  assert.equal(inferOfficeSlotId('SKILL.md'), 'markdown')
  assert.equal(inferOfficeSlotId('notes.MARKDOWN'), 'markdown')
  assert.equal(inferOfficeSlotId('spec.docx'), 'word')
  assert.equal(inferOfficeSlotId('budget.xlsx'), 'excel')
  assert.equal(inferOfficeSlotId('deck.pptx'), 'ppt')
  assert.equal(inferOfficeSlotId('manual.pdf'), 'pdf')
  assert.equal(inferOfficeSlotId('app.ts'), undefined)
  assert.equal(inferOfficeSlotId('README'), undefined)
})

test('extracts basename and parent directory on Unix and Windows paths', () => {
  assert.equal(basenameOfPath('/home/goodtime/docs/SKILL.md'), 'SKILL.md')
  assert.equal(parentDirOfPath('/home/goodtime/docs/SKILL.md'), '/home/goodtime/docs')
  assert.equal(parentDirOfPath('/SKILL.md'), '/')
  assert.equal(parentDirOfPath('C:\\Users\\dev\\deck.pptx'), 'C:/Users/dev')
  assert.equal(parentDirOfPath('C:\\deck.pptx'), 'C:/')
})

test('upsert deduplicates by path or url, keeps newest first, and enforces cap', () => {
  let list = upsertRecentOfficeFile([], {
    path: '/home/a/doc1.md',
    filename: 'doc1.md',
    slotId: 'markdown',
    fileUrl: officeLocalFileUrl('/home/a/doc1.md'),
    openedAt: 1000,
  }, 3)
  list = upsertRecentOfficeFile(list, {
    path: '/home/b/sheet.xlsx',
    filename: 'sheet.xlsx',
    slotId: 'excel',
    fileUrl: officeLocalFileUrl('/home/b/sheet.xlsx'),
    openedAt: 2000,
  }, 3)
  // Re-opening doc1 moves it back to top and updates timestamp.
  list = upsertRecentOfficeFile(list, {
    path: '/home/a/doc1.md',
    filename: 'doc1.md',
    slotId: 'markdown',
    fileUrl: '/agent/tasks/t1/workspace-file?path=doc1.md',
    openedAt: 3000,
  }, 3)
  assert.equal(list.length, 2)
  assert.equal(list[0]?.path, '/home/a/doc1.md')
  assert.equal(list[0]?.openedAt, 3000)
  assert.equal(list[0]?.dir, '/home/a')

  list = upsertRecentOfficeFile(list, {
    path: '/home/c/slides.pptx',
    filename: 'slides.pptx',
    slotId: 'ppt',
    fileUrl: officeLocalFileUrl('/home/c/slides.pptx'),
    openedAt: 4000,
  }, 3)
  list = upsertRecentOfficeFile(list, {
    path: '/home/d/manual.pdf',
    filename: 'manual.pdf',
    slotId: 'pdf',
    fileUrl: officeLocalFileUrl('/home/d/manual.pdf'),
    openedAt: 5000,
  }, 3)
  assert.equal(list.length, 3)
  assert.deepEqual(list.map(item => item.filename), ['manual.pdf', 'slides.pptx', 'doc1.md'])

  const afterRemove = removeRecentOfficeFile(list, '/home/c/slides.pptx')
  assert.deepEqual(afterRemove.map(item => item.filename), ['manual.pdf', 'doc1.md'])
})

test('storage round-trip persists and sanitizes entries', () => {
  const data = new Map()
  const fakeStorage = {
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => { data.set(k, v) },
  }
  const initial = upsertRecentOfficeFile([], {
    path: '/home/a/plan.docx',
    filename: 'plan.docx',
    slotId: 'word',
    fileUrl: officeLocalFileUrl('/home/a/plan.docx'),
    openedAt: 12345,
  })
  saveRecentOfficeFiles(initial, fakeStorage)
  const loaded = loadRecentOfficeFiles(fakeStorage)
  assert.deepEqual(loaded, initial)
})

test('formats relative opened-at timestamps cleanly', () => {
  const now = 1_700_000_000_000
  assert.equal(formatRelativeOpenedAt(now - 20_000, now), '刚刚')
  assert.equal(formatRelativeOpenedAt(now - 5 * 60_000, now), '5 分钟前')
  assert.equal(formatRelativeOpenedAt(now - 3 * 3600_000, now), '3 小时前')
  assert.equal(formatRelativeOpenedAt(now - 28 * 3600_000, now), '昨天')
})
