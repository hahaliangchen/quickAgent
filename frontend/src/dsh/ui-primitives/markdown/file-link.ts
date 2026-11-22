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
  if (scheme === null) return value
  const authority = value.slice(scheme[0].length)
  const hostless = /^localhost[/\\]/i.test(authority)
    ? authority.slice('localhost'.length)
    : authority
  if (!hostless.startsWith('/')) return undefined
  // `file:///C:/x/y.ts` names the drive-rooted path `C:/x/y.ts`.
  return /^\/[a-z]:[/\\]/i.test(hostless) ? hostless.slice(1) : hostless
}

/**
 * Decode a file destination and its optional GitHub-style line fragment.
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
  let path: string
  try {
    path = decodeURIComponent(destination)
  } catch (_error) {
    // Malformed percent escapes cannot identify a file unambiguously.
    return undefined
  }
  if (path.length === 0 || /[\u0000-\u001f\u007f]/.test(path)
    || /^[\\/]{2}/.test(path)
    || (/^[a-z][a-z\d+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path))) return undefined
  if (hash < 0) return { path }
  const fragment = local.slice(hash + 1)
  const match = /^L([1-9]\d*)(?:-L([1-9]\d*))?$/.exec(fragment)
  if (match === null) return undefined
  const line = Number(match[1])
  const end = match[2] === undefined ? line : Number(match[2])
  if (!Number.isSafeInteger(line) || !Number.isSafeInteger(end) || end < line) return undefined
  return { path, line }
}