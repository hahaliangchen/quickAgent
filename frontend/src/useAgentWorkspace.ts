import { randomId } from './randomId.ts'
import { resolveEffort } from './reasoningEffort.ts'
import type {AttachmentSubmission} from './components/Attachments'
import { requestNotificationPermission, useTaskNotifications } from './useTaskNotifications.ts'
import type { PermissionMode } from './api'
import { useCallback, useEffect, useRef, useState } from 'react'
import { acceptsPrompt, type AgentInfo, type SettingsData, type Task, type Project } from './api.ts'
import { defaultsPayload, generationLocked, type GenerationDefaults } from './generationDefaults.ts'
import { useChatPermissionMode } from './permissionMode.ts'
import { useAgentApi } from './cordis/react.tsx'
import { t } from './i18n.ts'
import { useSession } from './useSession.ts'

export type SettingsTab = 'general' | 'models' | 'tools' | 'plugins'

type ModelPick = Readonly<{ model: string; provider: string }>

const LAST_MODEL_STORAGE_KEY = 'codex_last_model_pick'

function readSavedModelPick(): ModelPick | undefined {
  try {
    const raw = localStorage.getItem(LAST_MODEL_STORAGE_KEY)
    if (!raw) return undefined
    const parsed = JSON.parse(raw) as { model?: unknown; provider?: unknown }
    if (typeof parsed.model === 'string' && parsed.model && typeof parsed.provider === 'string' && parsed.provider) {
      return { model: parsed.model, provider: parsed.provider }
    }
  } catch {}
  return undefined
}

function writeSavedModelPick(pick: ModelPick): void {
  try {
    localStorage.setItem(LAST_MODEL_STORAGE_KEY, JSON.stringify(pick))
  } catch {}
}

function routeTaskId(): string | undefined {
  const match = /^#\/tasks\/(.+)$/.exec(location.hash)
  return match?.[1] === undefined ? undefined : decodeURIComponent(match[1])
}

function isSettingsRoute(): boolean {
  return location.pathname.endsWith('/settings') || location.hash.startsWith('#/settings')
}

function openTask(taskId: string | undefined): void {
  if (location.pathname.endsWith('/settings')) {
    history.replaceState(null, '', '/agent' + (taskId === undefined ? '#/' : `#/tasks/${encodeURIComponent(taskId)}`))
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    return
  }
  location.hash = taskId === undefined ? '#/' : `#/tasks/${encodeURIComponent(taskId)}`
}

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}

/** Task selection, history, settings state, and commands shared by the hero and conversation views. */
export function useAgentWorkspace() {
  const api = useAgentApi()
  const [info, setInfo] = useState<AgentInfo>()
  const [settings, setSettings] = useState<SettingsData>()
  const [allTasks, setAllTasks] = useState<readonly Task[]>([])
  const [taskId, setTaskId] = useState(routeTaskId)
  const [notice, setNotice] = useState<string>()
  const [parent, setParent] = useState<Task>()
  const [settingsOpen, setSettingsOpen] = useState(isSettingsRoute)
  const [settingsTab, setSettingsTab] = useState<SettingsTab>('general')
  const [selectedModel, setSelectedModel] = useState<ModelPick & { taskId: string | undefined }>()
  const [newSessionModel, setNewSessionModel] = useState<ModelPick | undefined>(readSavedModelPick)
  const [selectedEffortOverride, setSelectedEffortOverride] = useState<string | null>()
  const [selectedFastModeOverride, setSelectedFastModeOverride] = useState<boolean>()

  const [projects, setProjects] = useState<readonly Project[]>([])
  const workspaces = projects.map(project => project.path)
  const [activeWorkspace, setActiveWorkspace] = useState<string>(() => {
    try { return localStorage.getItem('codex_active_workspace') ?? '' } catch { return '' }
  })
  const [sidebarMode, setSidebarMode] = useState<'chat' | 'code'>(() => {
    try { return localStorage.getItem('codex_sidebar_mode') === 'chat' ? 'chat' : 'code' } catch { return 'code' }
  })
  const changeSidebarMode = useCallback((mode: 'chat' | 'code') => {
    setSidebarMode(mode)
    try { localStorage.setItem('codex_sidebar_mode', mode) } catch {}
  }, [])
  const [folderDialogOpen, setFolderDialogOpen] = useState(false)
  const refreshProjects = useCallback(async () => {
    const list = await api.projects()
    setProjects(list)
    setActiveWorkspace(current => list.some(project => project.path === current) ? current : list[0]?.path ?? '')
  }, [api])
  const selectWorkspace = useCallback((ws: string) => {
    setActiveWorkspace(ws)
    changeSidebarMode('code')
    try { localStorage.setItem('codex_active_workspace', ws) } catch {}
    openTask(undefined)
  }, [changeSidebarMode])
  const addWorkspace = useCallback(async (path: string, title?: string) => {
    const project = await api.addProject(path.trim(), title?.trim())
    await refreshProjects()
    selectWorkspace(project.path)
    setFolderDialogOpen(false)
  }, [api, refreshProjects, selectWorkspace])
  const removeWorkspace = useCallback(async (path: string) => {
    await api.removeProject(path)
    await refreshProjects()
    if (path === activeWorkspace) openTask(undefined)
  }, [api, activeWorkspace, refreshProjects])
  const renameWorkspace = useCallback(async (path: string, title: string) => {
    await api.renameProject(path, title)
    await refreshProjects()
  }, [api, refreshProjects])

  const openFolderDialog = useCallback(() => setFolderDialogOpen(true), [])
  const closeFolderDialog = useCallback(() => setFolderDialogOpen(false), [])

  useEffect(() => {
    setSelectedModel(undefined)
    if (taskId !== undefined) setNewSessionModel(undefined)
    setSelectedEffortOverride(undefined)
    setSelectedFastModeOverride(undefined)
  }, [taskId])

  const refreshTasks = useCallback(() => {
    api.listTasks(10000).then(
      list => { setAllTasks(list) },
      () => {},
    )
  }, [api])

  const refreshInfoAndSettings = useCallback(() => {
    api.info().then(
      value => {
        setInfo(value)
      },
      (cause: unknown) => { setNotice(messageOf(cause)) },
    )
    api.getSettings().then(
      value => { setSettings(value) },
      () => {},
    )
  }, [api])

  useEffect(() => {
    let disposed = false
    void (async () => {
      try {
        const saved = localStorage.getItem('codex_workspaces')
        if (saved) {
          const paths: unknown = JSON.parse(saved)
          if (Array.isArray(paths)) {
            for (const path of paths) if (typeof path === 'string') {
              try { await api.addProject(path) } catch { /* Invalid legacy paths were demo entries. */ }
            }
          }
          localStorage.removeItem('codex_workspaces')
        }
        if (!disposed) await refreshProjects()
      } catch (cause) { if (!disposed) setNotice(messageOf(cause)) }
    })()
    return () => { disposed = true }
  }, [api, refreshProjects])

  const deleteSession = useCallback(async (id: string) => {
    const result = await api.deleteSession(id)
    setAllTasks(current => current.filter(task => !result.deleted.includes(task.task_id)))
    if (taskId && result.deleted.includes(taskId)) openTask(undefined)
    refreshTasks()
  }, [api, taskId, refreshTasks])

  const session = useSession(taskId, refreshTasks)
  const task = session.task?.task_id === taskId ? session.task : undefined
  // The open chat's own permission level, saved as soon as it is chosen.
  const savePermissionMode = useCallback(async (id: string, mode: PermissionMode) => {
    const updated = await api.updateTask(id, { permission_mode: mode })
    session.reload()
    return updated
  }, [api, session.reload])
  const { mode: permissionMode, select: selectPermissionMode } = useChatPermissionMode(task, savePermissionMode)
  const updateSession = useCallback(async (id: string, input: {title?: string; pinned?: boolean; archived?: boolean}) => {
    await api.updateSession(id, input)
    refreshTasks()
    if (id === taskId) session.reload()
  }, [api, refreshTasks, taskId, session.reload])
  useEffect(() => {
    if (!task) return
    changeSidebarMode(task.plugin_id || task.kind === 'chat' ? 'chat' : 'code')
    if (task.workspace) setActiveWorkspace(task.workspace)
  }, [task?.task_id, changeSidebarMode])

  useEffect(() => {
    const onHash = () => {
      if (isSettingsRoute()) {
        setSettingsOpen(true)
      } else {
        setTaskId(routeTaskId())
        setNotice(undefined)
      }
    }
    window.addEventListener('hashchange', onHash)
    return () => { window.removeEventListener('hashchange', onHash) }
  }, [])

  useEffect(() => {
    refreshInfoAndSettings()
    refreshTasks()
    const timer = window.setInterval(refreshTasks, 5000)
    return () => { window.clearInterval(timer) }
  }, [refreshInfoAndSettings, refreshTasks])

  const parentId = task?.parent_task_id ?? undefined
  useEffect(() => {
    let active = true
    setParent(undefined)
    if (parentId !== undefined) {
      api.getTask(parentId).then(value => { if (active) setParent(value) }, () => {})
    }
    return () => { active = false }
  }, [api, parentId])

  // Used directly as an onClick handler by some buttons, so `tab` may be a
  // click event rather than a tab id.
  const openSettings = useCallback((tab?: SettingsTab) => {
    setSettingsTab(typeof tab === 'string' ? tab : 'general')
    setSettingsOpen(true)
  }, [])

  const closeSettings = useCallback(() => {
    setSettingsOpen(false)
    if (location.pathname.endsWith('/settings')) {
      history.replaceState(null, '', '/agent' + (taskId === undefined ? '#/' : `#/tasks/${encodeURIComponent(taskId)}`))
    } else if (location.hash.startsWith('#/settings')) {
      location.hash = taskId === undefined ? '#/' : `#/tasks/${encodeURIComponent(taskId)}`
    }
    refreshInfoAndSettings()
  }, [refreshInfoAndSettings, taskId])

  // A pick has to hold for the next chat as well, so every pick rewrites the saved
  // defaults. Two writers can interleave here (the settings form, the shell path,
  // another tab) and the server refuses a save whose revision has moved on, so a
  // refused save rebases on the document it found and retries once instead of
  // leaving the pick half applied.
  const persistDefaults = useCallback(async (defaults: GenerationDefaults): Promise<void> => {
    const write = async (base: SettingsData): Promise<SettingsData> =>
      api.saveSettings(defaultsPayload(base, defaults))
    let updated: SettingsData
    try {
      updated = await write(settings ?? await api.getSettings())
    } catch (refused) {
      const fresh = await api.getSettings()
      // Only a document that actually moved on is worth a second attempt.
      if (fresh.revision === settings?.revision) throw refused
      updated = await write(fresh)
    }
    setSettings(updated)
    setInfo(await api.info())
  }, [api, settings])

  const selectModel = useCallback(async (providerId: string, modelId: string): Promise<void> => {
    setSelectedModel({ taskId, model: modelId, provider: providerId })
    const pick = { model: modelId, provider: providerId }
    setNewSessionModel(pick)
    writeSavedModelPick(pick)
    let rowError: string | undefined
    try {
      const latest = settings ?? await api.getSettings()
      const capability = latest.providers.find(p => p.id === providerId)?.model_capabilities[modelId]
      const defaultEffort = resolveEffort(undefined, capability?.reasoning_efforts ?? [], capability?.default_effort)
      setSelectedEffortOverride(defaultEffort || null)
      setSelectedFastModeOverride(false)

      // The open chat keeps the pick for its own next message, and that message
      // carries the model anyway, so a chat that is still turning is left alone:
      // the server rejects a mid-turn switch. A refusal is reported, never fatal:
      // the row of one chat must not cost the default that the next chat opens on.
      if (taskId !== undefined && !generationLocked(task?.status)) {
        try {
          await api.updateTask(taskId, {
            model: modelId,
            provider: providerId,
            reasoning_effort: defaultEffort || null,
            fast_mode: false,
          })
          session.reload()
          refreshTasks()
        } catch (cause) {
          rowError = messageOf(cause)
        }
      }

      await persistDefaults({ provider: providerId, model: modelId, reasoning_effort: defaultEffort, fast_mode: false })
      setNotice(rowError)
    } catch (cause) {
      setNotice(`已用于当前对话，但未能更新全局默认模型：${messageOf(cause)}`)
    }
  }, [api, persistDefaults, refreshTasks, session.reload, settings, task?.status, taskId])

  const setGenerationOptions = useCallback(async (reasoningEffort: string, fastMode: boolean): Promise<void> => {
    setSelectedEffortOverride(reasoningEffort || null)
    setSelectedFastModeOverride(fastMode)
    let rowError: string | undefined
    try {
      // Same reason one level down: a running chat refuses a level change, and
      // that refusal must not cost the levels their saved default either.
      if (taskId !== undefined && !generationLocked(task?.status)) {
        try {
          await api.updateTask(taskId, {
            reasoning_effort: reasoningEffort || null,
            fast_mode: fastMode,
          })
          session.reload()
        } catch (cause) {
          rowError = messageOf(cause)
        }
      }
      await persistDefaults({ reasoning_effort: reasoningEffort, fast_mode: fastMode })
      setNotice(rowError)
    } catch (cause) {
      setNotice(`已用于当前对话，但没能存成新对话的默认设置：${messageOf(cause)}`)
    }
  }, [api, persistDefaults, session.reload, task?.status, taskId])

  const tasks = allTasks.filter(item => item.parent_task_id === null)
  const ready = info?.ready === true
  const isChild = task?.parent_task_id != null
  const running = task?.status === 'running' || task?.status === 'cancelling'
  const heroPhase = taskId === undefined || (task?.status === 'draft' && session.events.length === 0)
  const composerNote = !ready && info !== undefined ? t('app.notReady') : notice ?? session.error

  const activePick = selectedModel?.taskId === taskId ? selectedModel : undefined
  const validDraftPick = newSessionModel && (
    settings === undefined
    || settings.providers.some(p => p.id === newSessionModel.provider && p.models.includes(newSessionModel.model))
  ) ? newSessionModel : undefined
  const draftPick = taskId === undefined ? validDraftPick : undefined
  const sessionModel = activePick?.model ?? task?.model ?? draftPick?.model
  const modelName = sessionModel ?? info?.model ?? settings?.orchestrator_model
  const requestedEffort = selectedEffortOverride !== undefined
    ? selectedEffortOverride
    : (task?.reasoning_effort ?? settings?.reasoning_effort ?? '')
  // The same model can be offered by several providers, e.g. a personal and a
  // company key: a model is identified by its provider and its id together.
  const requestedProvider = activePick?.provider ?? task?.provider_name ?? draftPick?.provider ?? settings?.orchestrator_provider ?? undefined
  const modelProvider = settings?.providers.find(p => p.id === requestedProvider && p.models.includes(modelName ?? ''))
    ?? settings?.providers.find(p => p.models.includes(modelName ?? ''))
  const modelProviderId = modelProvider?.id ?? requestedProvider
  const activeCapability = modelProvider?.model_capabilities[modelName ?? '']
  const reasoningEffort = resolveEffort(requestedEffort, activeCapability?.reasoning_efforts ?? [], activeCapability?.default_effort)
  const recordedFastMode = selectedFastModeOverride !== undefined
    ? selectedFastModeOverride
    : (task?.fast_mode ?? settings?.fast_mode ?? false)
  // A model that declares no fast mode must not be sent the flag, or the whole
  // settings save is rejected; an unknown capability is left alone.
  const fastMode = recordedFastMode && (activeCapability === undefined || activeCapability.fast_mode)

  const startNewSession = useCallback(() => {
    // Carry the displayed model into the next draft without waiting for the settings write.
    if (sessionModel && modelProviderId) {
      const pick = { model: sessionModel, provider: modelProviderId }
      setNewSessionModel(pick)
      writeSavedModelPick(pick)
    }
    openTask(undefined)
  }, [sessionModel, modelProviderId])

  const newTaskLaunch = useRef<{ taskId: string; at: number }>()
  const pendingSession=useRef<Promise<string>|null>(null)
  const uploadedAttachments=useRef(new Map<File,{task:string;receiptId:string}>())
  useEffect(()=>{pendingSession.current=null},[taskId,activeWorkspace,sidebarMode])
  useTaskNotifications(allTasks, taskId, openTask)
  const submit = useCallback(async (text: string, permissionMode: PermissionMode, attachment?:AttachmentSubmission): Promise<boolean> => {
    setNotice(undefined)
    if (!taskId && sidebarMode === 'code' && !projects.some(project => project.path === activeWorkspace)) {
      setNotice('请先添加或选择项目')
      setFolderDialogOpen(true)
      return false
    }
    requestNotificationPermission()
    try {
      const id = taskId ?? await (pendingSession.current??=api.createSession(sidebarMode === 'code' ? activeWorkspace : undefined, sidebarMode).catch(error=>{pendingSession.current=null;throw error}))
      const content=[]
      for(const file of attachment?.files??[]){
        let uploaded=uploadedAttachments.current.get(file)
        if(!uploaded||uploaded.task!==id){const value=await api.upload(id,file,attachment!.signal,p=>attachment!.onProgress(`正在上传 ${file.name} · ${p}%`));uploaded={task:id,receiptId:value.receiptId};uploadedAttachments.current.set(file,uploaded)}
        content.push({type:'file',receiptId:uploaded.receiptId})
      }
      if(attachment?.signal.aborted)throw Error('上传已取消')
      if (!taskId) newTaskLaunch.current = { taskId: id, at: Date.now() }
      await api.prompt(id, text, {
        content,requestId:attachment?.requestId??randomId(),
        permission_mode: permissionMode,
        model: modelName ?? undefined,
        provider: modelProviderId,
        reasoning_effort: reasoningEffort ? reasoningEffort : undefined,
        fast_mode: fastMode,
      })
      if (modelName && modelProviderId) {
        const pick = { model: modelName, provider: modelProviderId }
        setNewSessionModel(pick)
        writeSavedModelPick(pick)
        if (settings && (settings.orchestrator_provider !== modelProviderId || settings.orchestrator_model !== modelName)) {
          void persistDefaults({
            provider: modelProviderId,
            model: modelName,
            reasoning_effort: reasoningEffort,
            fast_mode: fastMode,
          }).catch(() => {})
        }
      }
      uploadedAttachments.current.clear()
      refreshTasks()
      if (id === taskId) session.reload()
      else openTask(id)
      return true
    } catch (cause) {
      setNotice(messageOf(cause))
      return false
    }
  }, [api, activeWorkspace, projects, sidebarMode, fastMode, modelName, modelProviderId, persistDefaults, reasoningEffort, refreshTasks, session.reload, settings, taskId])

  const stop = useCallback(() => {
    if (taskId === undefined) return
    api.cancel(taskId).then(
      () => { session.reload() },
      (cause: unknown) => { setNotice(messageOf(cause)) },
    )
  }, [api, session.reload, taskId])

  const interject = useCallback(async (text: string) => {
    if (taskId === undefined) throw new Error('NOT_RUNNING')
    await api.interject(taskId, text)
    session.reload()
  }, [api, session.reload, taskId])

  const saveSettings = useCallback((updated: SettingsData) => {
    setSettings(updated)
    refreshInfoAndSettings()
  }, [refreshInfoAndSettings])

  return {
    info, settings, tasks, allTasks, taskId, task, parent, parentId, session,
    ready, isChild, running, heroPhase, composerNote,
    modelName, modelProviderId, reasoningEffort, fastMode,
    permissionMode, selectPermissionMode,
    projects, workspaces, activeWorkspace, selectWorkspace, addWorkspace, removeWorkspace, renameWorkspace,
    sidebarMode, changeSidebarMode, updateSession, deleteSession,
    folderDialogOpen, openFolderDialog, closeFolderDialog,
    canContinue: ready && task !== undefined && acceptsPrompt(task),
    settingsOpen, settingsTab, setSettingsTab,
    openSettings, closeSettings, saveSettings, refreshInfoAndSettings, selectModel, setGenerationOptions,
    openTask, startNewSession, submit, stop, interject, newTaskLaunch,
  }
}

export type AgentWorkspace = ReturnType<typeof useAgentWorkspace>
