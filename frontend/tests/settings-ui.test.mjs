import assert from 'node:assert/strict'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'

test('provider editing preserves drafts and saves the configured values', { skip: !process.env.PLAYWRIGHT_MODULE }, async () => {
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE)
  const server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    server: { port: 0, host: '127.0.0.1', hmr: false },
    plugins: [{ name: 'settings-ui-test', configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (req.url?.split('?')[0] !== '/agent/settings-ui-test') return next()
        const html = await server.transformIndexHtml(req.url, `<!doctype html><html><head><meta charset="utf-8"></head><body><div id="root"></div><script type="module">
          import React, {useState} from 'react'; import {createRoot} from 'react-dom/client';
          import '/agent/src/dsh/theme/base.css'; import '/agent/src/dsh/theme/design-platform.css';
          import {SettingsModal} from '/agent/src/components/SettingsModal.tsx'; import {api} from '/agent/src/api.ts';
          const provider = id => ({id,display_name:id,url:'https://example.com/v1',api_type:'openai-completions',models:['model-a'],model_capabilities:{},has_api_key:true});
          window.reads=0; window.saved=[]; window.notifications=0;
          window.settings={revision:'initial',providers:new URLSearchParams(location.search).has('single')?[provider('first')]:[provider('first'),provider('second')],plugin_models:{},orchestrator_provider:'first',orchestrator_model:'model-a',reasoning_effort:'',fast_mode:false};
          if(new URLSearchParams(location.search).has('local'))window.settings={...window.settings,providers:[{...provider('deepseek'),url:'https://api.deepseek.com/v1',has_api_key:false}],web_search_provider:'deepseek',orchestrator_provider:'deepseek'};
          api.getSettings=async()=>{window.reads++;const result=structuredClone(window.settings);if(window.delayLoad)await new Promise(resolve=>window.resolveLoad=resolve);return result};
          api.getPlugins=async()=>({plugins:[],events:[]}); api.skillPackages=async()=>({packages:[]});
          api.saveSettings=async payload=>{
            window.saved.push(structuredClone(payload));
            window.settings={...payload,revision:'saved',providers:payload.providers.map(({api_key,clear_api_key,...p})=>({...p,has_api_key:!clear_api_key}))};
            if(window.delaySave)await new Promise(resolve=>window.resolveSave=resolve);
            return structuredClone(window.settings);
          };
          function App(){const [open,setOpen]=useState(true);const [tab,setTab]=useState('models');return React.createElement(React.Fragment,null,
            React.createElement('button',{onClick:()=>setOpen(true)},'打开设置'),
            React.createElement(SettingsModal,{open,activeTab:tab,workspacePath:'/test',onSelectTab:setTab,onClose:()=>setOpen(false),onThemeChanged:()=>{},onSaved:()=>window.notifications++}));}
          createRoot(document.getElementById('root')).render(React.createElement(App));
        </script></body></html>`)
        res.setHeader('Content-Type', 'text/html'); res.end(html)
      })
    }}],
  })
  let browser
  try {
    await server.listen()
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {}) })
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } })
    const errors = []
    page.on('pageerror', error => errors.push(String(error)))
    const settle = () => page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
    const url = server.resolvedUrls.local[0] + 'settings-ui-test'
    await page.goto(url)
    await page.getByRole('button', { name: '编辑', exact: true }).first().click()
    await settle()
    assert.equal(await page.evaluate(() => window.reads), 1, 'expanding a provider must not reload settings')
    await page.getByPlaceholder('例如 DeepSeek Official').fill('Edited provider')
    await page.getByPlaceholder('https://api.deepseek.com/v1', { exact: true }).fill('https://edited.example/v1')
    await page.getByPlaceholder('已配置（留空保持不变）').fill('test-key')
    await page.locator('textarea').fill('model-a\nmodel-b')
    await page.getByRole('button', { name: '编辑', exact: true }).click()
    await page.getByRole('button', { name: '编辑', exact: true }).click()
    await settle()
    assert.equal(await page.getByPlaceholder('例如 DeepSeek Official').inputValue(), 'Edited provider')
    assert.equal(await page.locator('textarea').inputValue(), 'model-a\nmodel-b')
    await page.getByRole('button', { name: '添加模型提供商', exact: true }).click()
    await settle()
    assert.equal(await page.getByPlaceholder('例如 deepseek', { exact: true }).inputValue(), 'provider-1')
    await page.getByPlaceholder('例如 DeepSeek Official').fill('New provider')
    await page.getByRole('button', { name: '专用模型', exact: true }).click()
    await page.getByRole('button', { name: '模型与提供商', exact: true }).click()
    await page.getByRole('button', { name: '保存更改', exact: true }).click()
    await page.getByText('设置已保存并立即对新任务生效。', { exact: true }).waitFor()
    const { reads, saved, notifications } = await page.evaluate(() => ({ reads: window.reads, saved: window.saved, notifications: window.notifications }))
    assert.equal(reads, 1)
    assert.equal(notifications, 1)
    assert.equal(saved.length, 1)
    assert.equal(saved[0].providers.length, 3)
    assert.equal(saved[0].providers[0].display_name, 'Edited provider')
    assert.equal(saved[0].providers[0].url, 'https://edited.example/v1')
    assert.equal(saved[0].providers[0].api_key, 'test-key')
    assert.deepEqual(saved[0].providers[0].models, ['model-a', 'model-b'])
    assert.equal(saved[0].providers[2].display_name, 'New provider')
    await page.getByRole('button', { name: '关闭设置', exact: true }).click()
    await page.getByRole('button', { name: '打开设置', exact: true }).click()
    await page.getByRole('button', { name: '编辑', exact: true }).first().click()
    await settle()
    assert.equal(await page.getByPlaceholder('例如 DeepSeek Official').inputValue(), 'Edited provider')
    assert.equal(await page.evaluate(() => window.reads), 2, 'reopening settings loads the saved configuration')

    await page.goto(url + '?single')
    await page.getByRole('button', { name: '收起', exact: true }).waitFor()
    await settle()
    assert.equal(await page.evaluate(() => window.reads), 1, 'auto-expanding the only provider must not load twice')
    await page.getByRole('button', { name: '收起', exact: true }).click()
    await settle()
    assert.equal(await page.getByRole('button', { name: '编辑', exact: true }).count(), 1, 'the only provider stays collapsed')
    assert.equal(await page.evaluate(() => window.reads), 1)

    // A save cannot silently replace edits entered while the request is pending.
    await page.getByRole('button', { name: '编辑', exact: true }).click()
    await page.getByPlaceholder('例如 DeepSeek Official').fill('Pending save')
    await page.evaluate(() => { window.delaySave = true })
    await page.getByRole('button', { name: '保存更改', exact: true }).click()
    await page.waitForFunction(() => typeof window.resolveSave === 'function')
    assert.equal(await page.getByPlaceholder('例如 DeepSeek Official').isDisabled(), true)
    // Closing/reopening starts a separate editing session; the old save is stale.
    await page.getByRole('button', { name: '关闭设置', exact: true }).click()
    await page.getByRole('button', { name: '打开设置', exact: true }).click()
    await page.getByPlaceholder('例如 DeepSeek Official').fill('New unsaved draft')
    await page.evaluate(() => { window.resolveSave() })
    await settle()
    assert.equal(await page.getByPlaceholder('例如 DeepSeek Official').inputValue(), 'New unsaved draft')
    assert.equal(await page.getByRole('button', { name: '保存更改', exact: true }).isEnabled(), true)
    assert.equal(await page.evaluate(() => window.notifications), 0)

    // A late load from a previous opening cannot overwrite the current draft.
    await page.getByRole('button', { name: '关闭设置', exact: true }).click()
    await page.evaluate(() => { window.delayLoad = true })
    await page.getByRole('button', { name: '打开设置', exact: true }).click()
    await page.waitForFunction(() => typeof window.resolveLoad === 'function')
    await page.getByRole('button', { name: '关闭设置', exact: true }).click()
    await page.evaluate(() => { window.delayLoad = false })
    await page.getByRole('button', { name: '打开设置', exact: true }).click()
    await page.getByPlaceholder('例如 DeepSeek Official').fill('Keep latest edit')
    await page.evaluate(() => { window.resolveLoad() })
    await settle()
    assert.equal(await page.getByPlaceholder('例如 DeepSeek Official').inputValue(), 'Keep latest edit')

    await page.goto(url + '?local')
    await page.getByRole('button', { name: '添加模型提供商', exact: true }).click()
    await page.getByPlaceholder('例如 deepseek', { exact: true }).fill('qwen-local')
    await page.getByPlaceholder('https://api.deepseek.com/v1', { exact: true }).fill('http://127.0.0.1:8000/v1')
    await page.locator('textarea').fill('customer-qwen3-vl-32b')
    assert.equal(await page.getByPlaceholder('免鉴权服务可留空').inputValue(), '')
    await page.getByRole('combobox', { name: '主代理提供商', exact: true }).selectOption('qwen-local')
    await page.getByRole('button', { name: '专用模型', exact: true }).click()
    await page.getByText('尚未配置联网搜索密钥，搜索服务暂不可用；不影响保存和使用自部署模型。', { exact: true }).waitFor()
    await page.getByRole('button', { name: '保存更改', exact: true }).click()
    await page.getByText('设置已保存并立即对新任务生效。', { exact: true }).waitFor()
    const local = await page.evaluate(() => window.saved[0])
    assert.equal(local.orchestrator_provider, 'qwen-local')
    assert.equal(local.orchestrator_model, 'customer-qwen3-vl-32b')
    assert.equal(local.web_search_provider, 'deepseek', 'search configuration is independent of local model editing')
    assert.equal(Object.hasOwn(local.providers[1], 'api_key'), false)
    assert.deepEqual(errors, [])
  } finally {
    if (browser) await browser.close()
    await server.close()
  }
})
