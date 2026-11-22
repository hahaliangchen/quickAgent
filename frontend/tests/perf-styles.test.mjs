import assert from 'node:assert/strict'
import { readdir, readFile } from 'node:fs/promises'
import test from 'node:test'
import postcss from 'postcss'

const src = new URL('../src/', import.meta.url)
const rootCss = new URL('dsh/conversation/ConversationRoot.module.css', src)

async function* styles(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) yield* styles(new URL(entry.name + '/', dir))
    else if (entry.isFile() && entry.name.endsWith('.css')) yield new URL(entry.name, dir)
  }
}

// These selectors can invalidate styles far beyond the component that changed.
test('CSS avoids broad :empty queries and root-level :has()', async () => {
  const violations = []
  for await (const file of styles(src)) {
    const sheet = postcss.parse(await readFile(file, 'utf8'), { from: file.pathname })
    sheet.walkRules(rule => {
      if (/(?:\b(?:body|html)|:root)\s*:\s*has\s*\(/i.test(rule.selector)
        || /:empty\s*:not\(\s*\[\s*class\s*\]\s*\)/i.test(rule.selector)) {
        violations.push(file.pathname.slice(src.pathname.length) + ':' + rule.source.start.line + ' ' + rule.selector)
      }
    })
  }
  assert.deepEqual(violations, [])
})

test('the scroll body resolves width before passing it to the conversation tree', async () => {
  const sheet = postcss.parse(await readFile(rootCss, 'utf8'), { from: rootCss.pathname })
  for (const property of ['--dsh-conversation-column-width', '--dsh-chat-content-width-resolved']) {
    const registration = sheet.nodes.find(node => node.type === 'atrule' && node.name === 'property' && node.params === property)
    assert.ok(registration, property + ' is registered')
    const options = Object.fromEntries(registration.nodes.map(node => [node.prop, node.value]))
    assert.match(options.syntax, /^['"]<length>['"]$/)
    assert.equal(options.inherits, 'false')
    assert.equal(options['initial-value'], '0px')
  }
  const scroller = sheet.nodes.find(node => node.type === 'rule' && node.selector === '.scrollBody')
  assert.ok(scroller)
  const declarations = Object.fromEntries(scroller.nodes.filter(node => node.type === 'decl').map(node => [node.prop, node.value]))
  assert.match(declarations['--dsh-chat-content-width-resolved'], /clamp\([\s\S]*var\(--dsh-conversation-column-width\)/)
  assert.equal(declarations['--dsh-chat-content-width'], 'var(--dsh-chat-content-width-resolved)')
  sheet.walkDecls('--dsh-chat-content-width-resolved', declaration => assert.equal(declaration.parent, scroller))
})
