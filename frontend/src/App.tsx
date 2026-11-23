import { useCallback, useEffect, useRef, useState } from 'react'
import { ConversationPage, HeroPage, SkillPackagesPage, type Tab } from './components/AgentPage.tsx'
import { SkillWorkbench } from './components/SkillWorkbench.tsx'
import { PluginPageHost } from './components/PluginPageHost.tsx'
import { OfficeStudioView } from './components/OfficeStudioView.tsx'
import { CodeChatDock } from './components/CodeChatDock.tsx'
import { type AiFocus, type RemoteTool, remoteTools, SshContext } from './builtin_plugins/ssh/api.ts'
import { CodexDock } from './components/CodexDock.tsx'
import { CodexRightPanel } from './components/CodexRightPanel.tsx'
import { SettingsModal } from './components/SettingsModal.tsx'
import { WorkspaceModal } from './components/WorkspaceModal.tsx'
import { ShellSetupDialog } from './components/ShellSetupDialog.tsx'
import { isCodeShellContext } from './shellSetup.ts'
import type { AgentSlotProps } from './cordis/slot-contract.ts'
import { toggleTheme } from './theme.ts'
import { previewFile, type OpenFilePreview, type PendingOpen, type PreviewFile } from './filePreview.ts'
import { useAgentWorkspace } from './useAgentWorkspace.ts'
import css from './App.module.css'

const limitPanelWidth = (width: number) => Math.min(Math.floor(window.innerWidth * 0.65), 1100, Math.max(300, width))
type PreviewTabs = { taskId: string; files: PreviewFile[]; active: string | undefined }

/** Assemble the task model and the Codex 4-column layout + SettingsModal overlay. */
export function App({ renderSlot }: AgentSlotProps) {
  const workspace = useAgentWorkspace()
  const [tab, setTab] = useState<Tab>('chat')
  const [workbench, setWorkbench] = useState(false)
  const [git, setGit] = useState(false)
  const [ssh, setSsh] = useState(false)
  const [sshMounted, setSshMounted] = useState(false)
  const [followAi, setFollowAi] = useState(() => {
    try { return localStorage.getItem('ssh_follow_ai') !== 'false' } catch { return true }
  })
  const [aiFocus, setAiFocus] = useState<AiFocus>()
  const [sshChatOpenSignal, setSshChatOpenSignal] = useState(0)
  const sshObservation = useRef<{ taskId: string; seq: number; turn: string; openedTurn: string | undefined; calls: Set<string> }>()
  const [sshDraft, setSshDraft] = useState<{ text: string }>()
  const [office, setOffice] = useState(false)
  const [officeMounted, setOfficeMounted] = useState(false)
  const [officeResetSignal, setOfficeResetSignal] = useState(0)
  const [dark, setDark] = useState(() => document.body.hasAttribute('data-ds-dark-theme'))
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false)
  const [sidebarWidth, setSidebarWidth] = useState(() => {
    const saved = localStorage.getItem('codex_sidebar_width')
    return saved ? Number(saved) : 276
  })
  const [rightPanelOpen, setRightPanelOpen] = useState(false)
  const [rightPanelWidth, setRightPanelWidth] = useState(() => {
    const saved = localStorage.getItem('codex_right_panel_width')
    return limitPanelWidth((saved && Number(saved)) || 380)
  })
  const [newChatVersion, setNewChatVersion] = useState(0)
  const [mode, setMode] = useState<'preview' | 'changes'>('changes')
  const [preview, setPreview] = useState<PreviewTabs>()
  const [pendingOpen, setPendingOpen] = useState<PendingOpen>()
  const taskId = workspace.taskId
  useEffect(() => {
    if (sshObservation.current?.taskId !== taskId) sshObservation.current = undefined
    if (!taskId || workspace.session.task?.task_id !== taskId) return
    const events = workspace.session.events
    if (!sshObservation.current) {
      const launch = workspace.newTaskLaunch.current
      const locallyLaunched = launch?.taskId === taskId
      // Ignore the first history snapshot, except events from a prompt we just submitted.
      const baseline = locallyLaunched ? events.filter(event => event.time < launch.at).at(-1) : events.at(-1)
      sshObservation.current = { taskId, seq: baseline?.seq ?? -1, turn: '', openedTurn: undefined, calls: new Set() }
      if (locallyLaunched) workspace.newTaskLaunch.current = undefined
    }
    const observation = sshObservation.current
    let latest: AiFocus | undefined
    let open = false
    for (const event of events) {
      const { data } = event
      if (event.type === 'turn/start' || event.type === 'user/message') observation.turn = String(data.turn ?? event.seq)
      if (event.seq <= observation.seq || event.type !== 'tool/call') continue
      const tool = data.name as RemoteTool
      if (!remoteTools.includes(tool)) continue
      const turn = String(data.turn ?? observation.turn)
      const callId = turn + ':' + String(data.callId ?? data.call_id ?? event.seq)
      if (observation.calls.has(callId)) continue
      observation.calls.add(callId)
      if (!followAi) continue
      let args: Record<string, unknown>
      try {
        const value: unknown = typeof data.arguments === 'string' ? JSON.parse(data.arguments) : data.arguments ?? data.args
        if (!value || typeof value !== 'object' || Array.isArray(value)) continue
        args = value as Record<string, unknown>
      } catch { continue }
      if (typeof args.server_id !== 'string' || !args.server_id.trim()) continue
      const remotePath = [args.cwd, args.path, args.remote_path].find((value): value is string => typeof value === 'string' && !!value.trim())?.trim()
      latest = { seq: event.seq, taskId, serverId: args.server_id.trim(), tool, ...(remotePath ? { remotePath } : {}) }
      if (observation.openedTurn !== turn) { observation.openedTurn = turn; open = true }
    }
    observation.seq = Math.max(observation.seq, events.at(-1)?.seq ?? -1)
    if (latest) setAiFocus(latest)
    if (open) {
      setSsh(true); setSshMounted(true); setOffice(false); setGit(false); setWorkbench(false)
      setSshChatOpenSignal(value => value + 1)
    }
  }, [taskId, workspace.session.task, workspace.session.events, workspace.newTaskLaunch, followAi])
  const taskRoot = workspace.task?.workspace ?? ''
  const tabs = preview?.taskId === taskId ? preview : undefined
  const openPreview = useCallback<OpenFilePreview>((path, options) => {
    if (!taskId) return
    const file = previewFile(taskId, path, options, taskRoot)
    if (!file) return
    setPreview(current => {
      const files = current?.taskId === taskId ? current.files : []
      return { taskId, active: file.url, files: files.some(item => item.url === file.url)
        ? files.map(item => item.url === file.url ? file : item) : [...files, file] }
    })
    setMode('preview')
    setRightPanelOpen(true)
    setRightPanelWidth(width => limitPanelWidth(width < 520 ? 560 : width))
  }, [taskId, taskRoot])
  const closePreview = (url: string) => setPreview(current => {
    if (!current) return current
    const index = current.files.findIndex(file => file.url === url)
    const files = current.files.filter(file => file.url !== url)
    return { ...current, files, active: current.active === url ? files[Math.min(index, files.length - 1)]?.url : current.active }
  })
  const editPreview = (slotId: string, file: PreviewFile) => {
    setPendingOpen(prev => ({ slotId, fileUrl: file.url, filename: file.name, seq: (prev?.seq ?? 0) + 1 }))
    setOffice(true)
    setSsh(false)
    setSshDraft(undefined)
    setOfficeMounted(true)
    setGit(false)
    setWorkbench(false)
  }
  useEffect(() => {
    const resize = () => setRightPanelWidth(limitPanelWidth)
    window.addEventListener('resize', resize)
    return () => window.removeEventListener('resize', resize)
  }, [])

  const updateSidebarWidth = (w: number) => {
    setSidebarWidth(w)
    try { localStorage.setItem('codex_sidebar_width', String(w)) } catch {}
  }

  const updateRightPanelWidth = (w: number) => {
    setRightPanelWidth(w)
    try { localStorage.setItem('codex_right_panel_width', String(w)) } catch {}
  }

  useEffect(() => {
    const syncTheme = () => {
      setDark(document.body.hasAttribute('data-ds-dark-theme'))
    }
    const observer = new MutationObserver(syncTheme)
    observer.observe(document.body, { attributes: true, attributeFilter: ['data-ds-dark-theme'] })
    return () => { observer.disconnect() }
  }, [])

  const handleToggleCollapse = () => {
    setSidebarCollapsed(prev => !prev)
  }

  const handleNewChat = () => {
    setOffice(false)
    setWorkbench(false)
    setGit(false)
    setSsh(false)
    setSshDraft(undefined)
    setNewChatVersion(version => version + 1)
    setTab('chat')
    workspace.startNewSession()
  }

  const sidebar = renderSlot('agent.sidebar', {
    tasks: workspace.tasks,
    allTasks: workspace.allTasks,
    projects: workspace.projects,
    mode: workspace.sidebarMode,
    onModeChange: workspace.changeSidebarMode,
    onRemoveWorkspace: workspace.removeWorkspace,
    onRenameWorkspace: workspace.renameWorkspace,
    onUpdateSession: workspace.updateSession,
    onDeleteSession: workspace.deleteSession,
    selectedId: workspace.isChild ? workspace.parentId : workspace.taskId,
    activeTaskId: workspace.taskId,
    workspace: workspace.activeWorkspace ?? workspace.info?.workspace ?? '',
    workspaces: workspace.workspaces,
    activeWorkspace: workspace.activeWorkspace,
    onSelectWorkspace: workspace.selectWorkspace,
    onAddWorkspace: workspace.openFolderDialog,
    dark,
    collapsed: sidebarCollapsed,
    width: sidebarWidth,
    onToggleCollapse: handleToggleCollapse,
    onNew: handleNewChat,
    onSelect: id => { setTab('chat'); workspace.openTask(id) },
    onToggleTheme: () => {
      toggleTheme()
      setDark(document.body.hasAttribute('data-ds-dark-theme'))
    },
    onOpenSettings: workspace.openSettings,
  })

  const gitWorkspace = workspace.task?.workspace ?? workspace.activeWorkspace ?? workspace.info?.workspace ?? ''
  const actualSidebarWidth = sidebarCollapsed ? 0 : sidebarWidth
  const openSshChat = (id: string) => { setSsh(false); setSshDraft(undefined); setGit(false); setWorkbench(false); setTab('chat'); workspace.openTask(id) }

  return (
    <div className={css.appFrame} data-ds-dark-theme={dark ? '' : undefined}>
      {/* 1. Activity Dock (48px fixed on left) */}
      <CodexDock
        activeTab={office ? 'office' : ssh ? 'ssh' : git ? 'git' : workbench || tab === 'skill-pages' ? 'skills' : 'home'}
        onSelectTab={selected => {
          setSsh(selected === 'ssh')
          if (selected === 'ssh') setSshMounted(true)
          if (selected !== 'ssh') setSshDraft(undefined)
          if (selected === 'office') {
            if (office) setOfficeResetSignal(n => n + 1)
            setOffice(true)
            setOfficeMounted(true)
            setGit(false)
            setWorkbench(false)
            return
          }
          setOffice(false)
          setGit(selected === 'git')
          if (selected === 'skills') setWorkbench(true)
          else if (selected === 'home') { setWorkbench(false); if (tab === 'skill-pages') handleNewChat() }
        }}
        dark={dark}
        onNewChat={handleNewChat}
        onOpenSettings={workspace.openSettings}
        onToggleTheme={() => {
          toggleTheme()
          setDark(document.body.hasAttribute('data-ds-dark-theme'))
        }}
      />

      {/* 2. Codex Sidebar (collapsible & resizable) */}
      {!sidebarCollapsed && !workbench && !git && !ssh && !office && (
        <div className={css.sidebarCol} style={{ width: actualSidebarWidth }}>
          {sidebar}
        </div>
      )}

      {/* Drag handle for Sidebar */}
      {!sidebarCollapsed && !workbench && !git && !ssh && !office && (
        <div
          className={css.dragHandle}
          onPointerDown={(e) => {
            e.preventDefault()
            const startX = e.clientX
            const startW = sidebarWidth
            const onMove = (moveEvt: PointerEvent) => {
              const nextW = Math.min(420, Math.max(200, startW + (moveEvt.clientX - startX)))
              updateSidebarWidth(nextW)
            }
            const onUp = () => {
              window.removeEventListener('pointermove', onMove)
              window.removeEventListener('pointerup', onUp)
            }
            window.addEventListener('pointermove', onMove)
            window.addEventListener('pointerup', onUp)
          }}
        />
      )}

      {/* 3. Center Main Column (Header + Conversation + Floating Composer) */}
      <div className={css.centerCol}>
        {officeMounted && (
          <div style={{ display: office ? 'flex' : 'none', flex: 1, minHeight: 0, minWidth: 0 }}>
            <OfficeStudioView
              workspaceRoot={gitWorkspace}
              resetSignal={officeResetSignal}
              pendingOpen={pendingOpen}
              theme={dark ? 'dark' : 'light'}
              onManagePlugins={() => {
                setOffice(false)
                setWorkbench(false)
                setGit(false)
                setTab('skill-pages')
              }}
            />
          </div>
        )}
        {sshMounted && <div className={css.dockedView} style={{ display: ssh ? 'flex' : 'none' }}>
          <SshContext.Provider value={{ access: { ...(taskId ? { task_id: taskId } : {}) }, openTask: openSshChat,
            aiFocus: aiFocus?.taskId === taskId ? aiFocus : undefined, followAi,
            onToggleFollowAi: enabled => {
              setFollowAi(enabled)
              if (!enabled) setAiFocus(undefined)
              try { localStorage.setItem('ssh_follow_ai', String(enabled)) } catch {}
            },
            titleFor: id => (workspace.task?.task_id === id ? workspace.task : workspace.allTasks.find(task => task.task_id === id))?.title ?? undefined,
            ...(gitWorkspace ? { diagnose: (text: string) => setSshDraft({ text }) } : {}) }}>
            <div className={css.dockedPage}><PluginPageHost owner="ssh" page="workbench" workspaceRoot={gitWorkspace} theme={dark ? 'dark' : 'light'} /></div>
          </SshContext.Provider>
          {gitWorkspace && <CodeChatDock key="ssh" workspace={gitWorkspace} renderSlot={renderSlot} onOpenInChat={openSshChat} diagnostic={sshDraft} embeddedLauncher linkedTaskId={taskId} linkedSession={workspace.session} openSignal={sshChatOpenSignal} />}
        </div>}
        {!office && !ssh && (git ? <div className={css.dockedView}>
          <div className={css.dockedPage}><PluginPageHost owner="git" page="history" workspaceRoot={gitWorkspace} /></div>
          {gitWorkspace && <CodeChatDock key="git" workspace={gitWorkspace} renderSlot={renderSlot} onOpenInChat={id => { setGit(false); setWorkbench(false); setTab('chat'); workspace.openTask(id) }} />}
        </div>
          : workbench ? <SkillWorkbench tasks={workspace.allTasks} projects={workspace.projects} defaultWorkspace={workspace.info?.workspace ?? ''} onOpenChat={id => { setWorkbench(false); setTab('chat'); workspace.openTask(id) }} onPackages={() => { setWorkbench(false); setTab('skill-pages') }} /> : tab === 'skill-pages' ? <SkillPackagesPage workspace={workspace} onBack={() => setTab('chat')} /> : workspace.heroPhase ? (
          <HeroPage key={newChatVersion} workspace={workspace} renderSlot={renderSlot} />
        ) : (
          <ConversationPage
            workspace={workspace}
            renderSlot={renderSlot}
            tab={tab}
            setTab={setTab}
            rightPanelOpen={rightPanelOpen}
            onFilePreview={openPreview}
            onToggleRightPanel={() => setRightPanelOpen(v => !v)}
          />
        ))}
      </div>

      {/* Drag handle for Right Panel */}
      {rightPanelOpen && !workspace.heroPhase && tab !== 'skill-pages' && !workbench && !git && !ssh && !office && (
        <div
          className={css.dragHandle}
          role="separator" aria-orientation="vertical" aria-label="调整预览抽屉宽度"
          title="拖动调整面板宽度"
          onPointerDown={(e) => {
            e.preventDefault()
            const startX = e.clientX
            const startW = rightPanelWidth
            const onMove = (moveEvt: PointerEvent) => {
              const nextW = limitPanelWidth(startW + startX - moveEvt.clientX)
              updateRightPanelWidth(nextW)
            }
            const onUp = () => {
              window.removeEventListener('pointermove', onMove)
              window.removeEventListener('pointerup', onUp)
            }
            window.addEventListener('pointermove', onMove)
            window.addEventListener('pointerup', onUp)
          }}
        />
      )}

      {/* 4. Right Inspector Panel (Changes, Sources) */}
      {rightPanelOpen && !workspace.heroPhase && tab !== 'skill-pages' && !workbench && !git && !ssh && !office && (
        <CodexRightPanel
          width={rightPanelWidth}
          mode={tabs ? mode : 'changes'}
          files={tabs?.files}
          activeFile={tabs?.active}
          theme={dark ? 'dark' : 'light'}
          onMode={next => {
            if (next === 'preview' && taskId) setPreview(current => current?.taskId === taskId ? current : { taskId, files: [], active: undefined })
            setMode(next)
          }}
          onSelectFile={url => setPreview(current => current ? { ...current, active: url } : current)}
          onCloseFile={closePreview}
          onFilePreview={openPreview}
          onEdit={editPreview}
          taskId={workspace.taskId}
          running={workspace.running}
          onChangesUpdated={workspace.session.reload}
          workspacePath={workspace.task?.workspace ?? workspace.activeWorkspace ?? workspace.info?.workspace ?? ''}
          events={workspace.session.events}
          onClose={() => setRightPanelOpen(false)}
        />
      )}

      <ShellSetupDialog
        enabled={!git && !workbench && tab !== 'skill-pages' && !workspace.settingsOpen && isCodeShellContext(workspace.taskId, workspace.task, workspace.sidebarMode, workspace.activeWorkspace ?? '')}
        scope={workspace.taskId ?? workspace.activeWorkspace ?? ''}
        onReady={workspace.refreshInfoAndSettings}
      />
      <SettingsModal
        open={workspace.settingsOpen}
        activeTab={workspace.settingsTab}
        workspacePath={workspace.info?.workspace ?? ''}
        onSelectTab={workspace.setSettingsTab}
        onClose={workspace.closeSettings}
        onThemeChanged={() => { setDark(document.body.hasAttribute('data-ds-dark-theme')) }}
        onSaved={workspace.refreshInfoAndSettings}
      />

      <WorkspaceModal
        open={workspace.folderDialogOpen}
        onClose={workspace.closeFolderDialog}
        onAddWorkspace={workspace.addWorkspace}
      />
    </div>
  )
}
