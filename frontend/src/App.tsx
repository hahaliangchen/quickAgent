import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { ConversationPage, HeroPage, SkillPackagesPage, type Tab } from './components/AgentPage.tsx'
import { SkillWorkbench } from './components/SkillWorkbench.tsx'
import { PluginPageHost } from './components/PluginPageHost.tsx'
import { OfficeStudioView } from './components/OfficeStudioView.tsx'
import { BLUEPRINT_OBSERVER_ENABLED, BlueprintObserverHost } from './experimental/blueprintObserver/index.ts'
import type { StudioAskAiRequest, StudioViewContext } from './plugins/protocol.ts'
import { BlueprintContext } from './builtin_plugins/blueprint/api.ts'
import { buildDistillPlaybookPrompt } from './builtin_plugins/blueprint/playbookPrompts.ts'
import type { Task } from './api.ts'
import { CodeChatDock } from './components/CodeChatDock.tsx'
import { type AiFocus, type RemoteTool, remoteTools, SshContext } from './builtin_plugins/ssh/api.ts'
import { announceDraftReady } from './builtin_plugins/daily-report/api.ts'
import { CodexDock } from './components/CodexDock.tsx'
import { CodexRightPanel } from './components/CodexRightPanel.tsx'
import { SettingsModal } from './components/SettingsModal.tsx'
import { WorkspaceModal } from './components/WorkspaceModal.tsx'
import { ShellSetupDialog } from './components/ShellSetupDialog.tsx'
import { AppDialogHost } from './components/AppDialog.tsx'
import { startHorizontalResize } from './components/dragResize.ts'
import { isCodeShellContext } from './shellSetup.ts'
import type { AgentSlotProps } from './cordis/slot-contract.ts'
import { toggleTheme } from './theme.ts'
import { usePluginPages } from './plugins/react.tsx'
import { previewFile, previewModule, type OpenFilePreview, type PendingOpen, type PreviewFile } from './filePreview.ts'
import { useAgentWorkspace } from './useAgentWorkspace.ts'
import css from './App.module.css'

const limitPanelWidth = (width: number) => Math.min(Math.floor(window.innerWidth * 0.65), 1100, Math.max(300, width))
type PreviewTabs = { taskId: string; files: PreviewFile[]; active: string | undefined }

/** Keep hidden views mounted (preserving PTY/WebSocket & scroll) without re-rendering during background streaming. */
const HiddenFreeze = memo(
  ({ children }: { hidden: boolean; children: ReactNode }) => <>{children}</>,
  (prev, next) => prev.hidden && next.hidden,
)

/** Assemble the task model and the Codex 4-column layout + SettingsModal overlay. */
export function App({ renderSlot }: AgentSlotProps) {
  const workspace = useAgentWorkspace()
  const { packages: pluginPackages } = usePluginPages()
  const [tab, setTab] = useState<Tab>('chat')
  const [workbench, setWorkbench] = useState(false)
  const [workbenchMode, setWorkbenchMode] = useState<'flows' | 'blueprint'>('flows')
  const [git, setGit] = useState(false)
  const [blueprintWorkspace, setBlueprintWorkspace] = useState('')
  const [blueprintDraft, setBlueprintDraft] = useState<{ text: string; kind: 'prompt' }>()
  const [blueprintChatOpenSignal, setBlueprintChatOpenSignal] = useState(0)
  const sendBlueprintDraft = useCallback((text: string) => {
    setBlueprintDraft({ text, kind: 'prompt' })
    setBlueprintChatOpenSignal(value => value + 1)
  }, [])
  const selectBlueprintWorkspace = useCallback((path: string) => {
    setBlueprintWorkspace(path)
    setBlueprintDraft(undefined)
  }, [])
  const distillTaskToBlueprint = useCallback((task: Task) => {
    const targetWorkspace = task.workspace || workspace.activeWorkspace || workspace.info?.workspace || ''
    if (targetWorkspace) setBlueprintWorkspace(targetWorkspace)
    setOffice(false); setSsh(false); setGit(false); setWorkbench(true); setWorkbenchMode('blueprint')
    sendBlueprintDraft(buildDistillPlaybookPrompt([task]))
  }, [workspace.activeWorkspace, workspace.info?.workspace, sendBlueprintDraft])
  const [ssh, setSsh] = useState(false)
  const [sshMounted, setSshMounted] = useState(false)
  const [followAi, setFollowAi] = useState(() => {
    try { return localStorage.getItem('ssh_follow_ai') !== 'false' } catch { return true }
  })
  const [aiFocus, setAiFocus] = useState<AiFocus>()
  const [sshChatOpenSignal, setSshChatOpenSignal] = useState(0)
  const sshObservation = useRef<{ taskId: string; seq: number; turn: string; calls: Set<string> }>()
  // Auto-jump to SSH workbench only once when a session first connects to a server
  // during this app lifecycle; subsequent commands on that active connection update
  // aiFocus silently without stealing focus from Home.
  const sshAutoOpenedRef = useRef<Set<string>>(new Set())
  const [sshDraft, setSshDraft] = useState<{ text: string }>()
  const [sshViewContext, setSshViewContext] = useState<StudioViewContext | null>(null)
  // A prepared daily-report draft brings its page forward, the way an SSH
  // command opens the remote terminal.
  const [dailyReportPage, setDailyReportPage] = useState<{ owner: string; seq: number }>()
  const dailyReportWatch = useRef<{ taskId: string; seq: number } | null>(null)
  const [office, setOffice] = useState(false)
  const [officeMounted, setOfficeMounted] = useState(false)
  const [officeResetSignal, setOfficeResetSignal] = useState(0)
  const [officeViewContext, setOfficeViewContext] = useState<StudioViewContext | null>(null)
  const [officeDraft, setOfficeDraft] = useState<{ text: string; kind: 'prompt' }>()
  const [officeChatOpenSignal, setOfficeChatOpenSignal] = useState(0)
  const handleOfficeAskAi = useCallback((req: StudioAskAiRequest) => {
    if (req.prompt && req.prompt.trim()) setOfficeDraft({ text: req.prompt, kind: 'prompt' })
    setOfficeChatOpenSignal(value => value + 1)
  }, [])
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
      sshObservation.current = { taskId, seq: baseline?.seq ?? -1, turn: '', calls: new Set() }
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
      const serverId = args.server_id.trim()
      const remotePath = [args.cwd, args.path, args.remote_path].find((value): value is string => typeof value === 'string' && !!value.trim())?.trim()
      latest = { seq: event.seq, taskId, serverId, tool, ...(remotePath ? { remotePath } : {}) }
      const connectionKey = `${taskId}:${serverId}`
      if (!sshAutoOpenedRef.current.has(connectionKey)) {
        sshAutoOpenedRef.current.add(connectionKey)
        open = true
      }
    }
    observation.seq = Math.max(observation.seq, events.at(-1)?.seq ?? -1)
    if (latest) setAiFocus(latest)
    if (open) {
      setSsh(true); setSshMounted(true); setOffice(false); setGit(false); setWorkbench(false)
      setSshChatOpenSignal(value => value + 1)
    }
  }, [taskId, workspace.session.task, workspace.session.events, workspace.newTaskLaunch, followAi])
  // A draft prepared during this session brings its own page forward. The
  // baseline is the session's high-water mark, so reopening a conversation that
  // already carried a draft does not move the user anywhere.
  useEffect(() => {
    const session = workspace.session
    // Wait for the history to load, otherwise every replayed event looks new.
    if (!taskId || session.task?.task_id !== taskId) return
    const events = session.events
    const last = events.at(-1)?.seq ?? -1
    const baseline = dailyReportWatch.current
    if (!baseline || baseline.taskId !== taskId) {
      dailyReportWatch.current = { taskId, seq: last }
      return
    }
    const ready = events.filter(event => event.type === 'daily_report/draft_ready' && event.seq > baseline.seq).at(-1)
    if (last > baseline.seq) dailyReportWatch.current = { taskId, seq: last }
    if (!ready) return
    setSsh(false); setGit(false); setOffice(false); setWorkbench(true); setWorkbenchMode('flows')
    setDailyReportPage({ owner: 'daily-report', seq: ready.seq })
    announceDraftReady()
  }, [taskId, workspace.session])
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
  const editPreview = useCallback((slotId: string, file: PreviewFile) => {
    setPendingOpen(prev => ({
      slotId,
      fileUrl: file.url,
      filename: file.name,
      ...(file.diskPath ? { filePath: file.diskPath } : {}),
      seq: (prev?.seq ?? 0) + 1,
    }))
    setOffice(true)
    setSsh(false)
    setSshDraft(undefined)
    setOfficeMounted(true)
    setGit(false)
    setWorkbench(false)
  }, [])
  const openDockFile = useCallback((targetTaskId: string, path: string, options?: { line?: number; url?: string }, root = '') => {
    const file = previewFile(targetTaskId, path, options, root)
    if (!file) return
    const mod = previewModule(file.name, pluginPackages)
    if (mod) {
      editPreview(mod.slotId, file)
      return
    }
    setOffice(false); setSsh(false); setSshDraft(undefined); setGit(false); setWorkbench(false)
    setTab('chat')
    if (targetTaskId !== taskId) workspace.openTask(targetTaskId)
    setPreview(current => {
      const files = current?.taskId === targetTaskId ? current.files : []
      return { taskId: targetTaskId, active: file.url, files: files.some(item => item.url === file.url)
        ? files.map(item => item.url === file.url ? file : item) : [...files, file] }
    })
    setMode('preview')
    setRightPanelOpen(true)
    setRightPanelWidth(width => limitPanelWidth(width < 520 ? 560 : width))
  }, [pluginPackages, editPreview, taskId, workspace.openTask])
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
    setBlueprintDraft(undefined)
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
  const openSshChat = useCallback((id: string) => {
    setSsh(false); setSshDraft(undefined); setGit(false); setWorkbench(false); setTab('chat'); workspace.openTask(id)
  }, [workspace.openTask])
  const toggleFollowAi = useCallback((enabled: boolean) => {
    setFollowAi(enabled)
    if (!enabled) setAiFocus(undefined)
    try { localStorage.setItem('ssh_follow_ai', String(enabled)) } catch {}
  }, [])
  const tasksLookupRef = useRef({ task: workspace.task, allTasks: workspace.allTasks })
  tasksLookupRef.current = { task: workspace.task, allTasks: workspace.allTasks }
  const titleForTask = useCallback((id: string) => {
    const { task, allTasks } = tasksLookupRef.current
    return (task?.task_id === id ? task : allTasks.find(item => item.task_id === id))?.title ?? undefined
  }, [])
  const diagnoseSsh = useCallback((text: string) => setSshDraft({ text }), [])
  const sshContextValue = useMemo(() => ({
    access: { ...(taskId ? { task_id: taskId } : {}) },
    openTask: openSshChat,
    aiFocus: aiFocus?.taskId === taskId ? aiFocus : undefined,
    followAi,
    onToggleFollowAi: toggleFollowAi,
    titleFor: titleForTask,
    ...(gitWorkspace ? { diagnose: diagnoseSsh } : {}),
  }), [taskId, openSshChat, aiFocus, followAi, toggleFollowAi, titleForTask, gitWorkspace, diagnoseSsh])
  const showHomeSidebar = !sidebarCollapsed && !workbench && !git && !ssh && !office
  const isHome = !office && !ssh && !git && !workbench && tab !== 'skill-pages'

  return (
    <div className={css.appFrame} data-ds-dark-theme={dark ? '' : undefined}>
      {/* 1. Activity Dock (48px fixed on left) */}
      <CodexDock
        activeTab={office ? 'office' : ssh ? 'ssh' : git ? 'git' : workbench || tab === 'skill-pages' ? 'skills' : 'home'}
        onSelectTab={selected => {
          setSsh(selected === 'ssh')
          if (selected !== 'skills') setBlueprintDraft(undefined)
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

      {/* 2. Codex Sidebar (collapsible & resizable; kept mounted for instant Home <-> SSH switching) */}
      {!sidebarCollapsed && (
        <div className={css.sidebarCol} style={{ width: actualSidebarWidth, display: showHomeSidebar ? undefined : 'none' }}>
          <HiddenFreeze hidden={!showHomeSidebar}>
            {sidebar}
          </HiddenFreeze>
        </div>
      )}

      {/* Drag handle for Sidebar */}
      {!sidebarCollapsed && (
        <div
          className={css.dragHandle}
          style={{ display: showHomeSidebar ? undefined : 'none' }}
          onPointerDown={(e) => {
            startHorizontalResize({
              event: e,
              startWidth: sidebarWidth,
              direction: 1,
              min: 200,
              max: 420,
              onWidth: setSidebarWidth,
              onCommit: updateSidebarWidth,
            })
          }}
        />
      )}

      {/* 3. Center Main Column (Header + Conversation + Floating Composer) */}
      <div className={css.centerCol}>
        {officeMounted && (
          <div className={css.dockedView} style={{ display: office ? 'flex' : 'none' }}>
            <div className={css.dockedPage}>
              <OfficeStudioView
                workspaceRoot={gitWorkspace}
                resetSignal={officeResetSignal}
                pendingOpen={pendingOpen}
                theme={dark ? 'dark' : 'light'}
                onStudioContext={setOfficeViewContext}
                onAskAi={handleOfficeAskAi}
                onManagePlugins={() => {
                  setOffice(false)
                  setWorkbench(false)
                  setGit(false)
                  setTab('skill-pages')
                }}
              />
            </div>
            <HiddenFreeze hidden={!office}>
              <CodeChatDock
                key="office"
                workspace={gitWorkspace}
                renderSlot={renderSlot}
                onOpenInChat={id => { setOffice(false); setOfficeDraft(undefined); setWorkbench(false); setTab('chat'); workspace.openTask(id) }}
                onFilePreview={openDockFile}
                diagnostic={officeDraft}
                viewContext={officeViewContext}
                sessions={workspace.allTasks}
                embeddedLauncher
                openSignal={officeChatOpenSignal}
              />
            </HiddenFreeze>
          </div>
        )}
        {sshMounted && <div className={css.dockedView} style={{ display: ssh ? 'flex' : 'none' }}>
          <SshContext.Provider value={sshContextValue}>
            <div className={css.dockedPage}><PluginPageHost owner="ssh" page="workbench" workspaceRoot={gitWorkspace} theme={dark ? 'dark' : 'light'} onStudioContext={setSshViewContext} /></div>
          </SshContext.Provider>
          <HiddenFreeze hidden={!ssh}>
            <CodeChatDock key="ssh" workspace={gitWorkspace} renderSlot={renderSlot} onOpenInChat={openSshChat} onFilePreview={openDockFile} diagnostic={sshDraft} viewContext={sshViewContext} sessions={workspace.allTasks} embeddedLauncher linkedTaskId={taskId} linkedSession={workspace.session} openSignal={sshChatOpenSignal} />
          </HiddenFreeze>
        </div>}
        {!office && !ssh && (git ? <div className={css.dockedView}>
          <div className={css.dockedPage}><PluginPageHost owner="git" page="history" workspaceRoot={gitWorkspace} /></div>
          {gitWorkspace && <CodeChatDock key="git" workspace={gitWorkspace} renderSlot={renderSlot} sessions={workspace.allTasks} onOpenInChat={id => { setGit(false); setWorkbench(false); setTab('chat'); workspace.openTask(id) }} onFilePreview={openDockFile} />}
        </div>
          : workbench ? (() => {
            const activeBlueprintWorkspace = blueprintWorkspace || gitWorkspace
            const knownProjects = workspace.projects.some(p => p.path === gitWorkspace) || !gitWorkspace
              ? workspace.projects
              : [{ path: gitWorkspace, title: gitWorkspace.split(/[\\/]/).filter(Boolean).pop() ?? gitWorkspace }, ...workspace.projects]
            return <div className={css.workbenchShell}>
              <div className={css.workbenchTopBar}>
                <div className={css.workbenchTabs} role="tablist" aria-label="流程与蓝图模式">
                  <button
                    type="button"
                    role="tab"
                    aria-selected={workbenchMode === 'flows'}
                    className={css.workbenchTab}
                    onClick={() => setWorkbenchMode('flows')}
                  >
                    历史编排与技能
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={workbenchMode === 'blueprint'}
                    className={css.workbenchTab}
                    onClick={() => setWorkbenchMode('blueprint')}
                  >
                    架构蓝图与最短路径
                  </button>
                </div>
                <span className={css.workbenchHint}>
                  {workbenchMode === 'flows'
                    ? '查看历史会话编排代码，或一键提炼为架构蓝图最短路径'
                    : '代码表示蓝图 · 架构画布 + 从历史会话提炼的可复用最短路径'}
                </span>
              </div>
              <div className={css.workbenchBody}>
                {workbenchMode === 'blueprint' ? (
                  <div className={css.dockedView}>
                    <BlueprintContext.Provider value={{
                      sendToAi: sendBlueprintDraft,
                      refreshSignal: workspace.session.task?.status,
                      projects: knownProjects,
                      selectWorkspace: selectBlueprintWorkspace,
                      sessions: workspace.allTasks,
                      openSession: id => { setBlueprintDraft(undefined); setWorkbench(false); setTab('chat'); workspace.openTask(id) },
                    }}>
                      <div className={css.dockedPage}><PluginPageHost owner="blueprint" page="workbench" workspaceRoot={activeBlueprintWorkspace} theme={dark ? 'dark' : 'light'} /></div>
                    </BlueprintContext.Provider>
                    {activeBlueprintWorkspace && <CodeChatDock key={`blueprint:${activeBlueprintWorkspace}`} workspace={activeBlueprintWorkspace} renderSlot={renderSlot}
                      onOpenInChat={id => { setBlueprintDraft(undefined); setWorkbench(false); setTab('chat'); workspace.openTask(id) }}
                      onFilePreview={openDockFile}
                      diagnostic={blueprintDraft} sessions={workspace.allTasks} embeddedLauncher linkedTaskId={activeBlueprintWorkspace === gitWorkspace ? taskId : undefined} linkedSession={activeBlueprintWorkspace === gitWorkspace ? workspace.session : undefined} openSignal={blueprintChatOpenSignal} />}
                  </div>
                ) : (
                  <SkillWorkbench tasks={workspace.allTasks} projects={workspace.projects} defaultWorkspace={workspace.info?.workspace ?? ''} onOpenChat={id => { setWorkbench(false); setTab('chat'); workspace.openTask(id) }} onDistillToBlueprint={distillTaskToBlueprint} onFilePreview={openDockFile} onPackages={() => { setWorkbench(false); setTab('skill-pages') }} pageRequest={dailyReportPage} renderSlot={renderSlot} theme={dark ? 'dark' : 'light'} />
                )}
              </div>
            </div>
          })()
            : tab === 'skill-pages' ? <SkillPackagesPage workspace={workspace} renderSlot={renderSlot} theme={dark ? 'dark' : 'light'} onBack={() => setTab('chat')} onOpenInChat={id => { setWorkbench(false); setTab('chat'); workspace.openTask(id) }} onFilePreview={openDockFile} /> : null)}
        <div style={{ display: isHome ? 'flex' : 'none', flex: 1, flexDirection: 'row', minHeight: 0, minWidth: 0, overflow: 'hidden', position: 'relative' }}>
          <div style={{ display: 'flex', flex: 1, flexDirection: 'column', minHeight: 0, minWidth: 0, overflow: 'hidden' }}>
            <HiddenFreeze hidden={!isHome}>
              {workspace.heroPhase ? (
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
              )}
            </HiddenFreeze>
          </div>
          {BLUEPRINT_OBSERVER_ENABLED && !workspace.heroPhase && workspace.task && (
            <BlueprintObserverHost
              watchedTask={workspace.task}
              watchedEvents={workspace.session.events}
              watchedRunning={workspace.running}
              workspaceRoot={workspace.task.workspace ?? workspace.activeWorkspace ?? workspace.info?.workspace ?? ''}
              renderSlot={renderSlot}
              onMainSessionUpdated={workspace.session.reload}
              onOpenTask={workspace.openTask}
              onFilePreview={openDockFile}
            />
          )}
        </div>
      </div>

      {/* Drag handle for Right Panel */}
      {rightPanelOpen && !workspace.heroPhase && tab !== 'skill-pages' && !workbench && !git && !ssh && !office && (
        <div
          className={css.dragHandle}
          role="separator" aria-orientation="vertical" aria-label="调整预览抽屉宽度"
          title="拖动调整面板宽度"
          onPointerDown={(e) => {
            startHorizontalResize({
              event: e,
              startWidth: rightPanelWidth,
              direction: -1,
              min: 300,
              max: () => limitPanelWidth(1100),
              onWidth: setRightPanelWidth,
              onCommit: updateRightPanelWidth,
            })
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
      <AppDialogHost />
    </div>
  )
}
