import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { previewFile, previewKind, previewModule, previewRevision, studioUrl } from '../src/filePreview.ts'
import { buildInlineCodeDiff, findSessionFileChange } from '../src/filePreviewDiff.ts'

const base = '/agent/tasks/task%20id'
const workspaceCode = previewFile('task id', 'file:///work/src/main.ts', { line: 8 }, '/work')

test('preview dispatch, scoped attachment URLs and targeted refresh', () => {
  assert.equal(workspaceCode.line, 8)
  assert.equal(workspaceCode.url, base + '/workspace-file?path=src%2Fmain.ts')
  assert.equal(previewKind('x.MARKDOWN'), 'markdown')
  assert.equal(previewKind('x.svg'), 'image')
  assert.equal(previewKind('x.pptx'), 'plugin')
  assert.equal(previewKind('x.zip'), 'unsupported')
  assert.equal(previewKind('InOutController.java:50'), 'code')
  assert.equal(previewKind('Makefile'), 'code')
  const winColonFile = previewFile(
    'task id',
    '/C:/Users/29563/Desktop/Project3/warehouse-supervision-system/backend/InOutController.java:50',
    undefined,
    '\\\\?\\c:\\Users\\29563\\Desktop\\Project3\\warehouse-supervision-system',
  )
  assert.equal(winColonFile.path, 'backend/InOutController.java')
  assert.equal(winColonFile.name, 'InOutController.java')
  assert.equal(winColonFile.line, 50)
  assert.equal(previewFile('task id','x.pdf',{url:base+'/attachments/sha256%3Aa'}).attachment, true)
  for (const url of ['https://evil.example/agent/x','/agent/tasks/wrong/attachments/x']) {
    assert.equal(previewFile('task id','x.pdf',{url}), undefined)
  }
  const pkg = { id:'pdf', enabled:true, origin:'external', pages:[{id:'studio',renderer:'html'}], tools:[] }
  assert.equal(previewModule('report.pdf',[pkg]).slotId,'pdf')
  assert.equal(previewModule('report.pdf',[{...pkg,remote:true}]),undefined)
  assert.equal(new URL(studioUrl('pdf','studio','preview',workspaceCode.url,'main.ts','dark'),'http://localhost').searchParams.get('file'),workspaceCode.url)
  assert.equal(previewRevision(workspaceCode,[{type:'workspace/changes',seq:2,data:{turn:1,files:[{path:'/work/src/main.ts'}]}},{type:'turn/end',seq:3,data:{turn:1}}],'/work'),2)
  assert.equal(previewRevision(workspaceCode,[{type:'workspace/changes',seq:2,data:{turn:1,files:[{path:'other.ts'}]}},{type:'turn/end',seq:3,data:{turn:1}}],'/work'),0)
})

test('inline code diff tracks line numbers, hunks, and session change lookup across platforms', () => {
  const diff = buildInlineCodeDiff('line1\nold2\nline3\n', 'line1\nnew2a\nnew2b\nline3\n')
  assert.equal(diff.hasChanges, true)
  assert.equal(diff.added, 2)
  assert.equal(diff.deleted, 1)
  assert.deepEqual(diff.hunkIndices, [1])
  assert.deepEqual(
    diff.rows.map(r => [r.kind, r.oldLine, r.newLine, r.text]),
    [
      ['context', 1, 1, 'line1'],
      ['del', 2, null, 'old2'],
      ['add', null, 2, 'new2a'],
      ['add', null, 3, 'new2b'],
      ['context', 3, 4, 'line3'],
    ],
  )
  assert.equal(buildInlineCodeDiff('same\n', 'same\r\n').hasChanges, false)
  const created = buildInlineCodeDiff(null, 'a\nb\n')
  assert.equal(created.added, 2)
  assert.equal(created.deleted, 0)

  const change = findSessionFileChange(
    [
      { seq: 1, type: 'workspace/changes', data: { turn: 1, total: 1, files: [{ path: 'backend\\InOutController.java', added: 2, deleted: 1, reverted: false }] } },
      { seq: 2, type: 'workspace/changes', data: { turn: 2, total: 1, files: [{ path: 'backend/InOutController.java', added: 1, deleted: 0, reverted: false }] } },
    ],
    'backend/InOutController.java',
    'C:\\Users\\29563\\app',
  )
  assert.equal(change.earliestTurn, 1)
  assert.equal(change.latestTurn, 2)
  assert.equal(change.added, 3)
  assert.equal(change.deleted, 1)
})

const fallback = fileURLToPath(new URL('../../../ai-ppt-view/node_modules/playwright/index.mjs', import.meta.url))
const playwrightModule = process.env.PLAYWRIGHT_MODULE ?? (existsSync(fallback) ? fallback : undefined)
test('conversation files, attachment and changes open the resizable preview drawer', {skip: !playwrightModule}, async () => {
  const { chromium } = await import(playwrightModule)
  const server = await createServer({root:fileURLToPath(new URL('../',import.meta.url)),cacheDir:'node_modules/.vite-file-preview-test',server:{host:'127.0.0.1',port:0,hmr:false}, plugins:[{
    name:'file-preview-fixture',configureServer(server){
      server.middlewares.use(async (req,res,next)=>{
        if(req.url!=='/agent/file-preview-test')return next()
        res.setHeader('Content-Type','text/html')
        res.end(await server.transformIndexHtml(req.url,pageSource))
      })
    },
  }]})
  let browser
  try {
    await server.listen(0)
    browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE_PATH?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE_PATH}:{})})
    const page=await browser.newPage({viewport:{width:1500,height:850},permissions:['notifications']})
    page.setDefaultTimeout(8000)
    const errors=[]
    page.on('pageerror',error=>errors.push(String(error)))
    await page.route('**/agent/settings/shell',route=>route.fulfill({status:503}))
    await page.route('**/agent/skill-packages',route=>route.fulfill({json:{packages:[]}}))
    await page.route(url=>url.pathname==='/agent/tasks/original/workspace-file',route=>route.fulfill({body:Array.from({length:80},(_,i)=>'line '+(i+1)).join('\n')}))
    await page.route('**/agent/tasks/original/attachments/*',route=>route.fulfill({body:'# Document from attachment\n\n```mermaid\nflowchart LR\n  A --> B\n```'}))
    await page.goto(server.resolvedUrls.local[0]+'file-preview-test#/tasks/original')
    await page.getByRole('button',{name:/Review code/}).click().catch(async error => { throw new Error(String(error) + '\n' + await page.locator('body').innerText() + '\n' + errors.join('\n')) })
    const drawer=page.getByRole('complementary',{name:'会话文件抽屉'})
    await drawer.locator('[data-line="25"][data-highlighted]').waitFor().catch(async error => { throw new Error(String(error) + '\n' + await page.locator('body').innerText() + '\n' + errors.join('\n')) })
    assert.ok((await drawer.boundingBox()).width>=520)
    await page.getByRole('button',{name:'note.md',exact:true}).click()
    await drawer.getByRole('heading',{name:'Document from attachment'}).waitFor()
    await drawer.getByRole('img',{name:'Mermaid 图表'}).waitFor({timeout:15000})
    await drawer.getByRole('tab',{name:'main.ts'}).click()
    await drawer.locator('[data-line="25"][data-highlighted]').waitFor()
    await drawer.getByRole('button',{name:'关闭 main.ts'}).click()
    await drawer.getByRole('heading',{name:'Document from attachment'}).waitFor()
    await drawer.getByRole('button',{name:'变更',exact:true}).click()
    await drawer.getByText('各轮改动累计').waitFor()
    await drawer.getByRole('button',{name:'预览 src/main.ts'}).click()
    await drawer.getByRole('tabpanel',{name:'main.ts'}).waitFor()
    const handle=await page.getByRole('separator',{name:'调整预览抽屉宽度'}).boundingBox()
    await page.mouse.move(handle.x+handle.width/2,handle.y+100)
    await page.mouse.down();await page.mouse.move(handle.x-350,handle.y+100,{steps:6});await page.mouse.up()
    assert.ok((await drawer.boundingBox()).width>700)
    assert.deepEqual(errors,[])
  } finally {await browser?.close();await server.close()}
})

const pageSource = "<!doctype html><html lang=\"zh\"><head><meta charset=\"utf-8\"></head><body><div id=\"root\"></div><script type=\"module\">\nimport React from 'react';\nimport { createRoot } from 'react-dom/client';\nimport '/agent/src/dsh/theme/base.css';\nimport { ChatView } from '/agent/src/components/ChatView.tsx';\nimport { App } from '/agent/src/App.tsx';\nimport { Composer } from '/agent/src/components/Composer.tsx';\nimport { Sidebar } from '/agent/src/components/Sidebar.tsx';\nimport { CordisProvider } from '/agent/src/cordis/react.tsx';\n\nconst workspace = '/project';\nconst makeTask = (task_id, model, provider_name, permission_mode = 'ask') => ({\n  task_id, model, provider_name, permission_mode, workspace, kind: 'code',\n  status: 'completed', parent_task_id: null, prompt: 'Session ' + task_id,\n  created_at: Date.now(), updated_at: Date.now(),\n});\nconst rows = new Map([['original', makeTask('original', 'gpt-6.2-sol', 'primary', 'full_access')]]);\nconst provider = (id, display_name, model) => ({ id, display_name, models: [model], model_capabilities: {} });\nconst settings = {\n  orchestrator_provider: 'local', orchestrator_model: 'qwen3.8-27b', revision: 'initial',\n  providers: [provider('local', 'Local', 'qwen3.8-27b'), provider('primary', 'Primary', 'gpt-6.2-sol'), provider('secondary', 'Secondary', 'gpt-6.2-sol')],\n};\nwindow.sent = [];\nwindow.settingsSavePending = false;\nconst agentApi = {\n  info: async () => ({ ready: true, workspace, model: settings.orchestrator_model }),\n  getSettings: async () => settings,\n  saveSettings: () => { window.settingsSavePending = true; return new Promise(() => {}); },\n  projects: async () => [{ path: workspace, title: 'Project' }],\n  listTasks: async () => [...rows.values()],\n  getTask: async id => ({ ...rows.get(id) }),\n  events: async () => [\n    {seq:1,time:1,type:'turn/start',data:{turn:1}},\n    {seq:2,time:2,type:'user/message',data:{turn:1,task_id:'original',message:{role:'user',content:[{type:'text',text:'See @src/main.ts'},{type:'file',attachment:{name:'note.md',attachmentId:'sha256:abc'}}]}}},\n    {seq:3,time:3,type:'assistant/message',data:{turn:1,step:1,message:{role:'assistant',content:'[Review code](src/main.ts#L25)'}}},\n    {seq:4,time:4,type:'workspace/changes',data:{turn:1,total:1,files:[{path:'src/main.ts',added:1,deleted:0}]}},\n    {seq:5,time:5,type:'turn/end',data:{turn:1,reason:{kind:'completed'}}}\n  ],\n  commands: async () => ({ commands: [] }),\n  files: async () => ({ files: [] }),\n  updateTask: async (id, input) => {\n    const task = { ...rows.get(id), ...input, provider_name: input.provider };\n    rows.set(id, task);\n    return task;\n  },\n  createSession: async () => {\n    const id = 'new-' + rows.size;\n    rows.set(id, makeTask(id, settings.orchestrator_model, settings.orchestrator_provider));\n    return id;\n  },\n  prompt: async (id, _text, input) => {\n    window.sent.push({ id, model: input.model, provider: input.provider, permission: input.permission_mode });\n    rows.set(id, makeTask(id, input.model, input.provider, input.permission_mode));\n  },\n};\nconst snapshot={packages:[],pages:[],errors:{}};\nconst pluginPages={subscribe:()=>()=>{},getSnapshot:()=>snapshot};\nconst slots = { 'agent.chat': ChatView, 'agent.sidebar': Sidebar, 'agent.composer': Composer };\nconst renderSlot = (name, props) => slots[name] ? React.createElement(slots[name], props) : null;\ncreateRoot(document.getElementById('root')).render(\n  React.createElement(CordisProvider, { context: { agentApi, pluginPages } }, React.createElement(App, { renderSlot })));\n</script></body></html>"
