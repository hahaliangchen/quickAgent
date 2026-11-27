import test from 'node:test'
import assert from 'node:assert/strict'
import { hasUserMarkdownSyntax, preserveUserLineBreaks } from '../src/components/userMarkdown.ts'

test('hasUserMarkdownSyntax detects structured Markdown prompts and skips plain text / session chips', () => {
  const structuredPrompt = [
    '本地 `rag-go` `main` 已推送最新提交 `ce35408`，**严禁触碰生产 `:7002`**：',
    '',
    '1. **编译部署最新 `:9005`**：',
    '   - 确认本地 `HEAD` 为 `ce35408`',
    '2. **实测端到端 SSE 流式问答**：',
    '   - 验证响应头 `Content-Type: text/event-stream`',
  ].join('\n')

  assert.equal(hasUserMarkdownSyntax(structuredPrompt), true)
  assert.equal(hasUserMarkdownSyntax('```bash\ncargo test\n```'), true)
  assert.equal(hasUserMarkdownSyntax('1. 第一步\n2. 第二步'), true)
  assert.equal(hasUserMarkdownSyntax('- A\n- B'), true)
  assert.equal(hasUserMarkdownSyntax('帮我看下这个接口'), false)
  assert.equal(hasUserMarkdownSyntax('重启吧，停止了'), false)
  assert.equal(hasUserMarkdownSyntax('参考 @[之前的会话](dsh-session:task_1) 继续做'), false)
})

test('preserveUserLineBreaks keeps single-Enter prose breaks without breaking lists or code fences', () => {
  const input = [
    '第一行说明',
    '第二行说明',
    '',
    '1. 列表一',
    '   - 子项一',
    '```js',
    'const a = 1',
    'const b = 2',
    '```',
  ].join('\n')

  const out = preserveUserLineBreaks(input)
  assert.equal(out.split('\n')[0], '第一行说明  ')
  assert.equal(out.split('\n')[1], '第二行说明')
  assert.equal(out.split('\n')[3], '1. 列表一')
  assert.equal(out.split('\n')[6], 'const a = 1')
})
