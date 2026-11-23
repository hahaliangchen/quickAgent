import { useEffect, useRef, useState } from 'react'
import { type Access, type Entry, type Server, confirmAccess, joinRemote, ssh } from './api.ts'
import css from './style.module.css'

type Transfer = { id: number; name: string; progress: number; status: '等待' | '上传中' | '完成' | '已取消' | '失败'; controller: AbortController }
const fileSize = (bytes: number | null) => bytes === null ? '—' : bytes < 1024 ? bytes + ' B' : bytes < 1024 * 1024 ? (bytes / 1024).toFixed(1) + ' KiB' : (bytes / (1024 * 1024)).toFixed(1) + ' MiB'
const validName = (value: string) => !!value && value !== '.' && value !== '..' && !value.includes('/') && !value.includes('\0')

export default function SftpExplorer({ server, access, hidden, followTarget, refreshSignal, onError, onActivity }: {
  server: Server; access: Access; hidden: boolean; followTarget?: { path: string; seq: number } | undefined; refreshSignal?: number | undefined
  onError: (error: unknown) => void; onActivity: () => void
}) {
  const [path, setPath] = useState(server.default_dir || '.')
  const [result, setResult] = useState<{ path: string; entries: Entry[] }>()
  const [busy, setBusy] = useState(false)
  const [revision, setRevision] = useState(0)
  const [initialized, setInitialized] = useState(false)
  const [dragging, setDragging] = useState(false)
  const [preview, setPreview] = useState<{ path: string; text: string; truncated: boolean }>()
  const [transfers, setTransfers] = useState<Transfer[]>([])
  const listRef = useRef<HTMLInputElement>(null)
  const queue = useRef<Transfer[]>([])
  const seq = useRef(0)
  const callback = useRef({ onError, onActivity })
  callback.current = { onError, onActivity }

  const followed = useRef<number>()
  const refreshed = useRef<number>()
  useEffect(() => {
    if (!followTarget || followed.current === followTarget.seq) return
    followed.current = followTarget.seq
    setPath(followTarget.path)
    setResult(undefined)
    setInitialized(true)
    setRevision(value => value + 1)
  }, [followTarget])
  useEffect(() => {
    if (refreshSignal === undefined || refreshed.current === refreshSignal) return
    refreshed.current = refreshSignal
    setRevision(value => value + 1)
  }, [refreshSignal])

  useEffect(() => {
    if (!initialized) return
    const abort = new AbortController()
    setBusy(true)
    ssh.entries(server.id, path, abort.signal).then(result => { if (!abort.signal.aborted) setResult(result) }, error => { if (!abort.signal.aborted) callback.current.onError(error) })
      .finally(() => { if (!abort.signal.aborted) setBusy(false) })
    return () => abort.abort()
  }, [server.id, path, revision, initialized])
  useEffect(() => () => { for (const item of queue.current) item.controller.abort() }, [])

  const target = result?.path ?? path
  const refresh = () => { setInitialized(true); setRevision(value => value + 1) }
  function navigate(dir: string) { setResult(undefined); setPath(dir); setInitialized(true) }
  function transferStatus(id: number, status: Transfer['status'], progress?: number) {
    setTransfers(items => items.map(item => item.id === id ? { ...item, status, progress: progress ?? item.progress } : item))
  }
  async function upload(input: FileList | File[]) {
    const files = Array.from(input).filter(file => validName(file.name))
    if (!result || busy || !files.length || !confirmAccess(server, '上传至 ' + target + '\n' + files.map(file => file.name).join('\n'))) return
    const rows = files.map(file => ({ id: ++seq.current, name: file.name, progress: 0, status: '等待' as const, controller: new AbortController() }))
    queue.current.push(...rows)
    setTransfers(items => [...items.filter(item => item.status === '等待' || item.status === '上传中'), ...rows])
    for (const [index, file] of files.entries()) {
      const item = rows[index]!
      if (item.controller.signal.aborted) { transferStatus(item.id, '已取消'); continue }
      transferStatus(item.id, '上传中')
      try {
        await ssh.upload(server.id, joinRemote(target, file.name), file, { ...access, approved: server.require_approval }, item.controller.signal, progress => transferStatus(item.id, '上传中', progress))
        transferStatus(item.id, '完成', 1)
        callback.current.onActivity()
        setRevision(value => value + 1)
      } catch (error) {
        transferStatus(item.id, item.controller.signal.aborted ? '已取消' : '失败')
        if (!item.controller.signal.aborted) callback.current.onError(error)
      }
    }
    queue.current = queue.current.filter(item => !rows.includes(item as typeof rows[number]))
  }
  async function mutate(action: string, name?: string) {
    const original = name ? joinRemote(target, name) : target
    let destination: string | undefined
    if (action === 'mkdir' || action === 'rename') {
      const input = window.prompt(action === 'mkdir' ? '新文件夹名称' : '新名称', name ?? '')?.trim()
      if (!input) return
      if (!validName(input)) { onError(new Error('请输入单个文件或文件夹名称')); return }
      destination = joinRemote(target, input)
    }
    const description = action === 'rename' ? '重命名 ' + original + ' → ' + destination : action === 'mkdir' ? '创建 ' + destination : '删除 ' + original
    if (!confirmAccess(server, description) || (action.startsWith('remove') && !server.require_approval && !window.confirm(description + '？'))) return
    try {
      await ssh.mutate(server.id, action, action === 'mkdir' ? destination! : original, { ...access, approved: server.require_approval }, action === 'rename' ? destination : undefined)
      refresh(); onActivity()
    } catch (error) { onError(error) }
  }
  async function showPreview(name: string) {
    const file = joinRemote(target, name)
    try { setPreview({ path: file, ...await ssh.preview(server.id, file, access) }); onActivity() } catch (error) { onError(error) }
  }
  const parts = target.split('/').filter(Boolean)
  return <section className={css.explorer} hidden={hidden} aria-label="SFTP 文件" data-testid="ssh-sftp-drop"
    data-dragging={dragging} onDragOver={event => { if (event.dataTransfer.types.includes('Files')) { event.preventDefault(); setDragging(!!result); event.dataTransfer.dropEffect = result ? 'copy' : 'none' } }}
    onDragLeave={event => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDragging(false) }}
    onDrop={event => { if (event.dataTransfer.files.length) { event.preventDefault(); setDragging(false); void upload(event.dataTransfer.files) } }}>
    <header className={css.panelHeader}><h2>SFTP 文件</h2><div className={css.actions}>
      <button title="刷新目录" onClick={refresh} disabled={busy} aria-label="刷新目录">↻</button>
      <button title="新建文件夹" onClick={() => void mutate('mkdir')} disabled={!result || busy}>新建目录</button>
      <button onClick={() => listRef.current?.click()} disabled={!result || busy}>上传</button>
      <input ref={listRef} type="file" multiple hidden onChange={event => { if (event.target.files) void upload(event.target.files); event.target.value = '' }} />
    </div></header>
    {initialized && <nav className={css.breadcrumb} aria-label="远程目录">
      {target.startsWith('/') && <button onClick={() => navigate('/')}>/</button>}
      {parts.map((part, index) => <button key={index} title={part} aria-current={index === parts.length - 1 ? 'page' : undefined} onClick={() => navigate((target.startsWith('/') ? '/' : '') + parts.slice(0, index + 1).join('/'))}>{part}</button>)}
    </nav>}
    {!initialized ? <div className={css.empty}><button onClick={refresh}>打开远程目录</button></div> : busy ? <p className={css.muted} role="status">读取目录中…</p> : result && <ul className={css.entries}>
      {result.entries.length === 0 && <li className={css.muted}>空目录</li>}
      {result.entries.map(entry => <li key={entry.name}>
        <button className={css.fileName} title={entry.name} onClick={() => entry.kind === 'directory' ? navigate(joinRemote(target, entry.name)) : entry.kind === 'file' ? void showPreview(entry.name) : undefined} disabled={entry.kind !== 'directory' && entry.kind !== 'file'}>
          <span aria-hidden="true">{entry.kind === 'directory' ? '▸' : '·'}</span>{entry.name}
        </button>
        {entry.kind === 'file' && <small>{fileSize(entry.size)}</small>}
        <span className={css.actions}>
          {entry.kind === 'file' && <a aria-label={'下载 ' + entry.name} title="下载" href={ssh.download(server.id, joinRemote(target, entry.name), access)} download onClick={() => { window.setTimeout(onActivity, 1500) }}>↓</a>}
          <button aria-label={'重命名 ' + entry.name} title="重命名" onClick={() => void mutate('rename', entry.name)}>✎</button>
          <button aria-label={'删除 ' + entry.name} title="删除" onClick={() => void mutate(entry.kind === 'directory' ? 'remove_dir' : 'remove_file', entry.name)}>×</button>
        </span>
      </li>)}
    </ul>}
    {transfers.length > 0 && <ul className={css.transfers} aria-label="上传任务">{transfers.map(item => <li key={item.id}>
      <span title={item.name}>{item.name}</span><progress aria-label={item.name + ' 上传进度'} value={item.progress} max={1} /><small>{item.status}</small>
      {(item.status === '等待' || item.status === '上传中') && <button onClick={() => { item.controller.abort(); transferStatus(item.id, '已取消') }}>取消</button>}
    </li>)}</ul>}
    {preview && <div className={css.overlay} onClick={() => setPreview(undefined)}><section className={css.dialog} role="dialog" aria-modal="true" aria-label="文件预览" onClick={event => event.stopPropagation()}>
      <header className={css.panelHeader}><h2 title={preview.path}>{preview.path}</h2><button aria-label="关闭预览" onClick={() => setPreview(undefined)}>×</button></header>
      <pre>{preview.text}</pre>{preview.truncated && <p className={css.muted}>仅显示前 64 KiB</p>}
    </section></div>}
  </section>
}
