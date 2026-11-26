import type { StudioViewContext } from '../../plugins/protocol.ts'
import type { Server } from './api.ts'

export interface SshViewContextOptions {
  readonly sftpPath?: string | undefined
  readonly activeTabKind?: 'ai' | 'human' | undefined
}

const AUTH_LABEL: Record<Server['auth_type'], string> = {
  password: '密码认证',
  key_file: '私钥文件',
  agent: 'SSH Agent',
}

/**
 * Build a zero-secret StudioViewContext snapshot for the currently selected SSH
 * server so any side-dock conversation (existing, switched, or newly created)
 * immediately knows which remote host and directory the user is operating on.
 */
export function buildSshViewContext(
  server: Server | undefined,
  options: SshViewContextOptions = {},
): StudioViewContext | null {
  if (!server) return null
  const target = `${server.username}@${server.host}:${server.port}`
  const sftpPath = options.sftpPath?.trim()
  const detail = sftpPath && sftpPath !== '.' ? `${target} · ${sftpPath}` : target
  const lines = [
    '当前远程终端正在操作的服务器（请直接使用内置 `ssh.*` 工具并传入下方 `server_id` 操作该主机，无需重新询问或盲猜服务器地址）：',
    `- server_id: ${server.id}`,
    `- 服务器名称: ${server.name}`,
    `- 连接目标: ${target}`,
    `- 环境层级: ${server.env_tier}（写操作需人工确认: ${server.require_approval ? '是' : '否'}）`,
    `- 认证方式: ${AUTH_LABEL[server.auth_type] ?? server.auth_type}`,
  ]
  if (server.default_dir) lines.push(`- 默认目录: ${server.default_dir}`)
  if (sftpPath) lines.push(`- 当前 SFTP 目录: ${sftpPath}`)
  if (server.tags.length > 0) lines.push(`- 标签: ${server.tags.join(', ')}`)
  if (server.notes?.trim()) lines.push(`- 备注: ${server.notes.trim()}`)
  if (options.activeTabKind === 'ai') lines.push('- 当前终端标签: AI 实时终端（只读）')
  else if (options.activeTabKind === 'human') lines.push('- 当前终端标签: 人工交互终端')

  return {
    plugin: 'ssh',
    id: `ssh:${server.id}`,
    badge: `远程服务器 · ${server.name}`,
    detail,
    text: lines.join('\n'),
  }
}
