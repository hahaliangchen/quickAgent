import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import assert from 'node:assert/strict'
import test from 'node:test'
import { changeSummaries } from '../src/changes.ts'
import { chatItems } from '../src/model.ts'
import { loadToolDetail } from '../src/toolDetail.ts'

test('trimmed history retains tool identity, live text and per-turn changes; details load and cache on demand', async t => {
  const event = (seq, type, data) => ({ seq, time: 50, type, data })
  const tool = content => ({ message: { role: 'tool', toolCallId: 'call-1', isError: false, content } })
  const data = { ...tool('preview'), truncated: true, detail_seq: 5, full_bytes: 128_000, meta: { result: { diff: { added: 2 } } } }
  const complete = event(5, 'tool/result', tool('complete output'))
  const rows = [
    event(1, 'user/message', { turn: 1, message: { role: 'user', content: 'Edit a file' } }),
    event(4, 'tool/call', { turn: 1, step: 1, callId: 'call-1', name: 'edit_file', arguments: '{}' }),
    event(5, 'tool/result', data),
    event(6, 'workspace/changes', { turn: 1, files: [{ path: 'a.txt' }], total: 1 }),
    event(7, 'turn/end', { turn: 1 }),
    event(8, 'assistant/delta', { turn: 2, step: 1, text: 'still live' }),
  ]
  const fetch = t.mock.method(globalThis, 'fetch', async () => new Response(JSON.stringify(complete)))
  const items = chatItems(rows)
  const preview = items.find(item => item.kind === 'tool').result
  assert.equal(preview.text, 'preview')
  assert.equal(preview.detailSeq, 5)
  assert.deepEqual(preview.summary, { diff: { added: 2 } })
  assert.equal(items.find(item => item.kind === 'assistant').text, 'still live')
  assert.equal(changeSummaries(rows).get(1).files[0].path, 'a.txt')
  assert.equal(fetch.mock.callCount(), 0)
  const load = () => loadToolDetail('task id', preview.detailSeq, 'test-cache-a', preview.fullBytes)
  const first = load()
  assert.equal(load(), first, 'concurrent consumers share a request')
  assert.equal((await first).text, 'complete output')
  assert.equal((await load()).detailSeq, undefined)
  assert.equal(fetch.mock.callCount(), 1)
  assert.equal(fetch.mock.calls[0].arguments[0], '/agent/tasks/task%20id/events/5')
  await loadToolDetail('other-task', 5, 'test-cache-b', preview.fullBytes)
  assert.equal(fetch.mock.callCount(), 2)
})

const fallback = fileURLToPath(new URL('../../../ai-ppt-view/node_modules/playwright/index.mjs', import.meta.url))
const playwright = process.env.PLAYWRIGHT_MODULE ?? (existsSync(fallback) ? fallback : undefined)

test('a rendered tool row only fetches the full result on expansion', { skip: !playwright }, async () => {
  const { chromium } = await import(playwright)
  const fixture = '/agent/tool-detail-test'
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)), cacheDir: 'node_modules/.vite-tool-detail-test',
    server: { host: '127.0.0.1', port: 0, hmr: false },
    plugins: [{ name: 'tool-detail-fixture', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url !== fixture) return next()
        res.setHeader('Content-Type', 'text/html')
        res.end(await server.transformIndexHtml(fixture, html))
      })
    } }],
  })
  let browser
  try {
    await server.listen(0)
    browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH })
    const page = await browser.newPage()
    page.setDefaultTimeout(8000)
    let calls = 0
    await page.route('**/agent/tasks/test/events/42', route => {
      calls += 1
      return route.fulfill({ json: { seq: 42, time: 50, type: 'tool/result', data: { message: { role: 'tool', content: 'Full tool detail' } } } })
    })
    await page.goto(new URL(fixture, server.resolvedUrls.local[0]).href)
    const row = page.locator('[data-disclosure-row]')
    await row.waitFor()
    assert.equal(await row.getAttribute('aria-expanded'), 'false')
    assert.equal(calls, 0)
    await row.click()
    await page.getByText('Full tool detail', { exact: false }).waitFor()
    await row.click()
    await row.click()
    await page.getByText('Full tool detail', { exact: false }).waitFor()
    assert.equal(calls, 1)
  } finally {
    await browser?.close()
    await server.close()
  }
})

const html = `<!doctype html><html><body><div id="root"></div><script type="module">
import React from 'react';
import { createRoot } from 'react-dom/client';
import { ToolRow } from '/agent/src/components/ToolRow.tsx';
import { ConversationTaskProvider } from '/agent/src/components/ConversationTask.tsx';
createRoot(document.getElementById('root')).render(React.createElement(ConversationTaskProvider, { taskId: 'test' },
  React.createElement(ToolRow, { name: 'test_tool', rawArguments: '{}', stopped: false, onOpenChild() {},
    result: { text: 'Preview', isError: false, time: 50, detailSeq: 42, fullBytes: 40960 } })));
</script></body></html>`
