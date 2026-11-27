/**
 * Resolve copyable absolute and workspace-relative paths for a Markdown file link.
 *
 * Product rule:
 * - General chat sessions (`isChatSession === true`) only offer copying the absolute path.
 * - Code sessions (`isChatSession === false`) offer both absolute and workspace-relative paths
 *   when the file lies inside the active code workspace.
 */

export interface ResolvedFileLinkPaths {
  readonly absolutePath?: string | undefined
  readonly relativePath?: string | undefined
}

export interface ResolveFileLinkOptions {
  readonly workspaceRoot?: string | undefined
  readonly isChatSession: boolean
}

function decodeFileUri(input: string): string {
  let value = input
  if (value.startsWith('file://')) {
    const match = /^file:\/\/(?:localhost)?(\/.*)$/u.exec(value)
    if (match?.[1]) {
      try {
        value = decodeURIComponent(match[1])
      } catch {
        return input
      }
    }
  }
  if (value.startsWith('\\\\?\\')) value = value.slice(4)
  if (/^\/[A-Za-z]:[/\\]/u.test(value)) value = value.slice(1)
  // Strip optional line suffix (`#L10`, `#L10-L20`, `:10`, `:10-20`, `:10:4`) when present.
  value = value.replace(/#L[1-9]\d*(?:-L[1-9]\d*)?$/u, '')
  const colonMatch = /^(.*?):[1-9]\d*(?:(?:-|:)[1-9]\d*)?$/u.exec(value)
  if (colonMatch?.[1] && /[\\/.]/u.test(colonMatch[1]) && !/^[A-Za-z]$/u.test(colonMatch[1])) {
    value = colonMatch[1]
  }
  return value
}

function isWindowsDrivePath(value: string): boolean {
  return /^[A-Za-z]:[\\/]/u.test(value)
}

function isAbsolutePath(value: string): boolean {
  return value.startsWith('/') || isWindowsDrivePath(value)
}

function normalizePosixSegments(raw: string): string {
  const winDrive = /^([A-Za-z]:)\/(.*)$/u.exec(raw)
  const prefix = winDrive ? `${winDrive[1]}/` : raw.startsWith('/') ? '/' : ''
  const body = winDrive ? winDrive[2]! : raw.startsWith('/') ? raw.slice(1) : raw
  const out: string[] = []
  for (const part of body.split('/')) {
    if (!part || part === '.') continue
    if (part === '..') {
      if (out.length > 0 && out[out.length - 1] !== '..') {
        out.pop()
      } else if (!prefix) {
        out.push('..')
      }
      continue
    }
    out.push(part)
  }
  return prefix ? `${prefix}${out.join('/')}` : out.join('/')
}

function normalizeSlash(value: string, useWindowsSlashes: boolean): string {
  const replaced = useWindowsSlashes ? value.replaceAll('\\', '/') : value
  return replaced.replace(/\/+$/u, '')
}

function stripWorkspacePrefix(absPath: string, root: string, caseInsensitive: boolean): string | undefined {
  if (!root) return undefined
  const lhs = caseInsensitive ? absPath.toLowerCase() : absPath
  const rhs = caseInsensitive ? root.toLowerCase() : root
  if (lhs === rhs) return ''
  if (lhs.startsWith(`${rhs}/`)) return absPath.slice(root.length + 1)
  return undefined
}

export function resolveFileLinkPaths(
  rawPath: string,
  options: ResolveFileLinkOptions,
): ResolvedFileLinkPaths | undefined {
  const trimmed = rawPath.trim()
  if (!trimmed) return undefined
  const decoded = decodeFileUri(trimmed)
  let rawRoot = (options.workspaceRoot ?? '').trim()
  if (rawRoot.startsWith('\\\\?\\')) rawRoot = rawRoot.slice(4)
  if (/^\/[A-Za-z]:[/\\]/u.test(rawRoot)) rawRoot = rawRoot.slice(1)
  const winMode = isWindowsDrivePath(rawRoot) || isWindowsDrivePath(decoded)
  const root = rawRoot ? normalizePosixSegments(normalizeSlash(rawRoot, winMode)) : ''
  const path = normalizeSlash(decoded, winMode)
  if (!path) return undefined

  if (isAbsolutePath(path)) {
    const absolutePath = normalizePosixSegments(path)
    if (options.isChatSession) {
      return { absolutePath }
    }
    const rel = stripWorkspacePrefix(absolutePath, root, winMode)
    return {
      absolutePath,
      ...(rel ? { relativePath: rel } : {}),
    }
  }

  const cleanRel = normalizePosixSegments(path)
  if (!cleanRel) return undefined

  if (options.isChatSession) {
    if (!root) return undefined
    const joined = normalizePosixSegments(`${root}/${cleanRel}`)
    return { absolutePath: joined }
  }

  if (!root) {
    return { relativePath: cleanRel }
  }

  const joined = normalizePosixSegments(`${root}/${cleanRel}`)
  const rel = stripWorkspacePrefix(joined, root, winMode)
  return {
    absolutePath: joined,
    ...(rel ? { relativePath: rel } : {}),
  }
}
