/** Local Markdown destinations accepted by the file-preview callback. */


/**
 * Reduce a `file:` URL to the bare path it names. Models routinely spell a
 * workspace file as `file:///workspace/src/x.ts`, which must resolve to the
 * same target as the relative path a settled summary usually carries.
 * Only the empty-host form (`file:///…`) and `localhost` name a local file;
 * any other host is remote and cannot be opened.
 * @param value - Parsed Markdown link destination.
 * @returns The remaining path (Windows drive-letter form included), or
 * undefined when the URL names no local path at all.
 */
function stripFileScheme(value: string): string | undefined {
  const scheme = /^file:\/\//i.exec(value)
  if (scheme === null) {
    // Models occasionally emit `/C:/x/y.ts` without the `file://` prefix.
    return /^\/[a-z]:[/\\]/i.test(value) ? value.slice(1) : value
  }
  const authority = value.slice(scheme[0].length)
  const hostless = /^localhost[/\\]/i.test(authority)
    ? authority.slice('localhost'.length)
    : authority
  if (!hostless.startsWith('/')) return undefined
  // `file:///C:/x/y.ts` names the drive-rooted path `C:/x/y.ts`.
  return /^\/[a-z]:[/\\]/i.test(hostless) ? hostless.slice(1) : hostless
}

/**
 * Extract trailing `:line`, `:line-end`, or `:line:col` suffixes from local paths
 * (common in compiler diagnostics and AI summaries) without touching URI schemes
 * or Windows drive letters like `C:/...`.
 */
function stripTrailingColonLine(value: string): { path: string; line?: number } | undefined {
  if (/^[a-z][a-z\d+.-]*:\/\//i.test(value)) return { path: value }
  const match = /^(.*?):([1-9]\d*)(?:(-|:)([1-9]\d*))?$/u.exec(value)
  if (match === null) return { path: value }
  const base = match[1] ?? ''
  // Require a path separator or filename dot so schemes like `tel:12345` are never treated as paths.
  if (!/[\\/.]/u.test(base) || /^[a-z]$/i.test(base)) return { path: value }
  const line = Number(match[2])
  const sep = match[3]
  const second = match[4] === undefined ? undefined : Number(match[4])
  if (!Number.isSafeInteger(line)) return undefined
  if (sep === '-' && second !== undefined && (!Number.isSafeInteger(second) || second < line)) return undefined
  return { path: base, line }
}

/**
 * Decode a file destination and its optional line reference (`#L10`, `#L10-L20`, `:10`, `:10-20`, `:10:4`).
 * Literal `?` and `#` in filenames must be percent-encoded.
 * @param value - Parsed Markdown link destination.
 * @returns A local path and optional first line, or undefined for URLs,
 * fragment-only links, queries, malformed escapes, or invalid line ranges.
 */
export function parseFileLink(value: string): { path: string; line?: number } | undefined {
  const local = stripFileScheme(value)
  if (local === undefined) return undefined
  const hash = local.indexOf('#')
  const destination = hash < 0 ? local : local.slice(0, hash)
  if (destination.includes('?')) return undefined
  let decoded: string
  try {
    decoded = decodeURIComponent(destination)
  } catch (_error) {
    // Malformed percent escapes cannot identify a file unambiguously.
    return undefined
  }
  if (/^\/[a-z]:[/\\]/i.test(decoded)) decoded = decoded.slice(1)
  let path = decoded
  let colonLine: number | undefined
  if (hash < 0) {
    const stripped = stripTrailingColonLine(decoded)
    if (stripped === undefined) return undefined
    path = stripped.path
    colonLine = stripped.line
  }
  if (path.length === 0 || /[\u0000-\u001f\u007f]/.test(path)
    || /^[\\/]{2}/.test(path)
    || (/^[a-z][a-z\d+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path))) return undefined
  if (hash < 0) return colonLine === undefined ? { path } : { path, line: colonLine }
  const fragment = local.slice(hash + 1)
  const match = /^L([1-9]\d*)(?:-L([1-9]\d*))?$/.exec(fragment)
  if (match === null) return undefined
  const line = Number(match[1])
  const end = match[2] === undefined ? line : Number(match[2])
  if (!Number.isSafeInteger(line) || !Number.isSafeInteger(end) || end < line) return undefined
  return { path, line }
}