import assert from 'node:assert/strict'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'

test('flow canvas renders real frames and opens their details', { skip: !process.env.PLAYWRIGHT_MODULE }, async () => {
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE)
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    server: { port: 0, host: '127.0.0.1', hmr: false },
    plugins: [{ name: 'frame-ui-test', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url !== '/agent/frame-ui-test') return next()
        const html = await server.transformIndexHtml(req.url, `<!doctype html><html><head><meta charset="utf-8"><style>html,body,#root{height:100%;margin:0;font-family:sans-serif}</style></head><body><div id="root"></div><script type="module">
          import React from 'react'; import {createRoot} from 'react-dom/client';
          import {FlowView} from '/agent/src/components/FlowView.tsx'; import {CordisProvider} from '/agent/src/cordis/react.tsx';
          const base={parent_id:null,turn:1,description:'生成水果介绍',status:'done',call_site:null,concurrent:false,started_at:1,ended_at:1000,last_seq:1,error:null};
          const frames=[{...base,id:'root',caller_id:null,kind:'request',title:'创建 fruits.md',function:null},{...base,id:'apple',caller_id:'root',kind:'virtual',title:'generateApple',function:'generateApple',concurrent:true},{...base,id:'banana',caller_id:'root',kind:'virtual',title:'generateBanana',function:'generateBanana',concurrent:true}];
          const api={frames:async()=>({frames}),contextDebug:async()=>({enabled:false}),frame:async(_task,id)=>({frame:{...frames.find(f=>f.id===id),goal:'',parameters:[],arguments:[],captured:null,result:{result:'已生成'}},events:[],caller_program:null})};
          createRoot(document.getElementById('root')).render(React.createElement(CordisProvider,{context:{agentApi:api}},React.createElement(FlowView,{taskId:'test',running:false})));
        </script></body></html>`)
        res.setHeader('Content-Type', 'text/html'); res.end(html)
      })
    }}],
  })
  let browser
  try {
    await server.listen()
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {}) })
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    const errors = []
    page.on('pageerror', error => errors.push(String(error)))
    await page.goto(server.resolvedUrls.local[0] + 'frame-ui-test')
    await page.waitForFunction(() => document.querySelectorAll('.react-flow__node').length === 3)
    assert.equal(await page.locator('.react-flow__edge').count(), 2)
    await page.getByText('generateBanana', { exact: true }).click()
    await page.getByLabel('函数帧详情').getByText('已生成').waitFor()
    assert.deepEqual(errors, [])
  } finally {
    if (browser) await browser.close()
    await server.close()
  }
})
