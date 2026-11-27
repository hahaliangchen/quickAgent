import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import type { OfficeRecentDoc } from '../api.ts'
import { useAgentApi } from '../cordis/react.tsx'
import { systemFilePicker } from '../desktopFolder.ts'
import { writeClipboard } from '../dsh/ui-primitives/clipboard.ts'
import { resolveFileLinkPaths } from '../fileLinkPaths.ts'
import type { PendingOpen, PreviewTheme } from '../filePreview.ts'
import {
  OFFICE_FILE_EXTENSIONS,
  basenameOfPath,
  formatRelativeOpenedAt,
  inferOfficeSlotId,
  loadRecentOfficeFiles,
  officeLocalFileUrl,
  parentDirOfPath,
  removeRecentOfficeFile,
  saveRecentOfficeFiles,
  upsertRecentOfficeFile,
  type OfficeRecentFile,
} from '../officeRecentFiles.ts'
import { buildStudioCatalog, type StudioModuleCard, type StudioModuleIcon } from '../officeStudio.ts'
import type { StudioAskAiRequest, StudioViewContext } from '../plugins/protocol.ts'
import { usePluginPages } from '../plugins/react.tsx'
import { PluginPageHost } from './PluginPageHost.tsx'
import { StudioModuleIconImg } from './RichFileIcon.tsx'
import css from './OfficeStudioView.module.css'

function ModuleGlyph({ kind, size = 26 }: { kind: StudioModuleIcon; size?: number }) {
  return <StudioModuleIconImg kind={kind} size={size} />
}

function accentClass(accent: StudioModuleCard['accent']): string | undefined {
  switch (accent) {
    case 'ppt': return css.accentPpt
    case 'word': return css.accentWord
    case 'markdown': return css.accentMarkdown
    case 'excel': return css.accentExcel
    case 'pdf': return css.accentPdf
    case 'database': return css.accentDatabase
    default: return css.accentDefault
  }
}

function slotBadgeLabel(slotId: string, filename?: string): string {
  const ext = filename?.split('.').pop()?.toUpperCase()
  if (ext && ['MD', 'MARKDOWN', 'DOCX', 'XLSX', 'PPTX', 'PDF'].includes(ext)) {
    return ext === 'MARKDOWN' ? 'MD' : ext
  }
  switch (slotId) {
    case 'ppt': return 'PPTX'
    case 'word': return 'DOCX'
    case 'markdown': return 'MD'
    case 'excel': return 'XLSX'
    case 'pdf': return 'PDF'
    default: return 'DOC'
  }
}

function slotBadgeTone(slotId: string): string | undefined {
  switch (slotId) {
    case 'ppt': return css.accentPpt
    case 'word': return css.accentWord
    case 'markdown': return css.accentMarkdown
    case 'excel': return css.accentExcel
    case 'pdf': return css.accentPdf
    default: return css.accentDefault
  }
}

interface OpenedModule {
  readonly key: string
  readonly owner: string
  readonly page: string
  readonly reloadTick: number
  readonly request?: PendingOpen | undefined
}

export interface OfficeStudioViewProps {
  readonly workspaceRoot: string
  readonly resetSignal?: number
  readonly pendingOpen?: PendingOpen | undefined
  readonly theme?: PreviewTheme | undefined
  readonly onManagePlugins: () => void
  readonly onStudioContext?: ((context: StudioViewContext | null) => void) | undefined
  readonly onAskAi?: ((request: StudioAskAiRequest) => void) | undefined
}

/**
 * Unified Office & Creation Studio Hub.
 * - Shows a modular card grid automatically driven by bundled/enabled plugins (PPT, Word, Markdown, Excel, PDF...).
 * - Provides a WPS-style unified "文件" menu across all editors with local file open, recent files,
 *   workspace/chat document quick access, open containing folder, and absolute/relative path copy.
 */
export function OfficeStudioView({
  workspaceRoot,
  resetSignal,
  pendingOpen,
  theme,
  onManagePlugins,
  onStudioContext,
  onAskAi,
}: OfficeStudioViewProps) {
  const api = useAgentApi()
  const { packages } = usePluginPages()
  const catalog = useMemo(() => buildStudioCatalog(packages), [packages])
  const readyCards = useMemo(() => catalog.filter(item => item.ready && item.owner && item.page), [catalog])
  const [opened, setOpened] = useState<OpenedModule[]>([])
  const [activeKey, setActiveKey] = useState<string | null>(null)
  const moduleContexts = useRef<Map<string, StudioViewContext | null>>(new Map())

  const [fileMenuOpen, setFileMenuOpen] = useState(false)
  const [recentFiles, setRecentFiles] = useState<OfficeRecentFile[]>(() => loadRecentOfficeFiles())
  const [workspaceDocs, setWorkspaceDocs] = useState<readonly OfficeRecentDoc[]>([])
  const [loadingDocs, setLoadingDocs] = useState(false)
  const [manualPath, setManualPath] = useState('')
  const [statusNotice, setStatusNotice] = useState<{ text: string; tone: 'ok' | 'error' } | null>(null)

  const localSeqRef = useRef(100_000)
  const lastPendingSeqRef = useRef<number | undefined>(undefined)
  const noticeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const browserFileInputRef = useRef<HTMLInputElement>(null)
  const blobUrlsRef = useRef<string[]>([])

  const hasNativePicker = useMemo(() => systemFilePicker() !== undefined, [])

  const showNotice = useCallback((text: string, tone: 'ok' | 'error' = 'ok') => {
    if (noticeTimerRef.current !== null) clearTimeout(noticeTimerRef.current)
    setStatusNotice({ text, tone })
    noticeTimerRef.current = setTimeout(() => {
      setStatusNotice(null)
      noticeTimerRef.current = null
    }, 2400)
  }, [])

  useEffect(() => () => {
    if (noticeTimerRef.current !== null) clearTimeout(noticeTimerRef.current)
    for (const url of blobUrlsRef.current) URL.revokeObjectURL(url)
    blobUrlsRef.current = []
  }, [])

  const recordRecent = useCallback((entry: {
    path?: string | undefined
    filename: string
    slotId: string
    fileUrl: string
  }) => {
    // Ephemeral browser blob: URLs do not survive reload, so only persist durable URLs / disk paths.
    if (entry.fileUrl.startsWith('blob:') && !entry.path) return
    setRecentFiles(prev => {
      const next = upsertRecentOfficeFile(prev, entry)
      saveRecentOfficeFiles(next)
      return next
    })
  }, [])

  const refreshWorkspaceDocs = useCallback(() => {
    if (typeof api.recentOfficeDocs !== 'function') return
    setLoadingDocs(true)
    api.recentOfficeDocs(workspaceRoot || undefined).then(
      res => { setWorkspaceDocs(res.items ?? []); setLoadingDocs(false) },
      () => { setLoadingDocs(false) },
    )
  }, [api, workspaceRoot])

  useEffect(() => {
    if (activeKey === null || fileMenuOpen) {
      refreshWorkspaceDocs()
    }
  }, [activeKey, fileMenuOpen, refreshWorkspaceDocs])

  useEffect(() => {
    if (!fileMenuOpen) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        setFileMenuOpen(false)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [fileMenuOpen])

  useEffect(() => {
    onStudioContext?.(activeKey ? (moduleContexts.current.get(activeKey) ?? null) : null)
  }, [activeKey, onStudioContext])

  const handleModuleContext = useCallback((key: string, ctx: StudioViewContext | null) => {
    moduleContexts.current.set(key, ctx)
    if (key === activeKey) onStudioContext?.(ctx)
  }, [activeKey, onStudioContext])

  // Clicking the left Dock "Office" icon while already inside a module returns to Hub overview.
  useEffect(() => {
    if (resetSignal && resetSignal > 0) {
      setActiveKey(null)
      setFileMenuOpen(false)
    }
  }, [resetSignal])

  // Drop any keep-alive pane whose plugin was disabled or uninstalled.
  useEffect(() => {
    const valid = new Set(readyCards.map(item => item.key))
    setOpened(prev => prev.filter(item => valid.has(item.key)))
    setActiveKey(prev => (prev && valid.has(prev) ? prev : null))
  }, [readyCards])

  const openDocumentRequest = useCallback((req: {
    slotId: string
    fileUrl: string
    filename: string
    filePath?: string | undefined
    seq?: number | undefined
  }) => {
    const card = readyCards.find(item => item.slotId === req.slotId)
    if (!card?.owner || !card.page) {
      showNotice(`当前未启用支持 ${req.filename} 的编辑器模块`, 'error')
      return false
    }
    const { key, owner, page } = card
    const nextRequest: PendingOpen = {
      slotId: req.slotId,
      fileUrl: req.fileUrl,
      filename: req.filename,
      ...(req.filePath ? { filePath: req.filePath } : {}),
      seq: req.seq ?? ++localSeqRef.current,
    }
    setOpened(prev => prev.some(item => item.key === key)
      ? prev.map(item => item.key === key ? { ...item, request: nextRequest } : item)
      : [...prev, { key, owner, page, reloadTick: 0, request: nextRequest }])
    setActiveKey(key)
    setFileMenuOpen(false)
    recordRecent({
      path: req.filePath,
      filename: req.filename,
      slotId: req.slotId,
      fileUrl: req.fileUrl,
    })
    return true
  }, [readyCards, recordRecent, showNotice])

  useEffect(() => {
    if (!pendingOpen || pendingOpen.seq === lastPendingSeqRef.current) return
    const card = readyCards.find(item => item.slotId === pendingOpen.slotId)
    if (!card?.owner || !card.page) return
    lastPendingSeqRef.current = pendingOpen.seq
    const { key, owner, page } = card
    setOpened(prev => prev.some(item => item.key === key)
      ? prev.map(item => item.key === key ? { ...item, request: pendingOpen } : item)
      : [...prev, { key, owner, page, reloadTick: 0, request: pendingOpen }])
    setActiveKey(key)
    recordRecent({
      path: pendingOpen.filePath,
      filename: pendingOpen.filename,
      slotId: pendingOpen.slotId,
      fileUrl: pendingOpen.fileUrl,
    })
  }, [pendingOpen, readyCards, recordRecent])

  const openLocalPath = useCallback((absPath: string) => {
    const clean = absPath.trim()
    if (!clean) return
    const filename = basenameOfPath(clean)
    const slotId = inferOfficeSlotId(filename)
    if (!slotId) {
      showNotice(`暂不支持打开该格式：${filename}（支持 .md / .docx / .xlsx / .pptx / .pdf）`, 'error')
      return
    }
    openDocumentRequest({
      slotId,
      fileUrl: officeLocalFileUrl(clean),
      filename,
      filePath: clean,
    })
  }, [openDocumentRequest, showNotice])

  const openModule = (card: StudioModuleCard) => {
    if (!card.ready || !card.owner || !card.page) return
    const owner = card.owner
    const page = card.page
    setOpened(prev => (
      prev.some(item => item.key === card.key)
        ? prev
        : [...prev, { key: card.key, owner, page, reloadTick: 0 }]
    ))
    setActiveKey(card.key)
    setFileMenuOpen(false)
  }

  const closeModule = (key: string) => {
    moduleContexts.current.delete(key)
    setOpened(prev => prev.filter(item => item.key !== key))
    setActiveKey(prev => (prev === key ? null : prev))
    setFileMenuOpen(false)
  }

  const reloadModule = (key: string) => {
    setOpened(prev => prev.map(item => (
      item.key === key ? { ...item, reloadTick: item.reloadTick + 1 } : item
    )))
  }

  const activeModule = useMemo(
    () => (activeKey ? opened.find(item => item.key === activeKey) : undefined),
    [opened, activeKey],
  )
  const currentFileName = activeModule?.request?.filename
  const currentFilePath = activeModule?.request?.filePath
  const currentFileDir = currentFilePath ? parentDirOfPath(currentFilePath) : ''
  const currentCopyPaths = useMemo(
    () => (currentFilePath
      ? resolveFileLinkPaths(currentFilePath, { workspaceRoot, isChatSession: !workspaceRoot })
      : undefined),
    [currentFilePath, workspaceRoot],
  )

  const handlePickLocalFile = useCallback(async () => {
    const picker = systemFilePicker()
    if (picker) {
      setFileMenuOpen(false)
      try {
        const picked = await picker({
          title: '打开办公文档',
          defaultPath: currentFileDir || workspaceRoot || undefined,
          filters: [
            { name: '所有支持的办公文档', extensions: [...OFFICE_FILE_EXTENSIONS] },
            { name: 'Markdown', extensions: ['md', 'markdown'] },
            { name: 'Word 文档', extensions: ['docx'] },
            { name: 'Excel 表格', extensions: ['xlsx'] },
            { name: 'PPT 演示文稿', extensions: ['pptx'] },
            { name: 'PDF 文档', extensions: ['pdf'] },
          ],
        })
        if (picked) openLocalPath(picked)
      } catch (err) {
        showNotice(`打开文件选择器失败：${err instanceof Error ? err.message : String(err)}`, 'error')
      }
      return
    }
    browserFileInputRef.current?.click()
  }, [currentFileDir, workspaceRoot, openLocalPath, showNotice])

  const handleBrowserFileSelected = useCallback((file: File | undefined) => {
    if (!file) return
    const slotId = inferOfficeSlotId(file.name)
    if (!slotId) {
      showNotice(`暂不支持打开该格式：${file.name}`, 'error')
      return
    }
    const url = URL.createObjectURL(file)
    blobUrlsRef.current.push(url)
    openDocumentRequest({
      slotId,
      fileUrl: url,
      filename: file.name,
    })
  }, [openDocumentRequest, showNotice])

  const handleRevealFolder = useCallback(async (targetPath: string) => {
    if (!targetPath) return
    setFileMenuOpen(false)
    try {
      const res = await api.revealOfficeFolder(targetPath)
      showNotice(`已打开所在文件夹：${res.dir}`)
    } catch (err) {
      showNotice(err instanceof Error ? err.message : '无法打开所在文件夹', 'error')
    }
  }, [api, showNotice])

  const handleCopyPath = useCallback(async (label: string, text: string) => {
    const ok = await writeClipboard(text)
    setFileMenuOpen(false)
    showNotice(ok ? `已复制${label}` : '复制失败', ok ? 'ok' : 'error')
  }, [showNotice])

  const handleRemoveRecent = useCallback((id: string) => {
    setRecentFiles(prev => {
      const next = removeRecentOfficeFile(prev, id)
      saveRecentOfficeFiles(next)
      return next
    })
  }, [])

  const handleClearRecent = useCallback(() => {
    setRecentFiles([])
    saveRecentOfficeFiles([])
  }, [])

  const openedSet = useMemo(() => new Set(opened.map(item => item.key)), [opened])

  const hubRecentItems = useMemo(() => {
    if (recentFiles.length > 0) return recentFiles.slice(0, 8)
    return workspaceDocs.slice(0, 6).flatMap(doc => {
      const slotId = inferOfficeSlotId(doc.filename)
      if (!slotId) return []
      return [{
        id: doc.path,
        path: doc.path,
        filename: doc.filename,
        dir: doc.dir,
        slotId,
        fileUrl: officeLocalFileUrl(doc.path),
        openedAt: doc.modified_ms,
      } satisfies OfficeRecentFile]
    })
  }, [recentFiles, workspaceDocs])

  return (
    <div className={css.root}>
      <input
        ref={browserFileInputRef}
        type="file"
        accept=".md,.markdown,.docx,.xlsx,.pptx,.pdf"
        hidden
        onChange={event => {
          handleBrowserFileSelected(event.target.files?.[0])
          event.target.value = ''
        }}
      />

      {activeKey !== null && (
        <header className={css.topBar}>
          <div className={css.barLeft}>
            <button
              type="button"
              className={css.backBtn}
              onClick={() => { setActiveKey(null); setFileMenuOpen(false) }}
              title="返回办公创作台总览（当前打开的编辑器保持运行）"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="m15 18-6-6 6-6" />
              </svg>
              <span>办公创作台</span>
            </button>

            <div className={css.fileMenuAnchor}>
              <button
                type="button"
                className={clsx(css.fileMenuBtn, fileMenuOpen && css.fileMenuBtnOpen)}
                aria-expanded={fileMenuOpen}
                aria-haspopup="menu"
                onClick={() => setFileMenuOpen(v => !v)}
                title="文件菜单：打开本地文件、最近打开、打开所在文件夹"
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M4 7h16M4 12h16M4 17h10" />
                </svg>
                <span>文件</span>
                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.3" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="m6 9 6 6 6-6" />
                </svg>
              </button>

              {fileMenuOpen && (
                <>
                  <div className={css.fileMenuShield} onClick={() => setFileMenuOpen(false)} />
                  <div className={css.fileMenuPopover} role="menu" aria-label="文件菜单">
                    <div className={css.menuSection}>
                      <button
                        type="button"
                        role="menuitem"
                        className={css.menuActionRow}
                        onClick={() => { void handlePickLocalFile() }}
                      >
                        <span className={css.menuIcon}>
                          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                            <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
                            <path d="M12 11v6m-3-3h6" />
                          </svg>
                        </span>
                        <span className={css.menuRowBody}>
                          <span className={css.menuRowTitle}>打开本地文件…</span>
                          <span className={css.menuRowSub}>支持 Markdown、Word、Excel、PPT、PDF</span>
                        </span>
                      </button>

                      <button
                        type="button"
                        role="menuitem"
                        disabled={!currentFilePath}
                        className={css.menuActionRow}
                        onClick={() => { if (currentFilePath) void handleRevealFolder(currentFilePath) }}
                      >
                        <span className={css.menuIcon}>
                          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                            <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
                            <path d="m12 11 4 4m0-4-4 4" opacity="0" />
                          </svg>
                        </span>
                        <span className={css.menuRowBody}>
                          <span className={css.menuRowTitle}>
                            打开所在文件夹{currentFileName ? `（${currentFileName}）` : ''}
                          </span>
                          <span className={css.menuRowSub} title={currentFileDir || undefined}>
                            {currentFileDir || '当前标签尚未打开本地或工作区文件'}
                          </span>
                        </span>
                      </button>

                      {currentCopyPaths && (currentCopyPaths.absolutePath || currentCopyPaths.relativePath) && (
                        <div className={css.menuCopyBar}>
                          {currentCopyPaths.absolutePath && (
                            <button
                              type="button"
                              className={css.menuCopyChip}
                              onClick={() => { void handleCopyPath('绝对路径', currentCopyPaths.absolutePath!) }}
                            >
                              复制绝对路径
                            </button>
                          )}
                          {currentCopyPaths.relativePath && (
                            <button
                              type="button"
                              className={css.menuCopyChip}
                              onClick={() => { void handleCopyPath('相对路径', currentCopyPaths.relativePath!) }}
                            >
                              复制相对路径
                            </button>
                          )}
                        </div>
                      )}

                      {!hasNativePicker && (
                        <form
                          className={css.manualPathForm}
                          onSubmit={event => {
                            event.preventDefault()
                            if (manualPath.trim()) {
                              openLocalPath(manualPath)
                              setManualPath('')
                            }
                          }}
                        >
                          <input
                            type="text"
                            className={css.manualPathInput}
                            value={manualPath}
                            placeholder="或粘贴本地文件绝对路径并回车…"
                            onChange={event => setManualPath(event.target.value)}
                          />
                          <button type="submit" className={css.manualPathBtn} disabled={!manualPath.trim()}>
                            打开
                          </button>
                        </form>
                      )}
                    </div>

                    <div className={css.menuDivider} />

                    <div className={css.menuSection}>
                      <div className={css.menuSectionHeader}>
                        <span>最近打开</span>
                        {recentFiles.length > 0 && (
                          <button type="button" className={css.menuClearBtn} onClick={handleClearRecent}>
                            清空
                          </button>
                        )}
                      </div>
                      {recentFiles.length === 0 ? (
                        <div className={css.menuEmpty}>暂无最近打开的文档</div>
                      ) : (
                        <div className={css.menuScrollList}>
                          {recentFiles.map(item => (
                            <div key={item.id} className={css.menuDocRow}>
                              <button
                                type="button"
                                className={css.menuDocMain}
                                onClick={() => {
                                  openDocumentRequest({
                                    slotId: item.slotId,
                                    fileUrl: item.path ? officeLocalFileUrl(item.path) : item.fileUrl,
                                    filename: item.filename,
                                    filePath: item.path,
                                  })
                                }}
                                title={item.path ?? item.filename}
                              >
                                <span className={clsx(css.docBadge, slotBadgeTone(item.slotId))}>
                                  {slotBadgeLabel(item.slotId, item.filename)}
                                </span>
                                <span className={css.menuDocMeta}>
                                  <span className={css.menuDocName}>{item.filename}</span>
                                  <span className={css.menuDocSub}>
                                    <span className={css.menuDocDir}>{item.dir || '对话预览'}</span>
                                    <span className={css.menuDocTime}>{formatRelativeOpenedAt(item.openedAt)}</span>
                                  </span>
                                </span>
                              </button>
                              {item.path && (
                                <button
                                  type="button"
                                  className={css.menuDocAction}
                                  title={`打开所在文件夹：${item.dir}`}
                                  aria-label={`打开 ${item.filename} 所在文件夹`}
                                  onClick={e => { e.stopPropagation(); void handleRevealFolder(item.path!) }}
                                >
                                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                                    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
                                  </svg>
                                </button>
                              )}
                              <button
                                type="button"
                                className={css.menuDocAction}
                                title="从最近列表移除"
                                aria-label={`移除 ${item.filename}`}
                                onClick={e => { e.stopPropagation(); handleRemoveRecent(item.id) }}
                              >
                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                                  <path d="M18 6 6 18M6 6l12 12" />
                                </svg>
                              </button>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>

                    <div className={css.menuDivider} />

                    <div className={css.menuSection}>
                      <div className={css.menuSectionHeader}>
                        <span>工作区与聊天文档</span>
                        {loadingDocs && <span className={css.menuLoading}>扫描中…</span>}
                      </div>
                      {workspaceDocs.length === 0 ? (
                        <div className={css.menuEmpty}>
                          {loadingDocs ? '正在查找工作区文档…' : '未在当前工作区或聊天目录发现文档'}
                        </div>
                      ) : (
                        <div className={css.menuScrollList}>
                          {workspaceDocs.slice(0, 8).map(doc => {
                            const slotId = inferOfficeSlotId(doc.filename) ?? 'markdown'
                            return (
                              <div key={doc.path} className={css.menuDocRow}>
                                <button
                                  type="button"
                                  className={css.menuDocMain}
                                  onClick={() => openLocalPath(doc.path)}
                                  title={doc.path}
                                >
                                  <span className={clsx(css.docBadge, slotBadgeTone(slotId))}>
                                    {slotBadgeLabel(slotId, doc.filename)}
                                  </span>
                                  <span className={css.menuDocMeta}>
                                    <span className={css.menuDocName}>{doc.filename}</span>
                                    <span className={css.menuDocSub}>
                                      <span className={css.menuDocDir}>{doc.dir}</span>
                                      <span className={css.menuDocTag}>
                                        {doc.source === 'chat-assets' ? '聊天文件' : '工作区'}
                                      </span>
                                    </span>
                                  </span>
                                </button>
                                <button
                                  type="button"
                                  className={css.menuDocAction}
                                  title={`打开所在文件夹：${doc.dir}`}
                                  aria-label={`打开 ${doc.filename} 所在文件夹`}
                                  onClick={e => { e.stopPropagation(); void handleRevealFolder(doc.path) }}
                                >
                                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                                    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
                                  </svg>
                                </button>
                              </div>
                            )
                          })}
                        </div>
                      )}
                    </div>
                  </div>
                </>
              )}
            </div>

            <span className={css.barDivider} aria-hidden="true" />
          </div>

          <nav className={css.barTabs} aria-label="已装配模块切换">
            {readyCards.map(card => {
              const isActive = card.key === activeKey
              const isLive = openedSet.has(card.key)
              return (
                <button
                  key={card.key}
                  type="button"
                  className={clsx(css.tabPill, isActive && css.tabPillActive)}
                  onClick={() => openModule(card)}
                >
                  <ModuleGlyph kind={card.icon} size={14} />
                  <span>{card.title}</span>
                  {isLive && <span className={css.liveDot} title="运行中（状态保活）" />}
                </button>
              )
            })}
          </nav>

          <div className={css.barRight}>
            {currentFilePath && (
              <button
                type="button"
                className={css.barAction}
                onClick={() => { void handleRevealFolder(currentFilePath) }}
                title={`打开所在文件夹：${currentFileDir}`}
              >
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
                </svg>
                <span>所在文件夹</span>
              </button>
            )}
            <button
              type="button"
              className={css.barAction}
              onClick={() => reloadModule(activeKey)}
              title="重新加载当前模块"
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M21 12a9 9 0 1 1-2.64-6.36L21 8" />
                <path d="M21 3v5h-5" />
              </svg>
              <span>重载</span>
            </button>
            <button
              type="button"
              className={css.barAction}
              onClick={() => closeModule(activeKey)}
              title="关闭当前标签并返回总览"
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
              <span>关闭</span>
            </button>
          </div>
        </header>
      )}

      {/* Hub overview grid (hidden while an active module is open, without unmounting keep-alive panes) */}
      {activeKey === null && (
        <div className={css.hubScroll}>
          <div className={css.hubInner}>
            <div className={css.hubHeader}>
              <div className={css.hubTitleBlock}>
                <span className={css.hubEyebrow}>QuickAgent Studio</span>
                <h1 className={css.hubTitle}>办公创作台</h1>
                <p className={css.hubSubtitle}>
                  模块化文档与应用套件，随包装配后自动点亮；切换页面时保持编辑现场不重载。
                </p>
              </div>
              <div className={css.hubActions}>
                <button
                  type="button"
                  className={css.primaryOpenBtn}
                  onClick={() => { void handlePickLocalFile() }}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
                  </svg>
                  <span>打开本地文件…</span>
                </button>
                <span className={css.countPill}>
                  已装配 {readyCards.length} 个模块
                </span>
                <button type="button" className={css.manageBtn} onClick={onManagePlugins}>
                  插件管理
                </button>
              </div>
            </div>

            <div className={css.grid}>
              {catalog.map(card => {
                const isRunning = openedSet.has(card.key)
                return (
                  <button
                    key={card.key}
                    type="button"
                    disabled={!card.ready}
                    className={clsx(css.card, !card.ready && css.cardPending)}
                    onClick={() => openModule(card)}
                  >
                    <div className={css.cardTop}>
                      <span className={clsx(css.iconTile, accentClass(card.accent))}>
                        <ModuleGlyph kind={card.icon} size={22} />
                      </span>
                      <div className={css.cardBadges}>
                        {card.formats.map(ext => (
                          <span key={ext} className={css.formatChip}>{ext}</span>
                        ))}
                        {isRunning ? (
                          <span className={css.statusRunning}>
                            <span className={css.statusDot} />
                            运行中
                          </span>
                        ) : card.ready ? (
                          <span className={css.statusReady}>
                            <span className={css.statusDot} />
                            已就绪
                          </span>
                        ) : (
                          <span className={css.statusPending}>待装配</span>
                        )}
                      </div>
                    </div>

                    <div className={css.cardBody}>
                      <h2 className={css.cardTitle}>{card.title}</h2>
                      <p className={css.cardDesc}>{card.subtitle}</p>
                    </div>

                    <div className={css.cardFooter}>
                      {card.ready ? (
                        <>
                          <span className={css.cardCta}>
                            {isRunning ? '切回编辑现场' : '打开工作台'}
                            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                              <path d="M5 12h14m-6-6 6 6-6 6" />
                            </svg>
                          </span>
                          <span className={css.cardMeta}>
                            {card.toolCount > 0 ? `${card.toolCount} 个 AI 工具` : '独立本地工作台'}
                          </span>
                        </>
                      ) : (
                        <>
                          <span className={css.cardMeta}>放入 {card.bundleDir} 自动启用</span>
                          <span className={css.cardMeta}>模块化即插即用</span>
                        </>
                      )}
                    </div>
                  </button>
                )
              })}
            </div>

            {hubRecentItems.length > 0 && (
              <section className={css.recentPanel} aria-label="最近文档">
                <div className={css.recentHeader}>
                  <div>
                    <h2 className={css.recentTitle}>
                      {recentFiles.length > 0 ? '最近打开' : '工作区与聊天文档'}
                    </h2>
                    <p className={css.recentSubtitle}>
                      一键回到上次预览或编辑的文档，也可直接打开文件所在文件夹。
                    </p>
                  </div>
                  {recentFiles.length > 0 && (
                    <button type="button" className={css.menuClearBtn} onClick={handleClearRecent}>
                      清空记录
                    </button>
                  )}
                </div>
                <div className={css.recentList}>
                  {hubRecentItems.map(item => (
                    <div key={item.id} className={css.recentRow}>
                      <button
                        type="button"
                        className={css.recentMain}
                        onClick={() => {
                          openDocumentRequest({
                            slotId: item.slotId,
                            fileUrl: item.path ? officeLocalFileUrl(item.path) : item.fileUrl,
                            filename: item.filename,
                            filePath: item.path,
                          })
                        }}
                        title={item.path ?? item.filename}
                      >
                        <span className={clsx(css.docBadge, slotBadgeTone(item.slotId))}>
                          {slotBadgeLabel(item.slotId, item.filename)}
                        </span>
                        <span className={css.recentName}>{item.filename}</span>
                        <span className={css.recentDir}>{item.dir || '对话预览'}</span>
                        <span className={css.recentTime}>{formatRelativeOpenedAt(item.openedAt)}</span>
                      </button>
                      {item.path && (
                        <button
                          type="button"
                          className={css.recentFolderBtn}
                          onClick={() => { void handleRevealFolder(item.path!) }}
                          title={`打开所在文件夹：${item.dir}`}
                        >
                          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                            <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
                          </svg>
                          <span>打开所在文件夹</span>
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              </section>
            )}

            <div className={css.hubFootnote}>
              <span>
                新模块放入 <code>bundled_plugins/&lt;id&gt;/</code> 后重启即自动生效，无需在设置中手动开启。
              </span>
              <span>已打开的模块在切回首页对话后仍保持后台保活</span>
            </div>
          </div>
        </div>
      )}

      {/* Keep-alive stack of opened plugin modules */}
      {opened.length > 0 && (
        <div className={clsx(css.stage, activeKey === null && css.stagePaneHidden)}>
          {opened.map(item => (
            <div
              key={`${item.key}:${item.reloadTick}`}
              className={clsx(css.stagePane, item.key !== activeKey && css.stagePaneHidden)}
            >
              <PluginPageHost
                owner={item.owner}
                page={item.page}
                workspaceRoot={workspaceRoot}
                pendingOpen={item.request}
                theme={theme}
                onStudioContext={ctx => handleModuleContext(item.key, ctx)}
                onAskAi={onAskAi}
              />
            </div>
          ))}
        </div>
      )}

      {statusNotice && (
        <div
          role="status"
          className={clsx(css.noticeToast, statusNotice.tone === 'error' && css.noticeToastError)}
        >
          {statusNotice.text}
        </div>
      )}
    </div>
  )
}
