import { useEffect, useMemo, useState } from 'react'
import clsx from 'clsx'
import { Icon } from '../../components/WorkbenchIcon.tsx'
import { iconUrl, loadIconTheme, type IconTheme } from './icons.ts'
import { WORKSPACE_PATH_TYPE } from '../../codeChat.ts'
import css from './style.module.css'

interface Folder { name: string; path: string; folders: Map<string, Folder>; files: string[] }

function build(paths: readonly string[]): Folder {
  const root: Folder = { name: '', path: '', folders: new Map(), files: [] }
  for (const path of paths) {
    const parts = path.split('/')
    let folder = root
    for (const part of parts.slice(0, -1)) {
      let next = folder.folders.get(part)
      if (!next) { next = { name: part, path: folder.path ? `${folder.path}/${part}` : part, folders: new Map(), files: [] }; folder.folders.set(part, next) }
      folder = next
    }
    folder.files.push(path)
  }
  return root
}

const WIDTH_KEY = 'git_tree_width'
function savedWidth(): number {
  try { const saved = Number(localStorage.getItem(WIDTH_KEY)); if (saved) return Math.min(520, Math.max(180, saved)) } catch {}
  return 260
}

const byName = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' }).compare
const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1)

function useDark() {
  const [dark, setDark] = useState(() => document.body.hasAttribute('data-ds-dark-theme'))
  useEffect(() => {
    const observer = new MutationObserver(() => setDark(document.body.hasAttribute('data-ds-dark-theme')))
    observer.observe(document.body, { attributes: true, attributeFilter: ['data-ds-dark-theme'] })
    return () => observer.disconnect()
  }, [])
  return dark
}

/** Files git sees, as a tree; changed files carry their status letter. */
export function FileTree({ repository, paths, truncated, changes, active, onOpen, dragPath }: {
  repository: string; paths: readonly string[]; truncated: boolean
  changes: ReadonlyMap<string, string>; active: string | undefined; onOpen: (path: string) => void
  /** The path a drop target receives, or undefined when the entry cannot be dragged. */
  dragPath: (path: string) => string | undefined
}) {
  const [theme, setTheme] = useState<IconTheme>()
  const [open, setOpen] = useState<ReadonlySet<string>>(() => new Set(['']))
  const [query, setQuery] = useState('')
  const [searching, setSearching] = useState(false)
  const [width, setWidth] = useState(savedWidth)
  const light = !useDark()
  useEffect(() => { void loadIconTheme().then(setTheme, () => {}) }, [])

  const needle = query.trim().toLowerCase()
  const shown = useMemo(() => needle ? paths.filter(path => path.toLowerCase().includes(needle)) : paths, [paths, needle])
  const tree = useMemo(() => build(shown), [shown])
  // A folder holding a change shows a dot, so changes stay findable when collapsed.
  const changedFolders = useMemo(() => {
    const folders = new Set<string>()
    for (const path of changes.keys()) for (let i = path.indexOf('/'); i >= 0; i = path.indexOf('/', i + 1)) folders.add(path.slice(0, i))
    return folders
  }, [changes])

  const image = (name: string, folder: false | { open: boolean }) => {
    const url = theme && iconUrl(theme, name, folder, light)
    return url ? <img className={css.treeIcon} src={url} alt="" draggable={false} /> : <span className={css.treeIcon}><Icon name={folder ? 'folder' : 'file'} size={15} /></span>
  }
  const toggle = (path: string) => setOpen(current => {
    const next = new Set(current)
    if (!next.delete(path)) next.add(path)
    return next
  })

  // Rows drag out their workspace path, e.g. onto the floating chat.
  const drag = (path: string) => {
    const target = dragPath(path)
    return target ? {
      draggable: true,
      onDragStart: (event: React.DragEvent) => {
        event.dataTransfer.setData(WORKSPACE_PATH_TYPE, target)
        event.dataTransfer.setData('text/plain', target)
        event.dataTransfer.effectAllowed = 'copy'
      },
    } : {}
  }

  function rows(folder: Folder, depth: number): React.ReactNode[] {
    const out: React.ReactNode[] = []
    for (const child of [...folder.folders.values()].sort((a, b) => byName(a.name, b.name))) {
      const expanded = !!needle || open.has(child.path)
      out.push(<button key={`d:${child.path}`} type="button" className={css.treeRow} style={{ paddingLeft: 8 + depth * 14 }} onClick={() => toggle(child.path)} aria-expanded={expanded} {...drag(child.path)}>
        <span className={css.treeChevron}><Icon name={expanded ? 'chevronDown' : 'chevron'} size={12} /></span>
        {image(child.name, { open: expanded })}
        <span className={css.ellipsis}>{child.name}</span>
        {changedFolders.has(child.path) && <span className={css.treeDot} />}
      </button>)
      if (expanded) out.push(...rows(child, depth + 1))
    }
    for (const path of [...folder.files].sort((a, b) => byName(baseName(a), baseName(b)))) {
      const code = changes.get(path)
      out.push(<button key={`f:${path}`} type="button" title={path} className={clsx(css.treeRow, active === path && css.treeRowActive, code && css[`tree${code === '?' ? 'Q' : code === '!' ? 'X' : code}`])}
        style={{ paddingLeft: 8 + depth * 14 + 16 }} onClick={() => onOpen(path)} {...drag(path)}>
        {image(baseName(path), false)}
        <span className={css.ellipsis}>{baseName(path)}</span>
        {code && <span className={css.treeCode}>{code === '?' ? 'U' : code}</span>}
      </button>)
    }
    return out
  }

  // Drag the right edge to resize; the width is remembered on this device.
  function resize(event: React.PointerEvent) {
    event.preventDefault()
    const startX = event.clientX, startWidth = width
    let next = startWidth
    const move = (e: PointerEvent) => { next = Math.min(520, Math.max(180, startWidth + e.clientX - startX)); setWidth(next) }
    const up = () => {
      window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', up)
      try { localStorage.setItem(WIDTH_KEY, String(next)) } catch {}
    }
    window.addEventListener('pointermove', move); window.addEventListener('pointerup', up)
  }

  const rootOpen = !!needle || open.has('')
  return <aside className={css.tree} style={{ width }} aria-label="文件">
    <div className={css.treeResizer} title="拖动调整宽度" onPointerDown={resize} />
    <div className={css.treeHead}>
      <h2>文件</h2>
      <button type="button" className={css.iconButton} title="搜索文件" onClick={() => { setSearching(v => !v); setQuery('') }}><Icon name={searching ? 'close' : 'search'} size={16} /></button>
    </div>
    {searching && <input className={css.treeSearch} autoFocus value={query} onChange={event => setQuery(event.target.value)} placeholder="按路径筛选文件" />}
    <div className={css.treeBody}>
      <button type="button" className={clsx(css.treeRow, css.treeRoot)} onClick={() => toggle('')} aria-expanded={rootOpen}>
        <span className={css.treeChevron}><Icon name={rootOpen ? 'chevronDown' : 'chevron'} size={12} /></span>
        <Icon name="folder" size={16} /><span className={css.ellipsis}>{repository}</span>
      </button>
      {rootOpen && rows(tree, 1)}
      {needle && shown.length === 0 && <p className={css.treeNote}>没有匹配的文件。</p>}
      {truncated && <p className={css.treeNote}>文件太多，只列出前 20000 个。</p>}
    </div>
  </aside>
}
