import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, type Dispatch, type SetStateAction, type ReactNode } from 'react'
import clsx from 'clsx'
import { BrandMark } from './BrandMark.tsx'
import { prefillComposer } from './RewindControl.tsx'
import { PlanProgress } from './PlanProgress.tsx'
import { taskTitle, workspaceName } from './Sidebar.tsx'
import { latestContextUsage } from './contextUsage.ts'
import { SkillPagesView } from './SkillPagesView.tsx'
import type { OpenFilePreview } from '../filePreview.ts'
import { NEW_CHAT_DRAFT } from '../composerDraft.ts'
import type { AgentSlotProps } from '../cordis/slot-contract.ts'
import root from '../dsh/conversation/ConversationRoot.module.css'
import welcomeCss from './Welcome.module.css'
import readonlyCss from '../dsh/subagent/SubagentReadOnlyComposer.module.css'
import { t } from '../i18n.ts'
import { chatItems, formatDuration } from '../model.ts'
import { useTaskDuration } from '../useTaskDuration.ts'
import type { AgentWorkspace } from '../useAgentWorkspace.ts'
import css from '../App.module.css'
import headerCss from './CodexHeader.module.css'

interface PageProps {
  readonly workspace: AgentWorkspace
  readonly renderSlot: AgentSlotProps['renderSlot']
}

interface StarterAction {
  readonly id: string
  readonly label: string
  readonly prompt: string
  readonly icon: ReactNode
}

const CODE_STARTERS: readonly StarterAction[] = [
  {
    id: 'review-diff',
    label: '审查最近改动',
    prompt: '请审查当前仓库最近的改动，指出潜在风险、边界问题或可改进之处。',
    icon: (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <line x1="8" x2="21" y1="6" y2="6" /><line x1="8" x2="21" y1="12" y2="12" /><line x1="8" x2="21" y1="18" y2="18" />
        <line x1="3" x2="3.01" y1="6" y2="6" /><line x1="3" x2="3.01" y1="12" y2="12" /><line x1="3" x2="3.01" y1="18" y2="18" />
      </svg>
    ),
  },
  {
    id: 'explain-arch',
    label: '梳理项目架构',
    prompt: '请梳理当前项目的核心架构、关键目录职责与主要执行链路。',
    icon: (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
        <line x1="8" x2="16" y1="9" y2="9" /><line x1="8" x2="13" y1="13" y2="13" />
      </svg>
    ),
  },
  {
    id: 'run-tests',
    label: '运行并修复测试',
    prompt: '请运行项目测试，定位所有失败项并修复后重新验证。',
    icon: (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <polyline points="4 17 10 11 4 5" /><line x1="12" x2="20" y1="19" y2="19" />
      </svg>
    ),
  },
]

const CHAT_STARTERS: readonly StarterAction[] = [
  {
    id: 'break-down',
    label: '拆解复杂问题',
    prompt: '请帮我把下面这个问题拆解成清晰的可执行步骤与关键风险：',
    icon: (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <line x1="8" x2="21" y1="6" y2="6" /><line x1="8" x2="21" y1="12" y2="12" /><line x1="8" x2="21" y1="18" y2="18" />
        <line x1="3" x2="3.01" y1="6" y2="6" /><line x1="3" x2="3.01" y1="12" y2="12" /><line x1="3" x2="3.01" y1="18" y2="18" />
      </svg>
    ),
  },
  {
    id: 'compare-options',
    label: '对比技术方案',
    prompt: '请从架构复杂度、性能与维护成本几个维度，帮我对比以下技术方案：',
    icon: (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
        <line x1="8" x2="16" y1="9" y2="9" /><line x1="8" x2="13" y1="13" y2="13" />
      </svg>
    ),
  },
  {
    id: 'polish-doc',
    label: '润色与总结文档',
    prompt: '请帮我提炼以下内容的核心要点，并改写得更精炼易读：',
    icon: (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M12 20h9" /><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" />
      </svg>
    ),
  },
]

export function HeroPage({ workspace, renderSlot }: PageProps) {
  const currentWs = workspace.projects.find(project => project.path === workspace.activeWorkspace)?.path ?? ''
  const wsName = workspaceName(currentWs)
  const starters = workspace.sidebarMode === 'chat' ? CHAT_STARTERS : CODE_STARTERS

  return (
    <main className={welcomeCss.page} data-phase="hero">
      <div className={welcomeCss.welcome}>
        <div className={welcomeCss.heading}>
          <div className={welcomeCss.emblem}>
            <BrandMark size={44} />
          </div>
          <h1 className={welcomeCss.title}>
            {workspace.sidebarMode === 'chat'
              ? '今天想聊点什么'
              : !currentWs
                ? '准备好开始新的代码任务了吗'
                : <>在 <span className={welcomeCss.workspace} title={currentWs}>{wsName}</span> 开始</>}
          </h1>
        </div>
        <div className={welcomeCss.composerWrap}>
          {renderSlot('agent.composer', {
            variant: 'hero',
            draftKey: workspace.taskId ?? NEW_CHAT_DRAFT,
            disabled: !workspace.ready,
            note: workspace.composerNote,
            running: false,
            workspacePath: currentWs,
            workspaces: workspace.workspaces,
            activeWorkspace: currentWs,
            onSelectWorkspace: workspace.selectWorkspace,
            onOpenAddWorkspace: workspace.openFolderDialog,
            modelName: workspace.modelName ?? undefined,
            modelProviderId: workspace.modelProviderId,
            reasoningEffort: workspace.reasoningEffort ?? undefined,
            fastMode: workspace.fastMode,

            settings: workspace.settings,
            onSelectModel: workspace.selectModel,
            onSetGenerationOptions: workspace.setGenerationOptions,
            onOpenSettings: workspace.openSettings,
            permissionMode: workspace.permissionMode,
            onSelectPermissionMode: workspace.selectPermissionMode,
            onSubmit: workspace.submit,
            onStop: undefined,
          })}
        </div>
        <div className={welcomeCss.starters} role="group" aria-label="快捷开始">
          {starters.map(item => (
            <button
              key={item.id}
              type="button"
              className={welcomeCss.starterChip}
              disabled={!workspace.ready}
              onClick={() => prefillComposer(item.prompt)}
            >
              <span className={welcomeCss.starterIcon}>{item.icon}</span>
              <span>{item.label}</span>
            </button>
          ))}
        </div>
      </div>
    </main>
  )
}

export function SkillPackagesPage({ workspace, onBack }: { workspace: AgentWorkspace; onBack: () => void }) {
  return <main className={root.root} style={{ minHeight: 0 }}>
    <SkillPagesView workspaceRoot={workspace.activeWorkspace ?? workspace.info?.workspace ?? ''} onBack={onBack} />
  </main>
}

export type Tab = 'chat' | 'flow' | 'skill-pages'

export function ConversationPage({
  workspace,
  renderSlot,
  tab,
  setTab,
  rightPanelOpen = true,
  onToggleRightPanel,
  onFilePreview,
}: PageProps & {
  readonly tab: Tab
  readonly setTab: Dispatch<SetStateAction<Tab>>
  readonly rightPanelOpen?: boolean | undefined
  readonly onToggleRightPanel?: (() => void) | undefined
  readonly onFilePreview?: OpenFilePreview | undefined
}) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const bodyRef = useRef<HTMLDivElement>(null)
  const followTail = useRef(true)
  const pendingJump = useRef<{ taskId: string | undefined; key: string }>()
  const scrollToMessage = useCallback((key: string) => {
    const scroll = scrollRef.current
    const message = scroll?.querySelector<HTMLElement>(`[data-chat-turn-message="${CSS.escape(key)}"]`)
    if (!scroll || !message) return
    followTail.current = false
    message.focus({ preventScroll: true })
    scroll.scrollTop += message.getBoundingClientRect().top - scroll.getBoundingClientRect().top - 12
  }, [])
  // The chat width scales with the space the conversation actually has (the
  // window minus the sidebar and any open side panel), which CSS reads from
  // --dsh-conversation-column-width.
  useLayoutEffect(() => {
    const body = bodyRef.current
    if (body === null) return
    const scroll = scrollRef.current
    if (scroll === null) return
    let lastWidth: number | undefined
    const observer = new ResizeObserver(([entry]) => {
      if (entry === undefined) return
      const width = entry.contentBoxSize?.[0]?.inlineSize ?? entry.contentRect.width
      const rounded = Math.round(width)
      if (rounded === lastWidth) return
      lastWidth = rounded
      scroll.style.setProperty('--dsh-conversation-column-width', `${rounded}px`)
    })
    observer.observe(body)
    return () => observer.disconnect()
  }, [])
  useLayoutEffect(() => { followTail.current = true }, [workspace.taskId, tab])
  useLayoutEffect(() => {
    const scroll = scrollRef.current
    if (scroll !== null && followTail.current && tab === 'chat') scroll.scrollTop = scroll.scrollHeight
  }, [workspace.session.events, workspace.taskId, tab])
  useLayoutEffect(() => {
    const jump = pendingJump.current
    if (tab !== 'chat' || jump === undefined) return
    pendingJump.current = undefined
    if (jump.taskId === workspace.taskId) scrollToMessage(jump.key)
  }, [tab, workspace.taskId, scrollToMessage])
  useEffect(() => {
    const scroll = scrollRef.current
    const transcript = scroll?.firstElementChild
    if (!scroll || !transcript || tab !== 'chat') return
    // Streaming text grows between event batches; follow the rendered size
    // rather than jumping only when a network event arrives.
    const observer = new ResizeObserver(() => {
      if (followTail.current) scroll.scrollTop = scroll.scrollHeight
    })
    observer.observe(transcript)
    return () => observer.disconnect()
  }, [workspace.taskId, tab])

  const {
    task, parent, parentId, isChild, running, ready, canContinue,
    composerNote, session, openTask, submit, stop,
  } = workspace
  const durationInfo = useTaskDuration(session.events, task, running)
  const contextUsage = useMemo(() => latestContextUsage(session.events), [session.events])
  const currentTitle = task === undefined ? t('app.untitled') : taskTitle(task)
  const items = useMemo(() => chatItems(session.events, task), [session.events, task])
  const recentTurns = useMemo(() => items.filter(item => item.kind === 'user').slice(-2).map(item => ({
    key: item.key,
    text: item.text.trim().replace(/\s+/g, ' ') || item.attachments?.map(file => file.name).join('、') || '附件消息',
  })), [items])

  const jumpToTurn = (key: string) => {
    if (tab === 'chat') {
      scrollToMessage(key)
    } else {
      pendingJump.current = { taskId: workspace.taskId, key }
      setTab('chat')
    }
  }
  // A summary file link reveals the inspector when it is closed; the toggle is
  // only asked to open, so a click never folds a panel the user is reading.
  const revealRightPanel = () => {
    if (!rightPanelOpen) onToggleRightPanel?.()
  }

  return (
    <main className={root.root} data-phase="active">
      {/* Codex-style Top Header: [ 📄 Clarify request ]  [ 对话 | 流程图 ]  [ 耗时 12s | ⧉ | ... ] */}
      <header className={headerCss.header}>
        {/* Left: Document icon + Session title */}
        <div className={headerCss.titleArea}>
          <span className={headerCss.titleIcon}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2">
              <path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z" />
              <polyline points="14 2 14 8 20 8" />
            </svg>
          </span>
          {recentTurns.length === 0 ? (
            <span className={headerCss.titleText} title={currentTitle}>
              {isChild && parent ? `${taskTitle(parent)} / ` : ''}{currentTitle}
            </span>
          ) : (
            <nav className={headerCss.turnList} aria-label="最近对话轮次">
              {recentTurns.map(({ key, text }, index) => {
                const label = index === recentTurns.length - 1 ? '当前轮' : '上一轮'
                return (
                  <button type="button" key={key} className={headerCss.turnButton} title={text} onClick={() => jumpToTurn(key)}>
                    <span className={headerCss.turnLabel}>{label}</span>
                    <span className={headerCss.titleText}>{text}</span>
                  </button>
                )
              })}
            </nav>
          )}
        </div>

        {/* Center: Clean Segmented Pill Tabs */}
        <div className={headerCss.tabPillGroup} role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'chat'}
            className={clsx(headerCss.tabPill, tab === 'chat' && headerCss.tabPillActive)}
            onClick={() => setTab('chat')}
          >
            对话
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'flow'}
            className={clsx(headerCss.tabPill, tab === 'flow' && headerCss.tabPillActive)}
            onClick={() => setTab('flow')}
          >
            流程图
          </button>
        </div>

        {/* Right: Duration badge, Right Panel Toggle */}
        <div className={headerCss.rightUtils}>
          {durationInfo.currentTurnDurationMs !== undefined && (
            <div className={headerCss.durationBadge}>
              {durationInfo.isRunning && <span className={headerCss.liveDot} />}
              <span>{durationInfo.isRunning ? '进行中' : '耗时'} {formatDuration(durationInfo.currentTurnDurationMs)}</span>
            </div>
          )}

          {/* Toggle Right Inspector Panel (Matches split icon in screenshot) */}
          {onToggleRightPanel && (
            <button
              type="button"
              className={clsx(headerCss.iconBtn, rightPanelOpen && headerCss.iconBtnActive)}
              title={rightPanelOpen ? '隐藏右侧检查器' : '显示右侧检查器'}
              onClick={onToggleRightPanel}
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2">
                <rect width="18" height="18" x="3" y="3" rx="2" ry="2" />
                <line x1="15" x2="15" y1="3" y2="21" />
              </svg>
            </button>
          )}

        </div>
      </header>

      <div ref={bodyRef} className={root.body}>
        <div
          ref={scrollRef}
          className={root.scrollBody}
          data-conversation-scroll
          onScroll={(event) => {
            const box = event.currentTarget
            followTail.current = box.scrollHeight - box.scrollTop - box.clientHeight < 80
          }}
        >
          <div className={css.session} data-slot="conversation.session" style={tab === 'flow' ? { height: '100%', flex: 1, display: 'flex', flexDirection: 'column' } : undefined}>
            <div className={root.viewArea} style={tab === 'flow' ? { height: '100%', flex: 1, minHeight: 520, display: 'flex' } : undefined}>
              {tab === 'chat'
                ? <>{renderSlot('agent.chat', { events: session.events, projectedItems: items, task, running, onOpenTask: openTask, onChangesUpdated: session.reload, onRevealRightPanel: revealRightPanel, onFilePreview })}</>
                : <>{renderSlot('agent.flow', { taskId: workspace.taskId, events: session.events, task, running })}</>}
            </div>
          </div>
          <div className={root.composerSeat}>
            {tab === 'chat' && <PlanProgress key={workspace.taskId ?? 'plan'} events={session.events} variant="dock" />}
            {isChild
              ? (
                  <div className={readonlyCss.frame}>
                    <span>{t('app.childReadonly')}</span>
                    <button
                      type="button"
                      className={css.readonlyAction}
                      onClick={() => { openTask(parentId) }}
                    >
                      {t('app.backToParent')}
                    </button>
                  </div>
                )
              : renderSlot('agent.composer', {
                  variant: 'composer',
                  // The text follows the session, not the box: come back and it is still there.
                  draftKey: workspace.taskId ?? NEW_CHAT_DRAFT,
                  // A running turn still takes messages: interjections or a queue.
                  disabled: !ready || (!canContinue && !running),
                  note: composerNote,
                  running,
                  workspacePath: workspace.activeWorkspace ?? workspace.info?.workspace,
                  workspaces: workspace.workspaces,
                  activeWorkspace: workspace.activeWorkspace,
                  onSelectWorkspace: workspace.selectWorkspace,
                  onOpenAddWorkspace: workspace.openFolderDialog,
                  modelName: workspace.modelName ?? undefined,
                  modelProviderId: workspace.modelProviderId,
                  reasoningEffort: workspace.reasoningEffort ?? undefined,
                  fastMode: workspace.fastMode,

                  settings: workspace.settings,
                  onSelectModel: workspace.selectModel,
                  onSetGenerationOptions: workspace.setGenerationOptions,
                  onOpenSettings: workspace.openSettings,
                  permissionMode: workspace.permissionMode,
                  onSelectPermissionMode: workspace.selectPermissionMode,
                  onSubmit: submit,
                  onStop: stop,
                  onInterject: workspace.interject,
                  contextUsage,
                })}
          </div>
        </div>
      </div>
    </main>
  )
}
