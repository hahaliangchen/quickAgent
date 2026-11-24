import assert from 'node:assert/strict'
import test from 'node:test'
import {
  commandLines,
  countCommands,
  emptyTranscript,
  feedTranscript,
} from '../src/builtin_plugins/ssh/transcript.ts'

const start = (cwd, command) => '\r\n\u001b[36m[AI \u00b7 ' + cwd + ']\u001b[0m $ ' + command + '\r\n'
const end = (text, ok = true) => '\r\n\u001b[3' + (ok ? '2' : '1') + 'm[' + text + ']\u001b[0m\r\n'

/** Feed a whole script in one chunk and return the state. */
function run(text) {
  const state = emptyTranscript()
  feedTranscript(state, text)
  return state
}

test('a command, its output and its outcome become one card', () => {
  const state = run(start('/nested', 'echo hello') + 'hello\r\n' + end('exit 0 \u00b7 758 ms'))
  assert.equal(state.blocks.length, 1)
  const [block] = state.blocks
  assert.equal(block.kind, 'command')
  assert.equal(block.cwd, '/nested')
  assert.equal(block.command, 'echo hello')
  assert.equal(block.output, 'hello\r\n')
  assert.deepEqual(block.exit, { code: 0, elapsed: 758 })
  assert.equal(state.buffer, '')
})

test('a marker split across chunks does not leak into the output', () => {
  const whole = start('/srv', 'docker ps') + 'CONTAINER ID\r\n' + end('exit 0 \u00b7 12 ms')
  const state = emptyTranscript()
  for (let at = 0; at < whole.length; at += 3) feedTranscript(state, whole.slice(at, at + 3))
  assert.equal(state.blocks.length, 1)
  const [block] = state.blocks
  assert.equal(block.command, 'docker ps')
  assert.equal(block.output, 'CONTAINER ID\r\n')
  assert.deepEqual(block.exit, { code: 0, elapsed: 12 })
  assert.equal(state.buffer, '')
  assert.ok(!block.output.includes('\u001b'), 'no escape sequence survives into the output')
})

test('a failed command keeps its exit code', () => {
  const state = run(start('.', 'false') + end('exit 1 \u00b7 4 ms', false))
  assert.deepEqual(state.blocks[0].exit, { code: 1, elapsed: 4 })
  assert.deepEqual(countCommands(state.blocks), { total: 1, failed: 1 })
})

test('a command that ended without a status is not shown as a failure', () => {
  const state = run(start('.', 'sleep 99') + end('no exit status \u00b7 3000 ms'))
  assert.deepEqual(state.blocks[0].exit, { code: null, elapsed: 3000 })
  assert.deepEqual(countCommands(state.blocks), { total: 1, failed: 0 })
})

test('escaped control characters in a command become display lines', () => {
  // The backend escapes control characters, so a real newline in a command
  // arrives as the two characters backslash-r backslash-n.
  const state = run(start('/', 'ss -ltnp\\r\\nps -ef | grep node'))
  assert.equal(commandLines(state.blocks[0].command).length, 2)
  assert.deepEqual(commandLines(state.blocks[0].command), ['ss -ltnp', 'ps -ef | grep node'])
})

test('an SFTP notice between commands stays a notice', () => {
  const state = run(
    start('.', 'ls') + 'a.txt\r\n' + end('exit 0 \u00b7 5 ms')
    + '\u001b[33m[AI SFTP]\u001b[0m 上传 a.txt\r\n'
    + start('.', 'md5sum a.txt') + 'abc\r\n',
  )
  assert.deepEqual(state.blocks.map(block => block.kind), ['command', 'note', 'command'])
  assert.equal(state.blocks[1].text, '上传 a.txt')
  assert.equal(state.blocks[2].output, 'abc\r\n')
})

test('the second command does not inherit the first one output', () => {
  const state = run(start('.', 'one') + 'first\r\n' + end('exit 0 \u00b7 1 ms') + start('.', 'two') + 'second\r\n')
  assert.equal(state.blocks.length, 2)
  assert.equal(state.blocks[0].output, 'first\r\n')
  assert.equal(state.blocks[1].output, 'second\r\n')
  assert.equal(state.blocks[1].exit, undefined)
  assert.deepEqual(countCommands(state.blocks), { total: 2, failed: 0 })
})
