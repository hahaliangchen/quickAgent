import { test, expect } from '@playwright/test'
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { setTimeout as delay } from 'node:timers/promises'

const repo = fileURLToPath(new URL('../../', import.meta.url))
let fixture, directory, project, base

test.beforeEach(async () => {
  test.setTimeout(240_000)
  directory = await mkdtemp(join(tmpdir(), 'quickagent-blueprint-e2e-'))
  fixture = spawn('cargo', ['test', '--bin', 'codex-workspace-mcp', 'blueprint_playwright_fixture', '--', '--ignored', '--nocapture'], {
    cwd: repo, env: { ...process.env, GROW_CONFIG_DIR: directory, GROW_RESOURCES_DIR: repo, AGENT_WEB_DIR: join(repo, 'frontend', 'dist') },
    stdio: ['ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32',
  })
  const ready = await new Promise((resolve, reject) => {
    let output = ''
    const read = chunk => {
      output = (output + chunk).slice(-10000)
      const marker = output.match(/BLUEPRINT_E2E_READY (\{.*\})\r?\n/)
      if (marker) resolve(JSON.parse(marker[1]))
    }
    fixture.stdout.setEncoding('utf8').on('data', read)
    fixture.stderr.setEncoding('utf8').on('data', read)
    fixture.once('error', reject)
    fixture.once('exit', code => reject(new Error('Fixture exited ' + code + '\n' + output)))
  })
  project = ready.root
  base = ready.url
  for (let attempt = 0; attempt < 80; attempt++) {
    if (await fetch(base + '/agent/').then(r => r.ok, () => false)) return
    await delay(100)
  }
  throw new Error('Fixture did not serve the built frontend')
})

test.afterEach(async () => {
  if (fixture?.pid && fixture.exitCode === null && fixture.signalCode === null) {
    const stopped = new Promise(resolve => fixture.once('exit', resolve))
    try { process.kill(process.platform === 'win32' ? fixture.pid : -fixture.pid, 'SIGTERM') } catch (e) { if (e.code !== 'ESRCH') throw e }
    await Promise.race([stopped, delay(2000)])
  }
  if (project) await rm(project, { recursive: true, force: true })
  if (directory) await rm(directory, { recursive: true, force: true })
  fixture = directory = project = base = undefined
})

async function drag(page, from, to) {
  const fromHandle = await from.elementHandle(), toHandle = await to.elementHandle()
  const ready = await page.waitForFunction(async ([source, target]) => {
    const point = element => {
      const { x, y, width, height } = element.getBoundingClientRect()
      return { x: x + width / 2, y: y + height / 2 }
    }
    const first = [point(source), point(target)]
    for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame)
    const second = [point(source), point(target)]
    return second.every(({ x, y }, i) => {
      const initial = first[i]
      const handle = [source, target][i]
      return Math.hypot(initial.x - x, initial.y - y) < .1 && document.elementFromPoint(x, y) === handle
    }) && second
  }, [fromHandle, toHandle])
  const [a, b] = await ready.jsonValue()
  await page.mouse.move(a.x, a.y)
  await page.mouse.down()
  await page.mouse.move(b.x, b.y, { steps: 24 })
  await page.mouse.up()
}

for (const theme of ['light', 'dark']) {
  test(theme + ': real canvas, layout, source and chat draft', async ({ page }, testInfo) => {
    const errors = []
    page.setDefaultTimeout(8000)
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', entry => { if (entry.type() === 'error') errors.push(entry.text()) })
    const rail = page.getByRole('button', { name: '架构蓝图', exact: true })
    const workbench = page.getByTestId('blueprint-workbench')
    const btn = name => workbench.getByRole('button', { name, exact: true })
    const editor = workbench.getByRole('dialog', { name: '实体检查器' })
    const node = id => workbench.getByTestId('node-' + id)
    const edgeLabel = btn(/POST \/orders/)
    await page.goto(base + '/agent/')
    await rail.click()
    await expect(workbench).toBeVisible()
    if (await workbench.getAttribute('data-theme') !== theme) await page.getByRole('button', { name: theme === 'dark' ? '切换为深色' : '切换为浅色', exact: true }).click()
    await expect(workbench).toHaveAttribute('data-theme', theme)
    await expect(workbench.getByText('此项目尚未建立蓝图')).toBeVisible()
    await expect(stat(join(project, '.quickagent', 'blueprint'))).rejects.toMatchObject({ code: 'ENOENT' })
    await btn('手动新建空白蓝图').click()
    await workbench.getByLabel('画布名称').fill('订单录入 ' + theme)
    await btn('创建画布').click()
    await expect(workbench.getByText('空白画布', { exact: true })).toBeVisible()
    async function addEntity(id, name, kind, ports) {
      await btn('+ 新建节点').click()
      await editor.getByLabel('实体名称').fill(name)
      await editor.getByLabel('实体 ID').fill(id)
      await editor.getByLabel('实体类型').selectOption(kind)
      if (id === 'ui') await editor.getByLabel('源码路径').fill('app.ts')
      for (const [index, [label, direction, symbol]] of ports.entries()) {
        await editor.getByRole('button', { name: '+ 添加端口', exact: true }).click()
        const field = editor.getByLabel('端口名称 ' + (index + 1), { exact: true })
        const group = field.locator('xpath=../..')
        await field.fill(label)
        await group.getByLabel('方向').selectOption(direction)
        if (symbol) { await group.getByLabel('符号').fill(symbol); await group.getByLabel('起始行').fill('16') }
      }
      await editor.getByRole('button', { name: '保存实体' }).click()
      await expect(node(id)).toBeVisible()
    }
    await addEntity('ui', '订单表单', 'trigger_ui', [['提交订单', 'out', 'onSubmit'], ['校验表单', 'both']])
    await addEntity('service', '订单服务', 'service', [['接收订单', 'in'], ['保存订单', 'out']])
    await workbench.getByRole('button', { name: /fit view/i }).click()
    await drag(page, node('ui').getByRole('button', { name: /提交订单/ }).locator('..').locator('.source'), node('service').getByRole('button', { name: /接收订单/ }).locator('..').locator('.target'))
    const edge = workbench.getByRole('dialog', { name: '连线检查器' })
    await edge.getByLabel('连线关系').selectOption('http')
    await edge.getByLabel('严重级别').selectOption('critical')
    await edge.getByLabel('连线标签').fill('POST /orders')
    await edge.getByLabel('连线红线').fill('必须先校验订单')
    await edge.getByRole('button', { name: '保存连线' }).click()
    await expect(edgeLabel).toBeVisible()
    await btn('编辑画布和红线').click()
    await workbench.getByLabel('画布红线（每行一条）').fill('必须保留幂等检查')
    await btn('保存画布').click()
    await expect(workbench.getByText('必须保留幂等检查')).toBeVisible()
    const storage = join(project, '.quickagent', 'blueprint')
    const manifest = JSON.parse(await readFile(join(storage, 'manifest.json'), 'utf8'))
    const readGraph = async () => JSON.parse(await readFile(join(storage, 'graphs', manifest.graphs[0].id + '.json'), 'utf8'))
    const position = graph => graph.nodes.find(n => n.entity_id === 'ui').x
    const first = await readGraph()
    const rect = await node('ui').locator('header strong').boundingBox()
    await page.mouse.move(rect.x + 16, rect.y + 6)
    await page.mouse.down()
    await page.mouse.move(rect.x + 95, rect.y + 75, { steps: 16 })
    await page.mouse.up()
    await expect.poll(async () => position(await readGraph())).not.toBe(position(first))
    const arrangedResponse = page.waitForResponse(r => r.url().includes('/layout?') && r.request().method() === 'PUT' && r.ok())
    await btn('自动排版').click()
    await arrangedResponse
    const arranged = await readGraph()
    expect(position(arranged)).toBeLessThan(arranged.nodes.find(n => n.entity_id === 'service').x)
    expect(arranged.edges).toMatchObject([{ label: 'POST /orders', invariant: '必须先校验订单', severity: 'critical' }])
    expect(arranged.invariants).toEqual(['必须保留幂等检查'])
    await page.reload()
    await rail.click()
    await expect(node('service')).toBeVisible()
    await expect(edgeLabel).toBeVisible()
    expect(await readGraph()).toEqual(arranged)
    for (const item of arranged.nodes) {
      await expect(workbench.locator('.react-flow__node[data-id="' + item.entity_id + '"]')).toHaveCSS('transform', 'matrix(1, 0, 0, 1, ' + item.x + ', ' + item.y + ')')
    }
    await node('ui').getByRole('button', { name: /提交订单/ }).click()
    const source = workbench.getByRole('region', { name: '源码预览' })
    await expect(source).toContainText('blueprint fixture')
    await expect(source).toContainText('app.ts:12–16')
    await expect(source.locator('[data-line-numbers]')).toBeAttached()
    const fitsCanvas = () => expect.poll(async () => {
      const canvas = await workbench.getByTestId('blueprint-canvas').boundingBox()
      const nodes = await Promise.all([node('ui').boundingBox(), node('service').boundingBox()])
      return nodes.every(card => card && canvas && card.x >= canvas.x && card.x + card.width <= canvas.x + canvas.width)
    }).toBe(true)
    await fitsCanvas()
    await workbench.screenshot({ path: testInfo.outputPath('source.png') })
    await btn('关闭检查器').click()
    await fitsCanvas()
    await workbench.screenshot({ path: testInfo.outputPath('canvas.png') })
    await edgeLabel.click()
    await btn('发给 AI 讨论').click()
    const chat = page.getByRole('region', { name: '代码问答' }).locator('textarea')
    await expect(chat).toHaveValue(/graph_id[\s\S]*必须先校验订单[\s\S]*必须保留幂等检查/)
    await expect(chat).not.toHaveValue(/请诊断以下终端输出/)
    await chat.fill('')
    await btn('启动分析').click()
    await expect(chat).toHaveValue(/项目形态[\s\S]*blueprint\.upsert_graph/)
    expect(errors).toEqual([])
  })
}

