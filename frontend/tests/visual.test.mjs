import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/visual.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { foldImageData, visualArtifactUrl, visualEvidenceOf } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))

test('request view folds image bytes without modifying the real request body', () => {
  const body = { messages: [{ role: 'user', content: [{ type: 'image_url', image_url: { url: 'data:image/png;base64,AAAA' } }] }] }
  assert.match(foldImageData(body).messages[0].content[0].image_url.url, /图片数据已折叠/)
  assert.equal(body.messages[0].content[0].image_url.url, 'data:image/png;base64,AAAA')
})

test('a turn\'s pictures are read from every payload shape a tool returns', () => {
  const call = (name, payload) => ({ name, result: { text: typeof payload === 'string' ? payload : JSON.stringify(payload) } })
  const found = visualEvidenceOf([
    // A screenshot names its saved artifact, with its size and its own task.
    call('browser.screenshot', { visual_artifact: { artifact_id: 'v1', task_id: 'T', width: 1280, height: 713 } }),
    // The vision model answers with the ids it read: v1 again, plus a second one.
    call('analyze_images', { status: 'completed', analysis: '…', artifact_ids: ['v1', 'v2'] }),
    // Reading an image attachment hands back the artifact object itself.
    call('read_attachment', { image_artifact_id: { artifact_id: 'v3', width: 456, height: 540 } }),
    // Output that names no picture contributes nothing, plain text included.
    call('exec_command', { stdout: 'done' }),
    call('screenshot_text', 'not json at all'),
    { name: 'still_running', result: undefined },
  ], 'T')

  assert.deepEqual(found.map(item => [item.artifactId, item.tool]),
    [['v1', 'browser.screenshot'], ['v2', 'analyze_images'], ['v3', 'read_attachment']],
    'each picture once, in the order the turn first named it')
  assert.equal(found[0].width, 1280)
  assert.equal(found[0].height, 713)
  assert.equal(found[1].taskId, 'T', 'an id without a task belongs to the conversation asking')
  assert.equal(found[2].width, 456)
})

test('an id that names no reachable task is dropped instead of breaking', () => {
  const call = (name, payload) => ({ name, result: { text: JSON.stringify(payload) } })
  assert.deepEqual(visualEvidenceOf([call('analyze_images', { artifact_ids: ['v1'] })], ''), [],
    'no conversation to serve from, so no picture')
  assert.deepEqual(
    visualEvidenceOf([call('browser.screenshot', { visual_artifact: { artifact_id: 'v1', task_id: 'T' } })], '').map(item => item.taskId),
    ['T'], 'an artifact that names its own task still shows')
})

test('an artifact is served from the task that saved it', () => {
  assert.equal(visualArtifactUrl('T', 'visual-1'), '/agent/tasks/T/visual-artifacts/visual-1/image')
  assert.equal(visualArtifactUrl('a b', 'c/d'), '/agent/tasks/a%20b/visual-artifacts/c%2Fd/image')
})
