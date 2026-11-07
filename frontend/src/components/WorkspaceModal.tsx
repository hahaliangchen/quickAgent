import { useEffect, useRef, useState } from 'react'
import { Modal } from '../dsh/ui-primitives/Modal.tsx'
import { IconCloseOutlineRegular, IconFolderOpenRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import { systemFolderPicker } from '../desktopFolder.ts'
import { workspaceName } from './Sidebar.tsx'
import css from './WorkspaceModal.module.css'

const ignoreClose = () => {}

function FolderPlusIcon() {
  return <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
    <path d="M12 10.5v5M9.5 13h5" />
  </svg>
}

/** Create a project from a folder: the desktop shell opens the system chooser; a browser falls back to a typed path. */
export function WorkspaceModal({ open, onClose, onAddWorkspace }: {
  open: boolean
  onClose: () => void
  onAddWorkspace: (path: string, title?: string) => Promise<void>
}) {
  const [name, setName] = useState('')
  const [path, setPath] = useState('')
  const [typing, setTyping] = useState(false)
  const [picking, setPicking] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const nameEdited = useRef(false)

  useEffect(() => {
    if (open) { setName(''); setPath(''); setTyping(false); setPicking(false); setError(null); nameEdited.current = false }
  }, [open])

  // The folder's name stands in as the project name until the user types their own.
  const suggestName = (folder: string) => { if (!nameEdited.current) setName(folder.trim() === '' ? '' : workspaceName(folder)) }

  const pickFolder = async () => {
    const pick = systemFolderPicker()
    if (pick === undefined) { setTyping(true); return }
    setPicking(true)
    setError(null)
    try {
      const folder = await pick('选择项目文件夹')
      if (folder !== null) { setPath(folder); suggestName(folder) }
    } catch (cause) {
      setTyping(true)
      setError(`无法打开系统文件夹窗口，请输入文件夹路径（${cause instanceof Error ? cause.message : String(cause)}）`)
    } finally { setPicking(false) }
  }

  const removeFolder = () => { setPath(''); suggestName(''); setError(null) }

  const confirm = async () => {
    if (!name.trim()) { setError('请输入项目名称'); return }
    if (!path.trim()) { setError('请添加源文件夹'); return }
    setBusy(true)
    setError(null)
    try { await onAddWorkspace(path.trim(), name.trim()); onClose() }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  const locked = busy || picking
  return <Modal open={open} onClose={locked ? ignoreClose : onClose} title="创建项目" headless className={css.dialog!}>
    <form className={css.form} onSubmit={event => { event.preventDefault(); void confirm() }}>
      <div className={css.header}>
        <h2>创建项目</h2>
        <button className={css.close} type="button" aria-label="关闭" disabled={locked} onClick={onClose}><IconCloseOutlineRegular size={18} /></button>
      </div>
      <label className={css.nameField}>
        <span className={css.nameIcon}><IconFolderOpenRegular size={19} /></span>
        <input autoFocus aria-label="项目名称" placeholder="项目名称" value={name} onChange={event => { nameEdited.current = true; setName(event.target.value); setError(null) }} />
      </label>
      <div className={css.sourceLabel}>源文件夹</div>
      <div className={css.sourceBox}>
        {path !== '' && !typing ? <div className={css.picked}>
          <span className={css.pickedIcon}><IconFolderOpenRegular size={20} /></span>
          <span className={css.pickedText}>
            <strong>{workspaceName(path)}</strong>
            <small title={path}>{path}</small>
          </span>
          <button className={css.change} type="button" disabled={locked} onClick={() => void pickFolder()}>更换</button>
          <button className={css.remove} type="button" aria-label="移除文件夹" disabled={locked} onClick={removeFolder}><IconCloseOutlineRegular size={14} /></button>
        </div> : typing ? <div className={css.pathArea}>
          <label htmlFor="project-folder-path">文件夹路径</label>
          <input id="project-folder-path" autoFocus placeholder="输入或粘贴文件夹的完整路径" value={path}
            onChange={event => { setPath(event.target.value); setError(null) }} onBlur={() => suggestName(path)} />
        </div> : <>
          <div className={css.sourceHint}>在此电脑上添加文件夹</div>
          <button className={css.addButton} type="button" disabled={locked} onClick={() => void pickFolder()}><FolderPlusIcon /> 添加</button>
        </>}
      </div>
      {error && <p className={css.error} role="alert">{error}</p>}
      <div className={css.footer}>
        <button type="button" className={css.cancel} disabled={locked} onClick={onClose}>取消</button>
        <button type="submit" className={css.create} disabled={locked}>{busy ? '正在创建…' : '创建项目'}</button>
      </div>
    </form>
  </Modal>
}
