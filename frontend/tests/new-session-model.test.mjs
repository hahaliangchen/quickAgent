import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ?? 'playwright')
const browserOptions = { skip: existsSync(process.env.PLAYWRIGHT_EXECUTABLE_PATH ?? chromium.executablePath()) ? false : 'Chromium is not installed' }

async function withChatPage(run) {
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    cacheDir: 'node_modules/.vite-new-session-model-test',
    server: { port: 0, host: '127.0.0.1', hmr: false },
    plugins: [{ name: 'new-session-model-test', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url?.split('?')[0] !== '/agent/new-session-model-test') return next()
        const html = await server.transformIndexHtml(req.url, pageSource)
        res.setHeader('Content-Type', 'text/html')
        res.end(html)
      })
    } }],
  })
  let browser
  try {
    await server.listen(0)
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {}) })
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, permissions: ['notifications'] })
    page.setDefaultTimeout(10000)
    await page.route('**/agent/settings/shell', route => route.fulfill({ status: 503 }))
    const errors = []
    page.on('pageerror', error => errors.push(String(error)))
    const failedRequests = []
    page.on('response', response => { if (response.status() >= 400) failedRequests.push(response.status() + ' ' + response.url()) })
    await page.goto(server.resolvedUrls.local[0] + 'new-session-model-test#/tasks/original')
    const model = provider => page.getByTitle('gpt-6.2-sol · ' + provider, { exact: true })
    const newSession = () => page.getByRole('button', { name: '新会话', exact: true }).click()
    try { await run(page, model, newSession, errors) } catch (error) {
      throw new Error([String(error), ...errors, ...failedRequests].join('\n'), { cause: error })
    }
  } finally {
    await browser?.close()
    await server.close()
  }
}

test('new session carries the current model and provider before the global default changes', browserOptions, async () => {
  await withChatPage(async (page, model, newSession, errors) => {
    await model('Primary').waitFor()
    await newSession()
    await page.locator('[data-phase="hero"]').waitFor()
    await model('Primary').waitFor()
    const send = async text => {
      const input = page.locator('main textarea')
      await input.fill(text)
      await input.press('Enter')
    }
    await send('Use the inherited model')
    await page.waitForFunction(() => window.sent.length === 1 && location.hash === '#/tasks/new-1')
    assert.deepEqual(await page.evaluate(() => window.sent), [{ id: 'new-1', model: 'gpt-6.2-sol', provider: 'primary', permission: 'ask' }])

    await model('Primary').click()
    await page.getByRole('menuitem', { name: /^模型/ }).click()
    await page.locator('section').filter({ hasText: 'Secondary' }).getByRole('menuitemradio', { name: 'gpt-6.2-sol' }).click()
    await model('Secondary').waitFor()
    await page.waitForFunction(() => window.settingsSavePending)
    await newSession()
    await page.locator('[data-phase="hero"]').waitFor()
    await model('Secondary').waitFor()
    assert.equal(await page.evaluate(() => window.settingsSavePending), true)
    await send('Keep the newly chosen provider')
    await page.waitForFunction(() => window.sent.length === 2 && location.hash === '#/tasks/new-2')
    assert.deepEqual(await page.evaluate(() => window.sent[1]), { id: 'new-2', model: 'gpt-6.2-sol', provider: 'secondary', permission: 'ask' })
    assert.deepEqual(errors, [])
  })
})

test('chat headers jump to recent messages and a new chat restores its independent draft', browserOptions, async () => {
  await withChatPage(async (page, _model, newSession, errors) => {
    const recent = page.getByRole('navigation', { name: '最近对话轮次' }).getByRole('button')
    await recent.nth(1).waitFor()
    assert.deepEqual((await recent.allTextContents()).map(text => text.slice(0, 20)), ['上一轮Previous question', '当前轮Current question '])
    const currentTitle = await recent.nth(1).getAttribute('title')
    assert.match(currentTitle, /^Current question with a long summary/)
    const checkEllipsis = () => recent.nth(1).locator('span').last().evaluate(element =>
      element.clientWidth > 0 && element.scrollWidth > element.clientWidth && getComputedStyle(element).textOverflow === 'ellipsis')
    assert.equal(await checkEllipsis(), true)
    if (process.env.CHAT_SCREENSHOT_DIR) await page.locator('main > header').screenshot({ path: process.env.CHAT_SCREENSHOT_DIR + '/chat-header-wide.png' })
    await page.setViewportSize({ width: 720, height: 900 })
    assert.equal(await checkEllipsis(), true)
    if (process.env.CHAT_SCREENSHOT_DIR) await page.locator('main > header').screenshot({ path: process.env.CHAT_SCREENSHOT_DIR + '/chat-header-narrow.png' })
    await page.setViewportSize({ width: 1280, height: 900 })

    const jumpedTo = key => page.waitForFunction(key => {
      const scroller = document.querySelector('[data-conversation-scroll]')
      const row = document.activeElement
      return row?.getAttribute('data-chat-turn-message') === key && scroller.scrollTop > 0
        && Math.abs(row.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 12) < 4
    }, key)
    await recent.first().click()
    await jumpedTo('user/message:6')
    await page.getByRole('tab', { name: '流程图' }).click()
    await recent.last().press('Enter')
    await jumpedTo('user/message:10')
    assert.equal(await page.getByRole('tab', { name: '对话' }).getAttribute('aria-selected'), 'true')

    const input = page.locator('main textarea')
    const expectDraft = text => page.waitForFunction(text => document.querySelector('main textarea')?.value === text, text)
    const open = id => page.getByText('Session ' + id, { exact: true }).click()
    await newSession()
    await page.locator('[data-phase="hero"]').waitFor()
    await input.fill('Retry this draft')
    await open('original')
    await recent.nth(1).waitFor()
    await expectDraft('')
    await open('saved-draft')
    await page.locator('[data-phase="hero"]').waitFor()
    await expectDraft('')
    await input.fill('A different server draft')
    await newSession()
    await expectDraft('Retry this draft')
    await page.reload()
    await expectDraft('Retry this draft')
    await input.press('Enter')
    await page.getByText('Mock send failed', { exact: true }).waitFor()
    await expectDraft('Retry this draft')
    await input.press('Enter')
    await page.waitForFunction(() => location.hash === '#/tasks/new-1')
    await page.getByRole('button', { name: '当前轮 Retry this draft' }).waitFor()
    assert.equal(await recent.count(), 1)
    await newSession()
    await expectDraft('')
    await page.reload()
    await expectDraft('')
    assert.deepEqual(errors, [])
  })
})

const pageSource = '<!doctype html><html><head><meta charset="utf-8"></head><body><div id="root"></div><script type="module">\n' + String.raw`
import React from 'react';
import { createRoot } from 'react-dom/client';
import '/agent/src/dsh/theme/base.css';
import { App } from '/agent/src/App.tsx';
import { Composer } from '/agent/src/components/Composer.tsx';
import { ChatView } from '/agent/src/components/ChatView.tsx';
import { Sidebar } from '/agent/src/components/Sidebar.tsx';
import { CordisProvider } from '/agent/src/cordis/react.tsx';

const workspace = '/project';
const makeTask = (task_id, model, provider_name, permission_mode = 'ask') => ({
  task_id, model, provider_name, permission_mode, workspace, kind: 'code',
  status: 'completed', parent_task_id: null, prompt: 'Session ' + task_id,
  created_at: Date.now(), updated_at: Date.now(),
});
const rows = new Map([
  ['original', makeTask('original', 'gpt-6.2-sol', 'primary', 'full_access')],
  ['saved-draft', { ...makeTask('saved-draft', 'gpt-6.2-sol', 'primary'), status: 'draft' }],
]);
let nextSession = 1;
const event = (seq, type, data) => ({ seq, type, data, time: Date.now() + seq * 100 });
const reply = Array.from({ length: 55 }, (_, i) => 'Assistant paragraph ' + i).join('\n\n');
const histories = new Map([['original', ['First question', 'Previous question', 'Current question  ' + 'with a long summary '.repeat(20)].flatMap((content, i) => [
  event(i * 4 + 1, 'turn/start', { turn: i + 1 }),
  event(i * 4 + 2, 'user/message', { content }),
  event(i * 4 + 3, 'assistant/message', { content: reply, turn: i + 1, step: 1 }),
  event(i * 4 + 4, 'turn/end', { turn: i + 1, status: 'completed' }),
])]]);
const provider = (id, display_name, model) => ({ id, display_name, models: [model], model_capabilities: {} });
const settings = {
  orchestrator_provider: 'local', orchestrator_model: 'qwen3.8-27b', revision: 'initial',
  providers: [provider('local', 'Local', 'qwen3.8-27b'), provider('primary', 'Primary', 'gpt-6.2-sol'), provider('secondary', 'Secondary', 'gpt-6.2-sol')],
};
window.sent = [];
window.settingsSavePending = false;
const agentApi = {
  info: async () => ({ ready: true, workspace, model: settings.orchestrator_model }),
  getSettings: async () => settings,
  saveSettings: () => { window.settingsSavePending = true; return new Promise(() => {}); },
  projects: async () => [{ path: workspace, title: 'Project' }],
  listTasks: async () => [...rows.values()],
  getTask: async id => ({ ...rows.get(id) }),
  events: async id => [...(histories.get(id) ?? [])],
  commands: async () => ({ commands: [] }),
  files: async () => ({ files: [] }),
  updateTask: async (id, input) => {
    const task = { ...rows.get(id), ...input, provider_name: input.provider };
    rows.set(id, task);
    return task;
  },
  createSession: async () => {
    const id = 'new-' + nextSession++;
    rows.set(id, makeTask(id, settings.orchestrator_model, settings.orchestrator_provider));
    return id;
  },
  prompt: async (id, text, input) => {
    if (text === 'Retry this draft' && !window.rejectedDraft) {
      window.rejectedDraft = true;
      throw new Error('Mock send failed');
    }
    const events = histories.get(id) ?? [];
    events.push(event(events.length + 1, 'user/message', { content: text }));
    histories.set(id, events);
    window.sent.push({ id, model: input.model, provider: input.provider, permission: input.permission_mode });
    rows.set(id, makeTask(id, input.model, input.provider, input.permission_mode));
  },
};
const slots = { 'agent.sidebar': Sidebar, 'agent.composer': Composer, 'agent.chat': ChatView };
const renderSlot = (name, props) => slots[name] ? React.createElement(slots[name], props) : null;
createRoot(document.getElementById('root')).render(
  React.createElement(CordisProvider, { context: { agentApi } }, React.createElement(App, { renderSlot })));
` + '</script></body></html>'
