import assert from 'node:assert/strict'
import test from 'node:test'
import { readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import ts from 'typescript'
import { createServer } from 'vite'

const source = await readFile(new URL('../src/shellSetup.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { isCodeShellContext, shellSetupApi } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('shell setup is limited to loaded code sessions and new code workspaces', () => {
  const task = { task_id: 'code', kind: 'code', workspace: 'D:/project' }
  assert.equal(isCodeShellContext('code', task, 'chat', ''), true)
  assert.equal(isCodeShellContext('plugin', { ...task, task_id: 'plugin', plugin_id: 'ppt' }, 'code', 'D:/project'), false)
  assert.equal(isCodeShellContext('chat', { ...task, task_id: 'chat', kind: 'chat' }, 'code', 'D:/project'), false)
  assert.equal(isCodeShellContext('loading', undefined, 'code', 'D:/project'), false)
  assert.equal(isCodeShellContext('loading', task, 'code', 'D:/project'), false)
  assert.equal(isCodeShellContext(undefined, undefined, 'chat', 'D:/project'), false)
  assert.equal(isCodeShellContext(undefined, undefined, 'code', ''), false)
  assert.equal(isCodeShellContext(undefined, undefined, 'code', 'D:/project'), true)
})

test('shell API keeps dependency probes separate from explicit installation and path changes', async () => {
  const original = globalThis.fetch
  const calls = []
  globalThis.fetch = async (url, init) => {
    calls.push({ url, method: init?.method ?? 'GET', body: init?.body })
    return new Response(JSON.stringify({ available: false, running: false, status: 'idle' }))
  }
  try {
    await shellSetupApi.status()
    await shellSetupApi.progress()
    assert.ok(calls.every(call => call.method === 'GET'))
    await shellSetupApi.detect()
    assert.equal(calls.at(-1).body, '{}')
    await shellSetupApi.detect('D:/custom/pwsh.exe')
    assert.deepEqual(JSON.parse(calls.at(-1).body), { path: 'D:/custom/pwsh.exe' })
    await shellSetupApi.detect('')
    assert.deepEqual(JSON.parse(calls.at(-1).body), { path: '' })
    await shellSetupApi.install()
    assert.equal(calls.at(-1).url, '/agent/settings/shell/install')
    assert.equal(calls.at(-1).method, 'POST')
    await shellSetupApi.openStore()
    assert.equal(calls.at(-1).url, '/agent/settings/shell/store')
  } finally { globalThis.fetch = original }
})

test('shell API surfaces installation failures', async () => {
  const original = globalThis.fetch
  globalThis.fetch = async () => new Response(JSON.stringify({ error: 'winget is unavailable' }), { status: 503 })
  try { await assert.rejects(shellSetupApi.install(), /winget is unavailable/) }
  finally { globalThis.fetch = original }
})

test('PowerShell setup offers install progress, retry, Store, manual path, and excludes plugins', { skip: !process.env.PLAYWRIGHT_MODULE }, async () => {
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE)
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    cacheDir: join(tmpdir(), 'shell-setup-vite-test-cache'),
    server: { port: 0, host: '127.0.0.1', hmr: false },
    plugins: [{ name: 'shell-setup-test', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url?.split('?')[0] !== '/agent/shell-setup-test') return next()
        const html = await server.transformIndexHtml(req.url, `<!doctype html><html><head><meta charset="utf-8"><style>
          body {font-family:system-ui;--codex-text-primary:#17251f;--codex-text-muted:#65756b;--codex-card-bg:#fff;--codex-card-border:#d8e2dc;--dsw-alias-bg-mask-1:#0004;--dsw-alias-bg-layer-2:#fff;--codex-blue-primary:#137454}
        </style></head><body><div id="root"></div><script type="module">
          import React from 'react'; import {createRoot} from 'react-dom/client';
          import {ShellSetupDialog} from '/agent/src/components/ShellSetupDialog.tsx';
          import {isCodeShellContext} from '/agent/src/shellSetup.ts';
          function Test(){const [plugin,setPlugin]=React.useState(new URLSearchParams(location.search).has('plugin'));const [kind,setKind]=React.useState('code');
            const task={task_id:'t',kind,workspace:'D:/project',plugin_id:plugin?'ppt':null};
            return React.createElement(React.Fragment,null,
              React.createElement('button',{onClick:()=>setPlugin(true)},'切换插件会话'),
              React.createElement('button',{onClick:()=>{setPlugin(false);setKind('chat')}},'切换普通聊天'),
              React.createElement('button',{onClick:()=>{setPlugin(false);setKind('code')}},'切换代码会话'),
              React.createElement(ShellSetupDialog,{enabled:isCodeShellContext('t',task,'code','D:/project'),scope:plugin?'plugin':kind,onReady:()=>{window.shellReady=true}}));}
          createRoot(document.getElementById('root')).render(React.createElement(Test));
        </script></body></html>`)
        res.setHeader('Content-Type', 'text/html'); res.end(html)
      })
    }}],
  })
  let browser
  try {
    await server.listen()
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {}) })
    const page = await browser.newPage({ viewport: { width: 1100, height: 850 } })
    const errors = []
    page.on('pageerror', error => errors.push(String(error)))
    const calls = []
    let available = false, started = false, finished = false, failInstall = false, pwshDetected = false, os = 'windows'
    const command = 'winget install --id 9MZ1SNWT0N5D --source msstore'
    const status = () => ({ available, pwsh_detected: pwshDetected, platform: { os, arch: 'x86_64' }, error: available ? null : 'PowerShell 7 is missing', install: { command, store_url: 'https://apps.microsoft.com/detail/9MZ1SNWT0N5D' } })
    await page.route('**/agent/settings/shell**', async route => {
      const request = route.request(), url = new URL(request.url()), method = request.method()
      calls.push({ path: url.pathname, method, body: request.postDataJSON() })
      let data
      if (url.pathname.endsWith('/install') && method === 'POST') {
        started = true
        data = failInstall ? { running: false, status: 'failed', exit_code: 1, stderr: 'winget missing', error: 'winget missing' } : { running: true, status: 'running', stdout: '正在下载 PowerShell 7…' }
      } else if (url.pathname.endsWith('/install')) {
        data = !started ? { running: false, status: 'idle' } : { running: !finished, status: finished ? 'completed' : 'running', stdout: '正在下载 PowerShell 7…', exit_code: finished ? 0 : null }
      } else if (url.pathname.endsWith('/detect')) {
        if (finished || request.postDataJSON()?.path === 'D:/custom/pwsh.exe') available = true
        data = status()
      } else if (url.pathname.endsWith('/store')) data = { opened: true }
      else data = status()
      await route.fulfill({ json: data })
    })
    const url = server.resolvedUrls.local[0] + 'shell-setup-test'
    await page.goto(url + '?plugin=1'); await page.waitForLoadState('networkidle')
    assert.equal(calls.length, 0, 'plugin sessions never probe shell dependencies')
    await page.getByRole('button', { name: '切换普通聊天' }).click()
    await page.waitForLoadState('networkidle')
    assert.equal(calls.length, 0, 'ordinary chat sessions never probe dependencies either')
    await page.getByRole('button', { name: '切换代码会话' }).click()
    const dialog = page.getByRole('dialog', { name: '代码任务需要 PowerShell 7' })
    await dialog.waitFor()
    assert.ok(await dialog.getByText(command, { exact: true }).isVisible())
    if (process.env.SHELL_SETUP_SCREENSHOT) await page.screenshot({ path: process.env.SHELL_SETUP_SCREENSHOT, fullPage: true })
    assert.equal(started, false, 'merely opening setup cannot install software')
    await dialog.getByRole('button', { name: '打开商店页面', exact: true }).click()
    assert.equal(page.url(), url + '?plugin=1', 'Store opens externally, never by navigating the WebView')
    await dialog.getByRole('button', { name: '一键安装', exact: true }).click()
    await dialog.getByLabel('安装命令输出').waitFor()
    assert.match(await dialog.getByLabel('安装命令输出').textContent(), /正在下载 PowerShell 7/)
    assert.equal(await dialog.getByRole('button', { name: '正在安装…', exact: true }).isDisabled(), true)
    await dialog.getByRole('button', { name: '暂不安装', exact: true }).click()
    await page.getByRole('button', { name: '切换插件会话' }).click()
    const paused = calls.length
    await new Promise(resolve => setTimeout(resolve, 1100))
    assert.equal(calls.length, paused, 'install progress polling stops outside code sessions')
    await page.getByRole('button', { name: '切换代码会话' }).click()
    await dialog.getByRole('button', { name: '正在安装…', exact: true }).waitFor()
    finished = true
    await dialog.waitFor({ state: 'hidden' })
    assert.equal(await page.evaluate(() => window.shellReady), true)
    assert.ok(calls.some(call => call.path.endsWith('/store') && call.method === 'POST'))
    available = false; started = false; finished = false; failInstall = true
    await page.goto(url); await dialog.waitFor()
    await dialog.getByRole('button', { name: '一键安装', exact: true }).click()
    await dialog.getByRole('alert').filter({ hasText: 'winget missing' }).waitFor()
    assert.equal(await dialog.getByRole('button', { name: '一键安装', exact: true }).isEnabled(), true)
    await dialog.getByRole('button', { name: '我已安装 / 手动指定路径' }).click()
    await page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'setup fits a narrow viewport')
    await dialog.getByLabel('PowerShell 7 的 pwsh.exe 路径').fill('D:/custom/pwsh.exe')
    await dialog.getByRole('button', { name: '保存并重新检测' }).click()
    await dialog.waitFor({ state: 'hidden' })
    assert.ok(calls.some(call => call.path.endsWith('/detect') && call.body?.path === 'D:/custom/pwsh.exe'))
    available = false; pwshDetected = true
    await page.goto(url); await page.waitForLoadState('networkidle')
    assert.equal(await dialog.count(), 0, 'missing rg does not ask to reinstall detected PowerShell')
    pwshDetected = false; os = 'linux'
    await page.goto(url); await page.waitForLoadState('networkidle')
    assert.equal(await dialog.count(), 0, 'Linux never gets the PowerShell installer')
    assert.deepEqual(errors, [])
  } finally {
    if (browser) await browser.close()
    await server.close()
  }
})
