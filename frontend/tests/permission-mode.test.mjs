import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'

// Playwright lives beside this checkout; PLAYWRIGHT_MODULE points at it elsewhere.
const fallback = fileURLToPath(new URL('../../../ai-ppt-view/node_modules/playwright/index.mjs', import.meta.url))
const playwrightModule = process.env.PLAYWRIGHT_MODULE ?? (existsSync(fallback) ? fallback : undefined)

// Each chat keeps its own permission level in the backend, beside its model and
// thinking level. Switching chats and reloading the page must show each chat's
// own choice, and full access must never reach a chat that did not choose it.
test('two chats keep their own permission level across switching and a reload', {
  skip: playwrightModule === undefined ? 'playwright is not available' : false,
}, async () => {
  const { chromium } = await import(playwrightModule)
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    cacheDir: 'node_modules/.vite-permission-mode-test',
    server: { port: 0, host: '127.0.0.1', hmr: false },
    plugins: [{ name: 'permission-mode-test', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url?.split('?')[0] !== '/agent/permission-mode-test') return next()
        const html = await server.transformIndexHtml(req.url, `<!doctype html><html><head><meta charset="utf-8"></head>
<body><div id="root"></div><script type="module">
import React, { useCallback, useState } from 'react';
import { createRoot } from 'react-dom/client';
import '/agent/src/dsh/theme/base.css';
import { Composer } from '/agent/src/components/Composer.tsx';
import { CordisProvider } from '/agent/src/cordis/react.tsx';
import { DEFAULT_PERMISSION_MODE, resolvePermissionMode, useChatPermissionMode } from '/agent/src/permissionMode.ts';
import { api } from '/agent/src/api.ts';

window.resolvePermissionMode = resolvePermissionMode;
window.DEFAULT_PERMISSION_MODE = DEFAULT_PERMISSION_MODE;

// Stands in for the backend: a chat's level is one row stored with the chat, so
// the desktop app and the browser would both read this same value.
const ROWS = 'permission-test-rows';
const rows = () => { try { return JSON.parse(sessionStorage.getItem(ROWS)) ?? null } catch { return null } };
window.rows = rows() ?? { A: { task_id: 'A' }, B: { task_id: 'B' } };
window.saves = [];
api.commands = async () => ({ commands: [] });
api.files = async () => ({ files: [] });
api.updateTask = async (id, payload) => {
  window.saves.push([id, payload.permission_mode]);
  window.rows[id] = { ...window.rows[id], permission_mode: payload.permission_mode };
  sessionStorage.setItem(ROWS, JSON.stringify(window.rows));
  return { ...window.rows[id] };
};

function App() {
  const [chat, setChat] = useState('A');
  const [, setRevision] = useState(0);
  // Saving is what the workspace does: write the chat's row, then re-read it.
  const save = useCallback(async (id, mode) => {
    const updated = await api.updateTask(id, { permission_mode: mode });
    setRevision(current => current + 1);
    return updated;
  }, []);
  const task = chat === null ? undefined : window.rows[chat];
  const { mode, select } = useChatPermissionMode(task, save);
  return React.createElement(React.Fragment, null,
    React.createElement('div', { style: { display: 'flex', gap: '8px' } },
      React.createElement('button', { onClick: () => setChat('A') }, 'chat A'),
      React.createElement('button', { onClick: () => setChat('B') }, 'chat B'),
      React.createElement('button', { onClick: () => setChat(null) }, 'new chat')),
    React.createElement('div', { id: 'shown', 'data-mode': mode }, mode),
    React.createElement(Composer, {
      variant: 'composer', disabled: false, note: undefined, running: false,
      permissionMode: mode, onSelectPermissionMode: select,
      onSubmit: async () => true, onStop: undefined,
    }));
}
createRoot(document.getElementById('root')).render(
  React.createElement(CordisProvider, { context: { agentApi: api } }, React.createElement(App)));
</script></body></html>`)
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
    page.setDefaultTimeout(15000)
    const errors = []
    page.on('pageerror', error => errors.push(String(error)))
    await page.goto(server.resolvedUrls.local[0] + 'permission-mode-test')
    await page.locator('#shown').waitFor()

    const pill = page.getByTitle('切换工作区操作权限')
    const mode = async () => page.locator('#shown').getAttribute('data-mode')
    const pillLabel = async () => ((await pill.textContent()) ?? '').trim()
    /** The level the composer shows, once React has painted it. */
    const shows = async (expected, label) => {
      await page.waitForFunction(value => document.querySelector('#shown')?.getAttribute('data-mode') === value, expected)
      await page.waitForFunction(value => document.querySelector('[title="切换工作区操作权限"]')?.textContent?.trim() === value, label)
      assert.equal(await mode(), expected)
      assert.equal(await pillLabel(), label)
    }
    const openChat = async name => {
      await page.getByRole('button', { name, exact: true }).click()
      await page.waitForFunction(() => document.querySelector('#shown') !== null)
    }
    // Choosing a level goes through the same menu the user sees.
    const pick = async label => {
      await pill.click()
      await page.getByRole('dialog', { name: '工作区操作权限' }).getByText(label, { exact: true }).click()
    }
    const pickFullAccess = async () => {
      await pick('完全权限')
      await page.getByRole('checkbox').click()
      await page.getByRole('button', { name: '切换', exact: true }).click()
    }
    const saves = () => page.evaluate(() => window.saves)

    // Both chats were saved before levels were kept per chat: no value, no error.
    assert.deepEqual(await page.evaluate(() => [window.DEFAULT_PERMISSION_MODE,
      window.resolvePermissionMode(undefined), window.resolvePermissionMode(null),
      window.resolvePermissionMode('full_access'), window.resolvePermissionMode('time_travel'),
      window.resolvePermissionMode('')]),
    ['ask', 'ask', 'ask', 'full_access', 'ask', 'ask'], 'the default is asking first, never full access')
    await openChat('chat A')
    await shows('ask', '执行前询问')

    // Chat A takes full access; the choice is saved at once, before any message.
    await pickFullAccess()
    await shows('full_access', '完全权限')
    assert.deepEqual(await saves(), [['A', 'full_access']], 'choosing a level saves it without sending a message')
    assert.equal(await page.evaluate(() => localStorage.getItem('dsh_permission_mode')), null,
      'a level must not be kept in device storage instead of the chat')

    // Chat B never chose it, so full access must not follow the switch.
    await openChat('chat B')
    await shows('ask', '执行前询问')
    await pick('自动审批')
    await shows('auto_review', '自动审批')
    assert.deepEqual(await saves(), [['A', 'full_access'], ['B', 'auto_review']])

    // Going back and forth shows each chat's own level.
    await openChat('chat A')
    await shows('full_access', '完全权限')
    await openChat('chat B')
    await shows('auto_review', '自动审批')

    // A reload reads both levels back from the backend, not from this page.
    await page.reload()
    await page.locator('#shown').waitFor()
    await openChat('chat A')
    await shows('full_access', '完全权限')
    await openChat('chat B')
    await shows('auto_review', '自动审批')

    // A new chat starts from the default, even while another chat holds full access.
    await openChat('new chat')
    await shows('ask', '执行前询问')
    // Choosing one for a chat that does not exist yet is kept for the chat its
    // first message creates, and is not written to any chat that already exists
    // (the reload above emptied the log of the saves made before it).
    await pick('自动审批')
    await shows('auto_review', '自动审批')
    assert.equal((await saves()).length, 0, 'a new chat choice must not overwrite another chat')
    await openChat('chat A')
    await shows('full_access', '完全权限')
    await openChat('chat B')
    await shows('auto_review', '自动审批')

    // The menu offers only the levels that ask, review or grant; read-only and
    // workspace-write are gone, so no chat can be put back on them.
    await pill.click()
    const menu = page.getByRole('dialog', { name: '工作区操作权限' })
    assert.equal(await menu.getByRole('button').count(), 3, 'three levels are offered')
    const menuText = (await menu.textContent()) ?? ''
    assert.ok(!menuText.includes('仅可查看'), 'read-only is no longer offered')
    assert.ok(!menuText.includes('工作区内修改'), 'workspace-write is no longer offered')
    assert.ok(menuText.includes("访问跨项目目录"))
    assert.ok(menuText.includes("敏感凭据与宿主保护规则始终生效"))
    assert.ok(!menuText.includes("沙箱"))

    assert.deepEqual(errors, [])
  } finally {
    if (browser) await browser.close()
    await server.close()
  }
})

test('CodeChatDock switches permission mode before and after creating a session without clipping', {
  skip: playwrightModule === undefined ? 'playwright is not available' : false,
}, async () => {
  const { chromium } = await import(playwrightModule)
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    cacheDir: 'node_modules/.vite-permission-mode-test',
    server: { port: 0, host: '127.0.0.1', hmr: false },
    plugins: [{ name: 'code-chat-dock-permission-test', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url?.split('?')[0] !== '/agent/code-chat-permission-test') return next()
        const html = await server.transformIndexHtml(req.url, `<!doctype html><html><head><meta charset="utf-8"><style>html,body,#root{margin:0;height:100%;width:100%;display:flex;justify-content:flex-end;}</style></head>
<body><div id="root"></div><script type="module">
import React from 'react';
import { createRoot } from 'react-dom/client';
import '/agent/src/dsh/theme/base.css';
import { CodeChatDock } from '/agent/src/components/CodeChatDock.tsx';
import { CordisProvider } from '/agent/src/cordis/react.tsx';
import { api } from '/agent/src/api.ts';

window.prompts = [];
window.updates = [];
const tasks = {};
api.getSettings = async () => ({ providers: [], orchestrator_provider: '', orchestrator_model: '' });
api.createSession = async (workspace) => {
  const id = 'dock-task-1';
  tasks[id] = { task_id: id, workspace, status: 'idle', title: '代码问答', permission_mode: 'ask' };
  return id;
};
api.updateSession = async () => ({});
api.getTask = async (id) => tasks[id];
api.events = async () => [];
api.streamTask = () => () => {};
api.prompt = async (id, prompt, options) => {
  window.prompts.push({ id, prompt, permission_mode: options?.permission_mode });
  if (tasks[id]) tasks[id] = { ...tasks[id], permission_mode: options?.permission_mode ?? 'ask' };
  return {};
};
api.updateTask = async (id, payload) => {
  window.updates.push({ id, permission_mode: payload.permission_mode });
  if (tasks[id]) tasks[id] = { ...tasks[id], permission_mode: payload.permission_mode };
  return tasks[id];
};

const snapshot = { current: undefined, tags: [] };
const codeChat = { subscribe: () => () => {}, getSnapshot: () => snapshot, add() {}, remove() {}, clear() {} };

localStorage.clear();
createRoot(document.getElementById('root')).render(
  React.createElement(CordisProvider, { context: { agentApi: api, codeChat } },
    React.createElement(CodeChatDock, {
      workspace: '/tmp/demo-repo',
      renderSlot: () => null,
      onOpenInChat: () => {},
      openSignal: 1,
    })
  )
);
</script></body></html>`)
        res.setHeader('Content-Type', 'text/html')
        res.end(html)
      })
    } }],
  })
  let browser
  try {
    await server.listen()
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {}) })
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } })
    page.setDefaultTimeout(15000)
    const errors = []
    page.on('pageerror', error => errors.push(String(error)))
    await page.goto(server.resolvedUrls.local[0] + 'code-chat-permission-test')

    const pill = page.getByTitle('切换工作区操作权限')
    await pill.waitFor()
    assert.equal(((await pill.textContent()) ?? '').trim(), '执行前询问')

    // 1. Open popover in right-docked panel and verify it stays inside the viewport.
    await pill.click()
    const popover = page.getByRole('dialog', { name: '工作区操作权限' })
    await popover.waitFor()
    const box = await popover.boundingBox()
    assert.ok(((await popover.textContent()) ?? "").includes("访问跨项目目录"))
    assert.ok(box && box.x >= 16 && box.x + box.width <= 1280 - 15, `popover must stay inside viewport, got ${JSON.stringify(box)}`)

    // 2. Switch to 自动审批 BEFORE sending the first message, then send.
    await popover.getByText('自动审批', { exact: true }).click()
    assert.equal(((await pill.textContent()) ?? '').trim(), '自动审批')

    await page.locator('textarea').fill('帮我看看这张表')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    await page.waitForFunction(() => window.prompts.length === 1)
    assert.deepEqual(await page.evaluate(() => window.prompts), [{ id: 'dock-task-1', prompt: '帮我看看这张表', permission_mode: 'auto_review' }])

    // 3. Switch existing session to 完全权限 via RiskConfirmation and verify backend updateTask is called.
    await pill.click()
    await page.getByRole('dialog', { name: '工作区操作权限' }).getByText('完全权限', { exact: true }).click()
    await page.getByRole('checkbox').click()
    await page.getByRole('button', { name: '切换', exact: true }).click()
    await page.waitForFunction(() => window.updates.length === 1)
    assert.equal(((await pill.textContent()) ?? '').trim(), '完全权限')
    assert.deepEqual(await page.evaluate(() => window.updates), [{ id: 'dock-task-1', permission_mode: 'full_access' }])

    assert.deepEqual(errors, [])
  } finally {
    if (browser) await browser.close()
    await server.close()
  }
})
