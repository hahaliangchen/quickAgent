import test from 'node:test'
import assert from 'node:assert/strict'
import { buildStudioCatalog } from '../src/officeStudio.ts'

function pkg(overrides) {
  return {
    id: 'ppt',
    name: 'ppt',
    title: 'PPT 编辑器',
    description: '本地 Rust WASM PPTX 渲染与编辑工作台',
    enabled: true,
    scope: 1,
    has_skill: true,
    tools: [],
    pages: [{ id: 'studio', title: 'PPT 编辑器', entry: 'page/index.html', renderer: 'html' }],
    package_root: '/bundled_plugins/ppt',
    origin: 'external',
    inject: [],
    ...overrides,
  }
}

test('buildStudioCatalog lights up bundled ppt and keeps remaining suite slots as pending cards', () => {
  const cards = buildStudioCatalog([pkg({})])
  assert.equal(cards.length, 5)
  assert.equal(cards[0].slotId, 'ppt')
  assert.equal(cards[0].ready, true)
  assert.equal(cards[0].key, 'ppt:studio')
  assert.equal(cards[1].slotId, 'word')
  assert.equal(cards[1].ready, false)
  assert.equal(cards[2].slotId, 'markdown')
  assert.equal(cards[2].ready, false)
  assert.equal(cards[3].slotId, 'excel')
  assert.equal(cards[3].ready, false)
  assert.equal(cards[4].slotId, 'pdf')
  assert.equal(cards[4].ready, false)
})

test('buildStudioCatalog auto-activates word and markdown slots and appends custom external modules', () => {
  const cards = buildStudioCatalog([
    pkg({ id: 'ppt' }),
    pkg({ id: 'word', title: 'Word 编辑器', pages: [{ id: 'studio', title: 'Word 编辑器', entry: 'page/index.html', renderer: 'html' }] }),
    pkg({ id: 'markdown', title: 'Markdown 编辑器', pages: [{ id: 'studio', title: 'Markdown 编辑器', entry: 'page/index.html', renderer: 'html' }] }),
    pkg({ id: 'pdf', title: 'PDF 阅读器', pages: [{ id: 'studio', title: 'PDF 阅读器', entry: 'page/index.html', renderer: 'html' }] }),
    pkg({ id: 'diagram-lab', title: '架构图画板', description: '流程图绘制', pages: [{ id: 'main', title: '架构图画板', entry: 'page/index.html', renderer: 'html' }] }),
    // Builtins and disabled packages must not show up in the Office Studio catalog.
    pkg({ id: 'git', origin: 'builtin' }),
    pkg({ id: 'excel', enabled: false }),
  ])
  assert.equal(cards.length, 6)
  assert.equal(cards.find(c => c.slotId === 'ppt')?.ready, true)
  assert.equal(cards.find(c => c.slotId === 'word')?.ready, true)
  assert.equal(cards.find(c => c.slotId === 'markdown')?.ready, true)
  assert.equal(cards.find(c => c.slotId === 'pdf')?.ready, true)
  assert.equal(cards.find(c => c.slotId === 'excel')?.ready, false)
  const custom = cards.find(c => c.slotId === 'diagram-lab')
  assert.ok(custom)
  assert.equal(custom.ready, true)
  assert.equal(custom.icon, 'app')
})
