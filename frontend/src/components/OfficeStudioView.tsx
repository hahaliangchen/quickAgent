import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import { usePluginPages } from '../plugins/react.tsx'
import type { StudioAskAiRequest, StudioViewContext } from '../plugins/protocol.ts'
import { buildStudioCatalog, type StudioModuleCard, type StudioModuleIcon } from '../officeStudio.ts'
import type { PendingOpen, PreviewTheme } from '../filePreview.ts'
import { PluginPageHost } from './PluginPageHost.tsx'
import css from './OfficeStudioView.module.css'

const ICON_STROKE = {
  width: 22,
  height: 22,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 1.9,
  strokeLinecap: 'round',
  strokeLinejoin: 'round',
} as const

function ModuleGlyph({ kind, size = 22 }: { kind: StudioModuleIcon; size?: number }) {
  const props = { ...ICON_STROKE, width: size, height: size, 'aria-hidden': true as const }
  switch (kind) {
    case 'ppt':
      return (
        <svg {...props}>
          <rect x="3" y="4" width="18" height="12" rx="2" />
          <path d="M8 20h8M12 16v4" />
          <path d="M7.5 9h4M7.5 12h7.5" />
        </svg>
      )
    case 'word':
      return (
        <svg {...props}>
          <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
          <path d="M14 3v5h5M8.5 13h7M8.5 16.5h5" />
        </svg>
      )
    case 'markdown':
      return (
        <svg {...props}>
          <rect x="3" y="5" width="18" height="14" rx="2" />
          <path d="M6.5 15V9l2.5 3 2.5-3v6" />
          <path d="M16 9v6m0 0-2-2m2 2 2-2" />
        </svg>
      )
    case 'excel':
      return (
        <svg {...props}>
          <rect x="3.5" y="4" width="17" height="16" rx="2" />
          <path d="M3.5 9.5h17M3.5 14.5h17M9.5 4v16" />
        </svg>
      )
    case 'pdf':
      return (
        <svg {...props}>
          <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
          <path d="M14 3v5h5" />
          <path d="M8.5 14.5h2a1.25 1.25 0 0 0 0-2.5h-2v4.5" />
          <path d="M13.5 12h1.2a1.8 1.8 0 0 1 0 3.6h-1.2z" />
        </svg>
      )
    default:
      return (
        <svg {...props}>
          <rect x="4" y="4" width="16" height="16" rx="3" />
          <path d="M8 9h8M8 13h8M8 17h5" />
        </svg>
      )
  }
}

function accentClass(accent: StudioModuleCard['accent']): string | undefined {
  switch (accent) {
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
 * - Shows a modular card grid automatically driven by bundled/enabled plugins (PPT, Word, Markdown, Excel...).
 * - Clicking a ready module opens it inside a keep-alive workspace with a slim top switcher bar so switching
 *   between modules or returning to Hub/Chat preserves live editing state without reloading.
 */
export function OfficeStudioView({ workspaceRoot, resetSignal, pendingOpen, theme, onManagePlugins, onStudioContext, onAskAi }: OfficeStudioViewProps) {
  const { packages } = usePluginPages()
  const catalog = useMemo(() => buildStudioCatalog(packages), [packages])
  const readyCards = useMemo(() => catalog.filter(item => item.ready && item.owner && item.page), [catalog])
  const [opened, setOpened] = useState<OpenedModule[]>([])
  const [activeKey, setActiveKey] = useState<string | null>(null)
  const moduleContexts = useRef<Map<string, StudioViewContext | null>>(new Map())

  useEffect(() => {
    onStudioContext?.(activeKey ? (moduleContexts.current.get(activeKey) ?? null) : null)
  }, [activeKey, onStudioContext])

  const handleModuleContext = useCallback((key: string, ctx: StudioViewContext | null) => {
    moduleContexts.current.set(key, ctx)
    if (key === activeKey) onStudioContext?.(ctx)
  }, [activeKey, onStudioContext])

  // Clicking the left Dock "Office" icon while already inside a module returns to Hub overview.
  useEffect(() => {
    if (resetSignal && resetSignal > 0) setActiveKey(null)
  }, [resetSignal])

  // Drop any keep-alive pane whose plugin was disabled or uninstalled.
  useEffect(() => {
    const valid = new Set(readyCards.map(item => item.key))
    setOpened(prev => prev.filter(item => valid.has(item.key)))
    setActiveKey(prev => (prev && valid.has(prev) ? prev : null))
  }, [readyCards])

  useEffect(() => {
    if (!pendingOpen) return
    const card = readyCards.find(item => item.slotId === pendingOpen.slotId)
    if (!card?.owner || !card.page) return
    const { key, owner, page } = card
    setOpened(prev => prev.some(item => item.key === key)
      ? prev.map(item => item.key === key ? { ...item, request: pendingOpen } : item)
      : [...prev, { key, owner, page, reloadTick: 0, request: pendingOpen }])
    setActiveKey(key)
  }, [pendingOpen, readyCards])

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
  }

  const closeModule = (key: string) => {
    moduleContexts.current.delete(key)
    setOpened(prev => prev.filter(item => item.key !== key))
    setActiveKey(prev => (prev === key ? null : prev))
  }

  const reloadModule = (key: string) => {
    setOpened(prev => prev.map(item => (
      item.key === key ? { ...item, reloadTick: item.reloadTick + 1 } : item
    )))
  }

  const openedSet = useMemo(() => new Set(opened.map(item => item.key)), [opened])

  return (
    <div className={css.root}>
      {activeKey !== null && (
        <header className={css.topBar}>
          <div className={css.barLeft}>
            <button
              type="button"
              className={css.backBtn}
              onClick={() => setActiveKey(null)}
              title="返回办公创作台总览（当前打开的编辑器保持运行）"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="m15 18-6-6 6-6" />
              </svg>
              <span>办公创作台</span>
            </button>
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
    </div>
  )
}
