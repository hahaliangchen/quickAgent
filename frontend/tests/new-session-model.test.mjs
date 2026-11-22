import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'

const fallback = fileURLToPath(new URL('../../../ai-ppt-view/node_modules/playwright/index.mjs', import.meta.url))
const playwrightModule = process.env.PLAYWRIGHT_MODULE ?? (existsSync(fallback) ? fallback : undefined)

test('new session carries the current model and provider before the global default changes', {
  skip: playwrightModule === undefined ? 'playwright is not available' : false,
}, async () => {
  const { chromium } = await import(playwrightModule)
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
    await page.goto(server.resolvedUrls.local[0] + 'new-session-model-test#/tasks/original')

    const model = provider => page.getByTitle('gpt-6.2-sol · ' + provider, { exact: true })
    const newSession = () => page.getByRole('button', { name: '新会话', exact: true }).click()
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
  } finally {
    await browser?.close()
    await server.close()
  }
})

const pageSource = '<!doctype html><html><head><meta charset="utf-8"></head><body><div id="root"></div><script type="module">\n' + String.raw`
import React from 'react';
import { createRoot } from 'react-dom/client';
import '/agent/src/dsh/theme/base.css';
import { App } from '/agent/src/App.tsx';
import { Composer } from '/agent/src/components/Composer.tsx';
import { Sidebar } from '/agent/src/components/Sidebar.tsx';
import { CordisProvider } from '/agent/src/cordis/react.tsx';

const workspace = '/project';
const makeTask = (task_id, model, provider_name, permission_mode = 'ask') => ({
  task_id, model, provider_name, permission_mode, workspace, kind: 'code',
  status: 'completed', parent_task_id: null, prompt: 'Session ' + task_id,
  created_at: Date.now(), updated_at: Date.now(),
});
const rows = new Map([['original', makeTask('original', 'gpt-6.2-sol', 'primary', 'full_access')]]);
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
  events: async () => [],
  commands: async () => ({ commands: [] }),
  files: async () => ({ files: [] }),
  updateTask: async (id, input) => {
    const task = { ...rows.get(id), ...input, provider_name: input.provider };
    rows.set(id, task);
    return task;
  },
  createSession: async () => {
    const id = 'new-' + rows.size;
    rows.set(id, makeTask(id, settings.orchestrator_model, settings.orchestrator_provider));
    return id;
  },
  prompt: async (id, _text, input) => {
    window.sent.push({ id, model: input.model, provider: input.provider, permission: input.permission_mode });
    rows.set(id, makeTask(id, input.model, input.provider, input.permission_mode));
  },
};
const slots = { 'agent.sidebar': Sidebar, 'agent.composer': Composer };
const renderSlot = (name, props) => slots[name] ? React.createElement(slots[name], props) : null;
createRoot(document.getElementById('root')).render(
  React.createElement(CordisProvider, { context: { agentApi } }, React.createElement(App, { renderSlot })));
` + '</script></body></html>'
