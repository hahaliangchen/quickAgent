import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createReadStream, existsSync } from 'node:fs'
import { once } from 'node:events'
import { mkdtemp, open, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { createServer } from 'node:http'
import { extname, join, resolve, sep } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../../bundled_plugins/', import.meta.url))
const fallback = fileURLToPath(new URL('../../../ai-ppt-view/node_modules/playwright/index.mjs', import.meta.url))
const playwrightModule = process.env.PLAYWRIGHT_MODULE ?? (existsSync(fallback) ? fallback : undefined)
async function oversizedPdf(file) {
  const handle = await open(file, 'wx')
  const offsets = [0]
  let position = 0
  const append = async text => { position += (await handle.write(text, position, 'ascii')).bytesWritten }
  const object = async (id, text) => { offsets[id] = position; await append(id + ' 0 obj\n' + text + '\nendobj\n') }
  try {
    await append('%PDF-1.7\n')
    await object(1, '<< /Type /Catalog /Pages 2 0 R >>')
    await object(2, '<< /Type /Pages /Kids [3 0 R] /Count 1 >>')
    await object(3, '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << >> /Contents 4 0 R >>')
    const content = '0 0 1 rg 45 45 210 210 re f'
    await object(4, '<< /Length ' + content.length + ' >>\nstream\n' + content + '\nendstream')
    // Leave a sparse, unused payload between the first page and the xref.
    const payloadSize = 72 * 1024 * 1024
    offsets[5] = position
    await append('5 0 obj\n<< /Length ' + payloadSize + ' >>\nstream\n')
    position += payloadSize
    await append('\nendstream\nendobj\n')
    const xref = position
    await append('xref\n0 6\n0000000000 65535 f \n' + offsets.slice(1).map(n => String(n).padStart(10, '0') + ' 00000 n \n').join('') +
      'trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n' + xref + '\n%%EOF\n')
  } finally { await handle.close() }
}
const ppt = process.env.QUICKAGENT_TEST_PPTX
const ids = ['pdf','excel','word','markdown','ppt']
const media = new Map(Object.entries({'.html':'text/html','.js':'text/javascript','.mjs':'text/javascript','.css':'text/css','.wasm':'application/wasm','.pdf':'application/pdf','.pptx':'application/vnd.openxmlformats-officedocument.presentationml.presentation'}))

async function startService(id, program, health) {
  const reserve = createServer()
  await new Promise(resolve => reserve.listen(0, '127.0.0.1', resolve))
  const port = reserve.address().port
  await new Promise(resolve => reserve.close(resolve))
  const base = 'http://127.0.0.1:' + port
  const child = spawn(join(root, id, 'bin', program), [], { env: { ...process.env, BIND: '127.0.0.1:' + port }, stdio: 'ignore' })
  const done = once(child, 'exit')
  // Handle a missing or unlaunchable bundle as a test failure.
  void done.catch(() => {})
  for (let attempt = 0; attempt < 80; attempt++) {
    if (child.exitCode !== null) break
    try { if ((await fetch(base + health)).ok) return { base, async stop() { child.kill(); await done } } } catch {}
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  child.kill()
  await done
  throw new Error('bundled ' + id + ' service did not start')
}

test('bundled office previews enforce the protocol and load real documents', {
  skip: !playwrightModule || !ids.every(id => existsSync(join(root,id,'page/index.html'))), timeout: 60000,
}, async () => {
  const { chromium } = await import(playwrightModule)
  const temp = await mkdtemp(join(tmpdir(), 'quickagent-office-preview-'))
  const pdf = join(temp, '72mib.pdf')
  await oversizedPdf(pdf)
  const transcript = []
  const services = new Map()
  const word = join(temp, 'note.docx'), excel = join(temp, 'sheet.xlsx')
  const server = createServer(async (req,res) => {
    const url = new URL(req.url,'http://localhost')
    const pathname = url.pathname
    const serviceRoute = /^\/agent\/skill-packages\/([^/]+)\/local-api(\/.*)$/u.exec(pathname)
    const service = services.get(serviceRoute?.[1])
    if (service) {
      try {
        const chunks = []
        for await (const chunk of req) chunks.push(chunk)
        const reply = await fetch(service.base + serviceRoute[2] + url.search, {
          method: req.method, headers: req.headers['content-type'] ? { 'content-type': req.headers['content-type'] } : {},
          body: chunks.length ? Buffer.concat(chunks) : undefined,
        })
        res.writeHead(reply.status, { 'Content-Type': reply.headers.get('content-type') ?? 'application/octet-stream' })
        res.end(Buffer.from(await reply.arrayBuffer()))
      } catch { res.writeHead(502); res.end() }
      return
    }
    if (pathname === '/') {
      res.writeHead(200,{'Content-Type':'text/html'})
      res.end('<meta charset="utf-8"><script>window.received=[];window.addEventListener("message", e => window.received.push(e.data));</script><iframe title="studio" style="width:710px;height:760px"></iframe>')
      return
    }
    if (pathname === '/agent/fixture/note.md' || pathname === '/agent/fixture/next.md') {
      res.writeHead(200,{'Content-Type':'text/plain'})
      res.end(pathname.endsWith('/note.md') ? '# Opened from preview' : '# Opened by message')
      return
    }
    const bundle = /^\/agent\/skill-packages\/(pdf|excel|word|ppt|markdown)\/pages\/studio\/(.*)$/u.exec(pathname)
    let file
    if (pathname === '/agent/fixture/large.pdf') file = pdf
    else if (pathname === '/agent/fixture/deck.pptx') file = ppt
    else if (pathname === '/agent/fixture/note.docx') file = word
    else if (pathname === '/agent/fixture/sheet.xlsx') file = excel
    else if (bundle) {
      const directory = resolve(root,bundle[1],'page')
      const resource = bundle[2] === 'index' ? 'index.html' : bundle[2]
      file = resolve(directory,resource)
      if (!file.startsWith(directory + sep)) file = undefined
    }
    try {
      if (!file) throw new Error('unavailable')
      const { size } = await stat(file)
      const headers = {'Content-Type':media.get(extname(file)) ?? 'application/octet-stream','Accept-Ranges':'bytes','Cache-Control':'no-store'}
      const range = /^bytes=(\d+)-(\d+)$/u.exec(req.headers.range ?? '')
      const start = range ? Number(range[1]) : 0, end = range ? Math.min(Number(range[2]),size-1) : size-1
      if (range) headers['Content-Range'] = 'bytes '+start+'-'+end+'/'+size
      headers['Content-Length'] = end-start+1
      const status = range ? 206 : 200
      if (pathname === '/agent/fixture/large.pdf') transcript.push({ status, length: end-start+1, size })
      res.writeHead(status,headers)
      const stream = createReadStream(file,{start,end})
      res.on('close',()=>stream.destroy())
      stream.on('error',()=>res.destroy())
      stream.pipe(res)
    } catch { res.writeHead(404); res.end() }
  })
  await new Promise(r=>server.listen(0,'127.0.0.1',r))
  let browser
  try {
    services.set('word', await startService('word', 'word-server', '/api/health'))
    services.set('excel', await startService('excel', 'excel-server', '/api/health'))
    if (ppt) services.set('ppt', await startService('ppt', 'ppt-font-server', '/api/fonts'))
    async function fixture(id, path, document, filename) {
      const response = await fetch(services.get(id).base + path, {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(document),
      })
      assert.ok(response.ok, id + ': ' + await response.clone().text().then(text => response.ok ? '' : text))
      await writeFile(filename, new Uint8Array(await response.arrayBuffer()))
    }
    const sample = await (await fetch(services.get('excel').base + '/api/xlsx/sample')).json()
    await fixture('excel', '/api/xlsx/export', sample, excel)
    await fixture('word', '/api/docx/create', {
      version: 1, title: 'Preview verification', media: {},
      page: { widthMm: 210, heightMm: 297, marginsMm: { top: 25, bottom: 25, left: 25, right: 25 } },
      blocks: [{ type: 'paragraph', id: 'preview', runs: [{ text: 'Read-only Word fixture' }] }],
    }, word)
    browser = await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? {executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH} : {})})
    const page=await browser.newPage({viewport:{width:1100,height:850}})
    await page.goto('http://127.0.0.1:'+server.address().port)
    async function open(id, file, name, mode='preview') {
      const query=new URLSearchParams({mode,file,name,theme:'dark'})
      await page.evaluate(({id,query})=>{
        window.received=[]
        document.querySelector('iframe').src='/agent/skill-packages/'+id+'/pages/studio/index?'+query
      },{id,query:String(query)})
      return page.frameLocator('iframe')
    }
    const pageErrors = []
    page.on('console', message => { if (message.type() === 'error' || message.type() === 'warning') pageErrors.push(message.text()) })
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('response', response => { if (response.status() >= 400) pageErrors.push(response.status() + ' ' + response.url()) })
    for (const id of ids) {
      await open(id, id==='excel' ? '/outside.xlsx' : 'https://invalid.example/agent/file', 'file.'+id)
      try {
        await page.waitForFunction(()=>window.received.some(item=>item?.type==='quickagent:preview-error' && /同源/.test(item.message)), undefined, {timeout:8000})
      } catch (cause) {
        assert.fail(id + ': ' + JSON.stringify({received: await page.evaluate(()=>window.received), pageErrors}) + ' (' + cause.message + ')')
      }
    }
    async function ready(kind) {
      const message = await page.waitForFunction(key => window.received.find(item =>
        item?.type === 'quickagent:preview-error' || (item?.type === 'quickagent:preview-ready' && item.kind === key)), kind, {timeout:10000})
      const result = await message.jsonValue()
      assert.equal(result.type, 'quickagent:preview-ready', JSON.stringify({result,pageErrors}))
      return result.meta
    }
    const document = await open('word', '/agent/fixture/note.docx', 'note.docx')
    await ready('word')
    await document.getByText('Read-only Word fixture', {exact:true}).waitFor()
    assert.equal(await document.locator('[contenteditable="true"]').count(), 0)
    const spreadsheet = await open('excel', '/agent/fixture/sheet.xlsx', 'sheet.xlsx')
    assert.ok((await ready('excel')).sheets > 0)
    assert.equal(await spreadsheet.locator('#fx-input').getAttribute('readonly'), '')
    assert.equal(await spreadsheet.getByRole('button', {name: '导出 .xlsx', includeHidden: true}).isVisible(), false)
    const md=await open('markdown','/agent/fixture/note.md','note.md','edit')
    await md.getByRole('heading',{name:'Opened from preview'}).waitFor({timeout:8000})
    await page.evaluate(()=>document.querySelector('iframe').contentWindow.postMessage({type:'quickagent:open-file',file:'/agent/fixture/next.md',name:'next.md'},location.origin))
    await md.getByRole('heading',{name:'Opened by message'}).waitFor({timeout:8000})
    {
      const reader=await open('pdf','/agent/fixture/large.pdf','large.pdf')
      await page.waitForFunction(()=>window.received.some(item=>item?.type==='quickagent:preview-ready' && item.kind==='pdf'))
      await reader.locator('#paper[data-ready="true"]').waitFor({timeout:12000})
      assert.equal(await reader.locator('#export').isVisible(),false)
      const chunks=transcript.filter(request=>request.status===206)
      const bytes=chunks.reduce((sum,request)=>sum+request.length,0)
      assert.ok(chunks.length>0 && bytes<chunks[0].size/2, JSON.stringify(transcript))
      assert.ok(chunks.some(request=>request.length===256*1024))
      process.stdout.write('PDF: '+chunks.length+' HTTP 206 responses, '+bytes+' requested bytes of '+chunks[0].size+'\n')
    }
    if (ppt) {
      const viewer=await open('ppt','/agent/fixture/deck.pptx','deck.pptx')
      assert.ok((await ready('ppt')).pages > 0)
      assert.equal(await viewer.locator('#btn-export').isVisible(),false)
    }
  } finally {
    await browser?.close()
    await Promise.all([...services.values()].map(service => service.stop()))
    await new Promise(r=>server.close(r))
    await rm(temp, { recursive: true, force: true })
  }
})
