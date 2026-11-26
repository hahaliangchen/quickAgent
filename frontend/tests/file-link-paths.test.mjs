import test from 'node:test'
import assert from 'node:assert/strict'
import { resolveFileLinkPaths } from '../src/fileLinkPaths.ts'

test('general chat session only exposes absolute path for absolute file links', () => {
  const result = resolveFileLinkPaths('/home/goodtime/project/report_backend/application-dev.yml', {
    workspaceRoot: '/home/goodtime/.local/share/com.grow.agent/workspace/chat-assets',
    isChatSession: true,
  })
  assert.deepEqual(result, {
    absolutePath: '/home/goodtime/project/report_backend/application-dev.yml',
  })
  assert.equal(result?.relativePath, undefined)
})

test('general chat session joins relative file link against chat-assets root without exposing relative path', () => {
  const result = resolveFileLinkPaths('docs/report.md', {
    workspaceRoot: '/home/goodtime/.local/share/com.grow.agent/workspace/chat-assets',
    isChatSession: true,
  })
  assert.deepEqual(result, {
    absolutePath: '/home/goodtime/.local/share/com.grow.agent/workspace/chat-assets/docs/report.md',
  })
  assert.equal(result?.relativePath, undefined)
})

test('code session exposes both absolute and relative path for files inside workspace', () => {
  const fromAbsolute = resolveFileLinkPaths('/home/goodtime/project/report_backend/src/application-dev.yml', {
    workspaceRoot: '/home/goodtime/project/report_backend',
    isChatSession: false,
  })
  assert.deepEqual(fromAbsolute, {
    absolutePath: '/home/goodtime/project/report_backend/src/application-dev.yml',
    relativePath: 'src/application-dev.yml',
  })

  const fromRelative = resolveFileLinkPaths('./src/application-dev.yml', {
    workspaceRoot: '/home/goodtime/project/report_backend/',
    isChatSession: false,
  })
  assert.deepEqual(fromRelative, {
    absolutePath: '/home/goodtime/project/report_backend/src/application-dev.yml',
    relativePath: 'src/application-dev.yml',
  })
})

test('code session omits relative path when absolute file is outside workspace', () => {
  const result = resolveFileLinkPaths('/etc/nginx/nginx.conf', {
    workspaceRoot: '/home/goodtime/project/report_backend',
    isChatSession: false,
  })
  assert.deepEqual(result, {
    absolutePath: '/etc/nginx/nginx.conf',
  })
  assert.equal(result?.relativePath, undefined)
})

test('decodes file:// URIs including spaces and CJK characters', () => {
  const result = resolveFileLinkPaths('file:///home/goodtime/docs/%E6%97%A5%E6%8A%A5%202026.md', {
    workspaceRoot: '/home/goodtime/docs',
    isChatSession: false,
  })
  assert.deepEqual(result, {
    absolutePath: '/home/goodtime/docs/日报 2026.md',
    relativePath: '日报 2026.md',
  })
})

test('handles Windows drive paths cleanly in code and chat sessions', () => {
  const code = resolveFileLinkPaths('file:///C:/Users/dev/app/src/index.ts', {
    workspaceRoot: 'C:\\Users\\dev\\app',
    isChatSession: false,
  })
  assert.deepEqual(code, {
    absolutePath: 'C:/Users/dev/app/src/index.ts',
    relativePath: 'src/index.ts',
  })

  const chat = resolveFileLinkPaths('C:\\Users\\dev\\app\\src\\index.ts', {
    workspaceRoot: 'C:\\Users\\dev\\app',
    isChatSession: true,
  })
  assert.deepEqual(chat, {
    absolutePath: 'C:/Users/dev/app/src/index.ts',
  })
})
