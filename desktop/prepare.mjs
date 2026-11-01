import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const desktop = dirname(fileURLToPath(import.meta.url))
const root = resolve(desktop, '..')
const release = process.argv.includes('--release')
function run(command, args, cwd = root) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' })
  if (result.error) throw result.error
  if (result.status !== 0) process.exit(result.status ?? 1)
}
run('npm', ['run', 'build'], join(root, 'frontend'))
run('cargo', ['build', ...(release ? ['--release'] : [])])
const rustc = spawnSync('rustc', ['--print', 'host-tuple'], { encoding: 'utf8', shell: process.platform === 'win32' })
if (rustc.status !== 0) throw new Error('无法获取 Rust 目标平台')
const target = rustc.stdout.trim()
const ext = process.platform === 'win32' ? '.exe' : ''
mkdirSync(join(desktop, 'binaries'), { recursive: true })
copyFileSync(join(root, 'target', release ? 'release' : 'debug', `codex-workspace-mcp${ext}`), join(desktop, 'binaries', `grow-agent-${target}${ext}`))
console.log(`后端已准备：${target}`)
