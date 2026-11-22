import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'
import { createServer } from 'vite'

const source = await readFile(new URL('../src/composerHistory.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { appendHistory, createComposerHistoryStore, HISTORY_STORAGE_KEY, historyDirection, stepHistoryUp, stepHistoryDown } =
  await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

function memoryStorage(raw = null) {
  return {
    getItem(key) { assert.equal(key, HISTORY_STORAGE_KEY); return raw },
    setItem(key, value) { assert.equal(key, HISTORY_STORAGE_KEY); raw = value },
  }
}

test('keeps the last 50 sent texts, skips blanks, and deduplicates consecutive entries only', () => {
  const storage = memoryStorage()
  const store = createComposerHistoryStore(() => storage)
  const entries = ['A', 'B', 'A']
  assert.equal(appendHistory(entries, '  A\n'), entries)
  assert.equal(appendHistory(entries, '\t '), entries)
  for (let i = 1; i <= 55; i++) store.record('message ' + i)
  store.record('message 55')
  store.record(' ')
  const reloaded = createComposerHistoryStore(() => storage).read()
  assert.equal(reloaded.length, 50)
  assert.equal(reloaded[0], 'message 6')
  assert.equal(reloaded.at(-1), 'message 55')
  for (const text of entries) store.record(text)
  assert.deepEqual(store.read().slice(-3), entries)
})

test('invalid or unavailable storage does not prevent recall', () => {
  const corrupt = memoryStorage('{invalid')
  const store = createComposerHistoryStore(() => corrupt)
  assert.deepEqual(store.read(), [])
  store.record('recovered')
  assert.deepEqual(store.read(), ['recovered'])
  const sanitized = createComposerHistoryStore(() => memoryStorage('[null, "", " A ", "A", 42, "B"]'))
  assert.deepEqual(sanitized.read(), ['A', 'B'])
  const offline = createComposerHistoryStore(() => ({ getItem: () => '["saved"]', setItem: () => { throw Error('no storage') } }))
  offline.record('in memory')
  assert.deepEqual(offline.read(), ['saved', 'in memory'])
})

test('steps from newest to oldest without wrapping and restores exact draft and caret', () => {
  const draft = { text: '  unfinished\nsecond line  ', caret: 0 }
  const entries = ['first\nline', 'second', 'third\nline']
  assert.equal(stepHistoryUp([], null, draft), null)
  assert.equal(stepHistoryDown(null), null)
  let step = stepHistoryUp(entries, null, draft)
  assert.deepEqual([step.text, step.caret], ['third\nline', 5])
  step = stepHistoryUp(['newer list'], step.cursor, { text: step.text, caret: 0 })
  assert.equal(step.text, 'second')
  step = stepHistoryUp(entries, step.cursor, draft)
  assert.deepEqual([step.text, step.caret], ['first\nline', 5])
  assert.deepEqual(stepHistoryUp(entries, step.cursor, draft), step)
  step = stepHistoryDown(step.cursor)
  assert.equal(step.text, 'second')
  step = stepHistoryDown(step.cursor)
  assert.deepEqual([step.text, step.caret], ['third\nline', 10])
  assert.deepEqual(stepHistoryDown(step.cursor), { cursor: null, ...draft })
})

const plain = { keyCode: 0, isComposing: false, shiftKey: false, ctrlKey: false, altKey: false, metaKey: false }
const direction = (key, value, start, navigating, options = {}) => historyDirection(
  { ...plain, key, ...options.event }, { value, selectionStart: start, selectionEnd: options.end ?? start }, navigating, options.menu ?? false,
)

test('Up only recalls from an eligible first line; Down only recalls from the last line in history', () => {
  assert.equal(direction('ArrowUp', '', 0, false), 'up')
  assert.equal(direction('ArrowUp', 'draft\nline 2', 0, false), 'up')
  assert.equal(direction('ArrowUp', 'draft\nline 2', 5, false), null)
  assert.equal(direction('ArrowUp', 'old\nnext', 3, true), 'up')
  assert.equal(direction('ArrowUp', 'old\nnext', 4, true), null)
  assert.equal(direction('ArrowDown', '', 0, false), null)
  assert.equal(direction('ArrowDown', 'old\nnext', 3, true), null)
  assert.equal(direction('ArrowDown', 'old\nnext', 4, true), 'down')
  assert.equal(direction('ArrowDown', 'old\n', 4, true), 'down')
  assert.equal(direction('ArrowLeft', '', 0, true), null)
})

test('direction keys ignore both IME signals, all modifiers, selections, and open menus', () => {
  for (const key of ['ArrowUp', 'ArrowDown']) {
    assert.notEqual(direction(key, 'draft', 0, true), null)
    for (const flag of ['isComposing', 'shiftKey', 'ctrlKey', 'altKey', 'metaKey']) {
      assert.equal(direction(key, 'draft', 0, true, { event: { [flag]: true } }), null, flag)
    }
    assert.equal(direction(key, 'draft', 0, true, { event: { keyCode: 229 } }), null)
    assert.equal(direction(key, 'draft', 0, true, { end: 2 }), null)
    assert.equal(direction(key, 'draft', 0, true, { menu: true }), null)
  }
})

const fallback = fileURLToPath(new URL('../../../ai-ppt-view/node_modules/playwright/index.mjs', import.meta.url))
const playwrightModule = process.env.PLAYWRIGHT_MODULE ?? (existsSync(fallback) ? fallback : undefined)

test('Composer recalls only successful sends and keeps drafts and attachments separate from history', {
  skip: playwrightModule === undefined ? 'playwright is not available' : false,
}, async () => {
  const { chromium } = await import(playwrightModule)
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    cacheDir: 'node_modules/.vite-composer-history-test',
    server: { port: 0, host: '127.0.0.1', hmr: false },
    plugins: [{ name: 'composer-history-test', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url !== '/agent/composer-history-test') return next()
        const html = await server.transformIndexHtml(req.url, fixture)
        res.setHeader('Content-Type', 'text/html')
        res.end(html)
      })
    } }],
  })
  let browser
  try {
    await server.listen()
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {}) })
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
    page.setDefaultTimeout(10000)
    await page.goto(server.resolvedUrls.local[0] + 'composer-history-test')
    const box = page.locator('textarea')
    const show = async text => {
      await page.waitForFunction(expected => document.querySelector('textarea')?.value === expected, text)
      assert.equal(await box.inputValue(), text)
    }
    const home = () => box.evaluate(input => input.setSelectionRange(0, 0))
    const history = () => page.evaluate(key => JSON.parse(localStorage.getItem(key) ?? '[]'), HISTORY_STORAGE_KEY)
    const open = key => page.getByRole('button', { name: 'Chat ' + key, exact: true }).click()

    await box.fill('sent one')
    await box.press('Enter')
    await show('')
    await box.fill('reject me')
    await box.press('Enter')
    assert.deepEqual(await history(), ['sent one'])

    await box.fill('my unsent draft')
    await page.getByLabel('上传附件').setInputFiles({ name: 'note.txt', mimeType: 'text/plain', buffer: Buffer.from('attachment') })
    const attachment = page.getByText('note.txt', { exact: true })
    await attachment.waitFor()
    await home()
    await box.press('ArrowUp')
    await show('sent one')
    assert.ok(await attachment.isVisible())
    await box.press('ArrowDown')
    await show('my unsent draft')
    assert.equal(await box.evaluate(input => input.selectionStart), 0)
    await box.press('ArrowUp')
    await show('sent one')
    await open('B')
    await show('')
    await open('A')
    await show('my unsent draft')
    assert.ok(await attachment.isVisible())

    // Selection and an open model menu leave the textarea alone.
    await box.evaluate(input => input.setSelectionRange(0, 2))
    await box.press('ArrowUp')
    await show('my unsent draft')
    const model = page.getByRole('button', { name: 'test-model' })
    await model.click()
    await page.getByRole('menu', { name: '模型选择与推理设置' }).waitFor()
    await home()
    await box.press('ArrowUp')
    await show('my unsent draft')
    await box.press('Escape')
    await home()
    await box.press('ArrowUp')
    await show('sent one')
    await box.fill('edited recall')
    await box.press('ArrowDown')
    await show('edited recall')
    await box.press('Enter')
    await show('')
    assert.deepEqual(await history(), ['sent one', 'edited recall'])
    assert.equal(await attachment.count(), 0)
  } finally {
    await browser?.close()
    await server.close()
  }
})

const fixture = '<!doctype html><html><head><meta charset="utf-8"></head><body><div id="root"></div><script type="module">\n' + String.raw`
import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import '/agent/src/dsh/theme/base.css';
import { Composer } from '/agent/src/components/Composer.tsx';
import { CordisProvider } from '/agent/src/cordis/react.tsx';

function App() {
  const [key, setKey] = useState('A');
  return React.createElement(React.Fragment, null,
    ...['A', 'B'].map(id => React.createElement('button', { key: id, onClick: () => setKey(id) }, 'Chat ' + id)),
    React.createElement(Composer, {
      variant: 'composer', draftKey: key, running: false, disabled: false, modelName: 'test-model',
      permissionMode: 'ask', onSelectPermissionMode: () => {}, onSubmit: async text => text !== 'reject me',
    }));
}
const agentApi = { commands: async () => ({ commands: [] }), files: async () => ({ files: [] }) };
createRoot(document.getElementById('root')).render(React.createElement(CordisProvider, { context: { agentApi } }, React.createElement(App)));
` + '</script></body></html>'
