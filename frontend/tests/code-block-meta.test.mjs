import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'

const source = await readFile(new URL('../src/dsh/ui-primitives/markdown/codeBlockMeta.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
}).outputText
const mod = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)
const { classifyCodeBlock, tokenizeTerminalLine, parseExecutionEntry, groupExecutionBlocks } = mod

test('classifies passing cargo test output as success terminal card with badge', () => {
  const meta = classifyCodeBlock(
    'cargo test --bin codex-workspace-mcp required_host -- --nocapture\nexit code: 0\n2 passed, 0 failed',
    'text',
  )
  assert.equal(meta.variant, 'terminal')
  assert.equal(meta.tone, 'success')
  assert.equal(meta.label, 'TERMINAL')
  assert.deepEqual(meta.badge, { text: '✓ exit 0 · 2 passed', tone: 'success' })
  assert.equal(meta.compact, true)
})

test('classifies partial failing cargo test output as error terminal card', () => {
  const meta = classifyCodeBlock(
    'cargo test --bin codex-workspace-mcp\nexit code: 101\n1051 passed, 2 failed, 12 ignored',
    'text',
  )
  assert.equal(meta.variant, 'terminal')
  assert.equal(meta.tone, 'error')
  assert.deepEqual(meta.badge, { text: '✕ exit 101 · 2 failed', tone: 'error' })
})

test('classifies single-line SANDBOX_UNAVAILABLE message as compact diagnostic strip', () => {
  const meta = classifyCodeBlock(
    'SANDBOX_UNAVAILABLE: bwrap: No permissions to create a new namespace',
    'text',
  )
  assert.equal(meta.variant, 'diagnostic')
  assert.equal(meta.tone, 'error')
  assert.equal(meta.compact, true)
})

test('classifies rust fence as source card with uppercase language label', () => {
  const meta = classifyCodeBlock('pub fn hello() -> i32 {\n  42\n}', 'rust')
  assert.equal(meta.variant, 'source')
  assert.equal(meta.tone, 'neutral')
  assert.equal(meta.label, 'RUST')
})

test('tokenizeTerminalLine preserves verbatim text and highlights pass/fail/command tokens', () => {
  const lines = [
    '$ cargo test --bin codex-workspace-mcp',
    '1051 passed, 2 failed, 12 ignored',
    'exit code: 0',
    'exit code: 101',
  ]
  for (const line of lines) {
    const tokens = tokenizeTerminalLine(line)
    assert.equal(tokens.map(t => t.text).join(''), line)
  }
  const summary = tokenizeTerminalLine('1051 passed, 2 failed, 12 ignored')
  assert.equal(summary.find(t => t.text === '1051 passed')?.kind, 'pass')
  assert.equal(summary.find(t => t.text === '2 failed')?.kind, 'fail')
  assert.equal(summary.find(t => t.text === '12 ignored')?.kind, 'muted')
})

test('parseExecutionEntry extracts command, exit code, output lines, and ignored count', () => {
  const entry = parseExecutionEntry(
    'cargo test --bin codex-workspace-mcp\nexit code: 101\n1051 passed, 2 failed, 12 ignored',
    'text',
  )
  assert.ok(entry)
  assert.equal(entry.command, 'cargo test --bin codex-workspace-mcp')
  assert.equal(entry.exitCode, 101)
  assert.equal(entry.passed, 1051)
  assert.equal(entry.failed, 2)
  assert.equal(entry.ignored, 12)
  assert.equal(entry.tone, 'error')
  assert.equal(entry.statusText, 'failed')
  assert.deepEqual(entry.outputLines, ['1051 passed, 2 failed, 12 ignored'])
})

test('groupExecutionBlocks merges verification commands and diagnostic bridge while keeping surrounding prose outside', () => {
  const blocks = [
    { type: 'paragraph', text: '验证结果：' },
    { type: 'code', lang: 'text', value: 'cargo test --bin codex-workspace-mcp read_tool_contracts -- --nocapture\nexit code: 0\n1 passed, 0 failed' },
    { type: 'code', lang: 'text', value: 'cargo test --bin codex-workspace-mcp required_host -- --nocapture\nexit code: 0\n2 passed, 0 failed' },
    { type: 'code', lang: 'text', value: 'cargo test --bin codex-workspace-mcp\nexit code: 101\n1051 passed, 2 failed, 12 ignored' },
    { type: 'paragraph', text: '全量测试中的 2 个失败均为既有虚拟帧测试执行 process.run_readonly 时无法创建 bwrap namespace：' },
    { type: 'code', lang: 'text', value: 'SANDBOX_UNAVAILABLE: bwrap: No permissions to create a new namespace' },
    { type: 'paragraph', text: '不是编译错误，也不是本次契约/路由测试失败。' },
    { type: 'code', lang: 'text', value: 'cargo check --bin codex-workspace-mcp\nexit code: 0' },
    { type: 'paragraph', text: '最终工作区状态：' },
  ]

  const grouped = groupExecutionBlocks(blocks)
  assert.equal(grouped.length, 3)
  assert.deepEqual(grouped[0], { kind: 'single', blockIndex: 0 })
  assert.deepEqual(grouped[2], { kind: 'single', blockIndex: 8 })
  assert.equal(grouped[1].kind, 'execution-group')
  assert.equal(grouped[1].startIndex, 1)
  assert.equal(grouped[1].endIndex, 7)
  assert.equal(grouped[1].entries.length, 4)
  assert.equal(grouped[1].entries[2].attachments.length, 3)
  assert.equal(grouped[1].entries[2].attachments[0].kind, 'note')
  assert.equal(grouped[1].entries[2].attachments[1].kind, 'diagnostic')
  assert.equal(grouped[1].entries[2].attachments[2].kind, 'note')
  assert.equal(grouped[1].entries[3].entry.command, 'cargo check --bin codex-workspace-mcp')
})

test('groupExecutionBlocks keeps single terminal block ungrouped and splits on unrelated prose', () => {
  const blocks = [
    { type: 'code', lang: 'bash', value: 'cargo check\nexit code: 0' },
    { type: 'paragraph', text: '这是一段普通的说明段落，不应该把两段命令强行合并到一起。' },
    { type: 'code', lang: 'bash', value: 'git status\nexit code: 0' },
  ]
  const grouped = groupExecutionBlocks(blocks)
  assert.deepEqual(grouped, [
    { kind: 'single', blockIndex: 0 },
    { kind: 'single', blockIndex: 1 },
    { kind: 'single', blockIndex: 2 },
  ])
})
