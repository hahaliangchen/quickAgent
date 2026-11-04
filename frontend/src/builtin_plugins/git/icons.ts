// Material Icon Theme (MIT). The 450 KB manifest is fetched as its own asset
// and every SVG stays a separate file, so the browser loads only icons shown.
import manifestUrl from '../../../node_modules/material-icon-theme/dist/material-icons.json?url'

const svgs = import.meta.glob('../../../node_modules/material-icon-theme/icons/*.svg', {
  eager: true, query: '?url&no-inline', import: 'default',
}) as Record<string, string>
const byFile = new Map(Object.entries(svgs).map(([path, url]) => [path.slice(path.lastIndexOf('/') + 1), url]))

type Names = Record<string, string>
interface Mapping { fileExtensions?: Names; fileNames?: Names; folderNames?: Names; folderNamesExpanded?: Names }
export interface IconTheme extends Mapping {
  file: string; folder: string; folderExpanded: string
  iconDefinitions: Record<string, { iconPath: string }>
  light?: Mapping
}

let pending: Promise<IconTheme> | undefined
export function loadIconTheme(): Promise<IconTheme> {
  return pending ??= fetch(manifestUrl).then(response => {
    if (!response.ok) throw new Error(`图标表加载失败：${response.status}`)
    return response.json() as Promise<IconTheme>
  })
}

/** A file name wins over its extension; `a.d.ts` tries `d.ts` before `ts`. */
export function iconUrl(theme: IconTheme, name: string, folder: false | { open: boolean }, light: boolean): string | undefined {
  const lower = name.toLowerCase()
  const pick = (key: keyof Mapping, value: string) => (light ? theme.light?.[key]?.[value] : undefined) ?? theme[key]?.[value]
  let id: string | undefined
  if (folder) id = pick(folder.open ? 'folderNamesExpanded' : 'folderNames', lower) ?? (folder.open ? theme.folderExpanded : theme.folder)
  else {
    id = pick('fileNames', lower)
    const parts = lower.split('.')
    for (let i = 1; !id && i < parts.length; i++) id = pick('fileExtensions', parts.slice(i).join('.'))
    id ??= theme.file
  }
  const path = theme.iconDefinitions[id]?.iconPath
  return path ? byFile.get(path.slice(path.lastIndexOf('/') + 1)) : undefined
}
