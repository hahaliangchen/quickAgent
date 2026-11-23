import { test, expect } from '@playwright/test'
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { setTimeout as delay } from 'node:timers/promises'

let fixture, directory, url, sshPort
const root = fileURLToPath(new URL('../../', import.meta.url))

test.beforeAll(async () => {
  test.setTimeout(240_000)
  directory = await mkdtemp(join(tmpdir(), 'grow-ssh-e2e-'))
  fixture = spawn('cargo', ['test', '--bin', 'codex-workspace-mcp', 'builtin_plugins::ssh::integration_tests::playwright_fixture', '--', '--ignored', '--nocapture'], {
    cwd: root, env: { ...process.env, GROW_CONFIG_DIR: directory, GROW_RESOURCES_DIR: root, AGENT_WEB_DIR: join(root, 'frontend', 'dist') },
    stdio: ['ignore', 'pipe', 'pipe'], detached: true,
  })
  await new Promise((resolve, reject) => {
    let output = ''
    const read = chunk => {
      output = (output + chunk).slice(-8000)
      const ready = output.match(/SSH_E2E_READY (http:\/\/127\.0\.0\.1:\d+) (\d+)/)
      if (ready) { url = ready[1]; sshPort = ready[2]; resolve() }
    }
    fixture.stdout.setEncoding('utf8').on('data', read)
    fixture.stderr.setEncoding('utf8').on('data', read)
    fixture.once('error', reject)
    fixture.once('exit', code => reject(new Error('SSH fixture exited: ' + code + '\n' + output)))
  })
  for (let retry = 0; retry < 100; retry++) {
    if (await fetch(url + '/agent/').then(response => response.ok, () => false)) return
    await delay(100)
  }
  throw new Error('The agent did not serve the compiled frontend')
})

test.afterAll(async () => {
  if (fixture?.pid && fixture.exitCode === null) {
    const stopped = new Promise(resolve => fixture.once('exit', resolve))
    try { process.kill(-fixture.pid, 'SIGTERM') } catch (error) { if (error.code !== 'ESRCH') throw error }
    await Promise.race([stopped, delay(2000)])
  }
  if (directory) await rm(directory, { recursive: true, force: true })
})

test('protected SSH profile, real PTY, SFTP drag/drop and session history', async ({ page, request }, testInfo) => {
  const errors = []
  page.setDefaultTimeout(7000)
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', entry => { if (entry.type() === 'error') errors.push(entry.text()) })
  await page.goto(url + '/agent/')
  const newSession = await request.post(url + '/agent/sessions', { data: { kind: 'chat' } })
  expect(newSession.ok(), await newSession.text()).toBeTruthy()
  const { task_id: id } = await newSession.json()
  const renamed = await request.patch(url + '/agent/sessions/' + id, { data: { title: 'SSH Playwright 会话' } })
  expect(renamed.ok()).toBeTruthy()
  await page.goto(url + '/agent/#/tasks/' + encodeURIComponent(id))
  await expect(page).toHaveURL(/#\/tasks\//)
  await page.getByRole('button', { name: '远程终端', exact: true }).click()
  const workbench = page.getByTestId('ssh-workbench')
  await expect(workbench).toBeVisible()
  await workbench.getByRole('button', { name: /添加服务器/ }).click()
  const create = page.getByRole('dialog', { name: '添加服务器' })
  await create.getByLabel('名称', { exact: true }).fill('Loopback SSH')
  await create.getByLabel('主机', { exact: true }).fill('127.0.0.1')
  await create.getByLabel('端口', { exact: true }).fill(sshPort)
  await create.getByLabel('用户名', { exact: true }).fill('tester')
  await create.getByLabel('认证方式').selectOption('password')
  await create.getByLabel('密码', { exact: true }).fill('loopback-test-password')
  await create.getByLabel('环境').selectOption('prod')
  await expect(create.getByLabel('终端与写入操作需确认')).toBeChecked()
  await create.getByRole('button', { name: '保存', exact: true }).click()
  await expect(create).toBeHidden()
  await workbench.getByRole('button', { name: '测试连接' }).click()
  await expect(workbench.getByText('连接与认证通过')).toBeVisible()
  page.once('dialog', dialog => dialog.dismiss())
  await workbench.getByRole('button', { name: /新建终端/ }).click()
  await expect(workbench.getByRole('tab')).toHaveCount(0)
  page.once('dialog', dialog => dialog.accept())
  await workbench.getByRole('button', { name: /新建终端/ }).click()
  await expect(workbench.getByText('已连接', { exact: true })).toBeVisible()
  const terminal = page.getByTestId('ssh-terminal')
  await expect(terminal.getByText(/local fixture ready/).first()).toBeAttached()
  await terminal.locator('textarea').focus()
  await page.keyboard.type('printf fixture-ok')
  await page.keyboard.press('Enter')
  await expect(terminal.getByText(/^\$ fixture-ok$/).first()).toBeAttached()
  await workbench.getByRole('button', { name: '打开远程目录' }).click()
  const sftp = page.getByTestId('ssh-sftp-drop')
  await expect(sftp.getByRole('button', { name: 'nested', exact: true })).toBeVisible()
  await sftp.getByRole('button', { name: 'hello.txt', exact: true }).click()
  await expect(page.getByRole('dialog', { name: '文件预览' }).getByText('SFTP fixture text')).toBeVisible()
  await page.getByRole('button', { name: '关闭预览' }).click()
  const drop = await page.evaluateHandle(() => {
    const payload = new DataTransfer()
    payload.items.add(new File(['browser fixture upload\n'], 'browser-upload.txt', { type: 'text/plain' }))
    return payload
  })
  page.once('dialog', dialog => dialog.accept())
  await sftp.dispatchEvent('drop', { dataTransfer: drop })
  await expect(sftp.getByRole('button', { name: 'browser-upload.txt', exact: true })).toBeVisible()
  const downloadPromise = page.waitForEvent('download')
  await sftp.getByRole('link', { name: '下载 browser-upload.txt' }).click()
  const download = await downloadPromise
  expect(download.suggestedFilename()).toBe('browser-upload.txt')
  expect(await readFile(await download.path(), 'utf8')).toBe('browser fixture upload\n')
  await expect(workbench.getByRole('button', { name: 'SSH Playwright 会话' }).first()).toBeVisible()
  await workbench.screenshot({ path: testInfo.outputPath('desktop.png') })
  await page.setViewportSize({ width: 590, height: 850 })
  await expect(sftp.getByRole('button', { name: 'browser-upload.txt', exact: true })).toBeVisible()
  await workbench.screenshot({ path: testInfo.outputPath('narrow.png') })
  expect(await workbench.evaluate(element => element.scrollWidth <= element.clientWidth + 1)).toBeTruthy()
  await workbench.getByRole('button', { name: 'SSH Playwright 会话' }).first().click()
  await expect(workbench).toBeHidden()
  await expect(page).toHaveURL(url + '/agent/#/tasks/' + encodeURIComponent(id))
  expect(errors).toEqual([])
})

test('follows only new SSH calls and keeps the linked chat, SFTP and independent terminals in sync', async ({ page, request }, testInfo) => {
  await page.setViewportSize({ width: 1500, height: 920 })
  const created = await request.post(url + '/agent/ssh/servers', {
    headers: { 'sec-fetch-site': 'same-origin' },
    data: { name: 'Follow SSH', host: '127.0.0.1', port: Number(sshPort), username: 'tester', auth_type: 'password', credential: 'loopback-test-password', env_tier: 'prod', require_approval: true },
  })
  expect(created.ok(), await created.text()).toBeTruthy()
  const { id: serverId } = await created.json()
  const session = await request.post(url + '/agent/sessions', { data: { kind: 'chat' } })
  expect(session.ok()).toBeTruthy()
  const { task_id: taskId } = await session.json()
  const endpoint = '/agent/tasks/' + encodeURIComponent(taskId)
  const current = await request.get(url + endpoint)
  expect(current.ok()).toBeTruthy()
  const task = { ...await current.json(), title: 'SSH 上线排查', status: 'running' }
  const event = (seq, type, data) => ({ seq, type, data, time: Date.now() })
  const call = (seq, turn, name, params) => event(seq, 'tool/call', { turn, callId: 'call-' + seq, name, arguments: JSON.stringify({ server_id: serverId, ...params }) })
  const events = [event(1, 'turn/start', { turn: 1 }), call(2, 1, 'ssh__exec', { command: 'historical SSH command' })]
  await page.route(url + endpoint, route => route.fulfill({ json: task }))
  await page.route(url + endpoint + '/events', route => route.fulfill({ json: { events } }))
  await page.addInitScript(({ endpoint }) => {
    const RealEventSource = window.EventSource
    window.EventSource = class extends EventTarget {
      constructor(path, options) {
        super()
        if (!String(path).startsWith(endpoint + '/stream?')) return new RealEventSource(path, options)
        window.sshTestStream = this
        queueMicrotask(() => this.onopen?.(new Event('open')))
      }
      close() { if (window.sshTestStream === this) window.sshTestStream = undefined }
    }
  }, { endpoint })
  const push = async (...batch) => {
    events.push(...batch)
    await page.evaluate(batch => {
      for (const item of batch) window.sshTestStream.dispatchEvent(new MessageEvent('session/event', { data: JSON.stringify(item) }))
    }, batch)
  }
  let broadcast, aiInputs = []
  let aiTickets = 0, humanTickets = 0
  page.on('request', req => {
    if (req.url().endsWith('/ai-stream-ticket')) { aiTickets++; expect(req.postDataJSON()).toEqual({ task_id: taskId }) }
    if (req.url().endsWith('/terminal-ticket')) humanTickets++
  })
  await page.routeWebSocket(/\/ai-stream\?ticket=/, socket => {
    broadcast = socket
    socket.onMessage(frame => aiInputs.push(frame))
    socket.send(JSON.stringify({ type: 'ready' }))
    socket.send(Buffer.from('[AI · /nested] $ echo live-fixture\r\nlive-fixture\r\n'))
  })
  await page.goto(url + '/agent/#/tasks/' + encodeURIComponent(taskId))
  await page.waitForFunction(() => !!window.sshTestStream)
  const workbench = page.getByTestId('ssh-workbench')
  await expect(workbench).toBeHidden()
  expect(aiTickets).toBe(0)
  await push(event(3, 'turn/start', { turn: 2 }),
    event(4, 'user/message', { turn: 2, message: { role: 'user', content: '当前部署检查' } }),
    call(5, 2, 'ssh__exec', { command: 'echo live-fixture', cwd: '/nested' }),
    event(6, 'approval/request', { call_id: 'call-5', tool: 'ssh__exec', once_only: true, arguments: '{"command":"echo live-fixture"}' }))
  await expect(workbench).toBeVisible()
  const aiTab = workbench.getByRole('tab', { name: 'Follow SSH · AI 实时' })
  await expect(aiTab).toHaveAttribute('aria-selected', 'true')
  const aiPane = workbench.getByRole('region', { name: 'AI 实时终端 · 只读' })
  const aiTerminal = aiPane.getByTestId('ssh-ai-terminal')
  await expect(aiTerminal.getByText(/live-fixture/).first()).toBeAttached()
  expect(humanTickets).toBe(0)
  const sftp = workbench.getByRole('region', { name: 'SFTP 文件' })
  await expect(sftp.getByRole('navigation', { name: '远程目录' }).locator('[aria-current="page"]')).toHaveText('nested')
  const dock = page.getByRole('region', { name: '代码问答' })
  await expect(dock.getByText('SSH 上线排查', { exact: true })).toBeVisible()
  await expect(dock.getByText('当前部署检查', { exact: true })).toBeVisible()
  const approval = dock.getByRole('alertdialog')
  await expect(approval.getByRole('button', { name: '允许这一次' })).toBeVisible()
  await expect(approval.getByRole('button', { name: '本会话都允许' })).toHaveCount(0)
  const interjected = page.waitForRequest(url + endpoint + '/interjections')
  await page.route(url + endpoint + '/interjections', route => route.fulfill({ json: { seq: 7 } }))
  await dock.locator('textarea').first().fill('请确认上线范围')
  await dock.getByRole('button', { name: '插话', exact: true }).click()
  expect((await interjected).postDataJSON()).toEqual({ text: '请确认上线范围' })
  await page.waitForFunction(() => !!window.sshTestStream)
  await aiTerminal.locator('textarea').focus()
  await page.keyboard.type('input-must-not-cross')
  await page.keyboard.press('Enter')
  expect(aiInputs).toEqual([])
  await workbench.screenshot({ path: testInfo.outputPath('ai-linked.png') })
  expect(await workbench.evaluate(element => element.scrollWidth <= element.clientWidth + 1)).toBeTruthy()

  page.once('dialog', dialog => dialog.accept())
  await aiPane.getByRole('button', { name: /打开人工终端/ }).click()
  const humanTab = workbench.getByRole('tab', { name: 'Follow SSH · 1' })
  await expect(humanTab).toHaveAttribute('aria-selected', 'true')
  const human = workbench.getByTestId('ssh-terminal')
  await expect(human.getByText(/local fixture ready/).first()).toBeAttached()
  await human.locator('textarea').focus()
  await page.keyboard.type('printf fixture-ok')
  await page.keyboard.press('Enter')
  await expect(human.getByText(/^\$ fixture-ok$/).first()).toBeAttached()
  expect(aiInputs).toEqual([])
  await expect(aiTerminal.getByText(/fixture-ok/)).toHaveCount(0)
  const refresh = page.waitForRequest(req => req.url().includes('/sftp/list?path=%2Fnested'))
  broadcast.send(JSON.stringify({ type: 'activity', action: 'exec', remote_path: '/nested', detail: 'exit 0' }))
  await refresh
  await expect(humanTab).toHaveAttribute('aria-selected', 'true')

  await dock.getByTitle('在主界面打开这个对话').click()
  await expect(workbench).toBeHidden()
  await expect(page).toHaveURL(url + '/agent/#/tasks/' + encodeURIComponent(taskId))
  const followRoot = page.waitForRequest(req => req.url().includes('/sftp/list?path=%2F') && !req.url().includes('%2Fnested'))
  await push(call(8, 2, 'ssh__read_file', { path: '/hello.txt' }))
  await followRoot
  await expect(workbench).toBeHidden()
  await page.getByRole('button', { name: '远程终端', exact: true }).click()
  await expect(aiTab).toHaveAttribute('aria-selected', 'true')
  await expect(sftp.getByRole('button', { name: 'hello.txt', exact: true })).toBeVisible()
  expect(aiTickets).toBe(1)
  expect(humanTickets).toBe(1)
  await expect(workbench.getByRole('tab')).toHaveCount(2)
  await humanTab.click()
  await expect(human.getByText(/^\$ fixture-ok$/).first()).toBeAttached()

  const following = workbench.getByRole('checkbox', { name: '跟随 AI 操作' })
  await following.uncheck()
  await page.getByTitle('首页 / 新会话').click()
  await push(event(9, 'turn/start', { turn: 3 }), call(10, 3, 'ssh__list_dir', { path: '/nested' }))
  await expect(workbench).toBeHidden()
  await page.getByRole('button', { name: '远程终端', exact: true }).click()
  await expect(following).not.toBeChecked()
  await following.check()
  await page.getByTitle('首页 / 新会话').click()
  await push(event(11, 'turn/start', { turn: 4 }), call(12, 4, 'ssh__list_dir', { path: '/nested' }))
  await expect(workbench).toBeVisible()
  await expect(aiTab).toHaveAttribute('aria-selected', 'true')
  await expect(sftp.getByRole('navigation', { name: '远程目录' }).locator('[aria-current="page"]')).toHaveText('nested')
  expect(aiTickets).toBe(1)
  expect(humanTickets).toBe(1)
})
