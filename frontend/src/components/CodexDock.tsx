import clsx from 'clsx'
import css from './CodexDock.module.css'

/** The rail's icons: a 2px round stroke on a 24px grid, like the workbench's. */
const STROKE = { width: 20, height: 20, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 2, strokeLinecap: 'round', strokeLinejoin: 'round', style: { display: 'block' } } as const

export type DockTab = 'home' | 'skills'

interface CodexDockProps {
  activeTab?: DockTab
  onSelectTab?: (tab: DockTab) => void
  onNewChat?: () => void
  onOpenSettings?: () => void
  onToggleTheme?: () => void
  dark?: boolean
}

export function CodexDock({
  activeTab = 'home',
  onSelectTab,
  onNewChat,
  onOpenSettings,
  onToggleTheme,
  dark,
}: CodexDockProps) {
  return (
    <aside className={css.dock} aria-label="应用主导航">
      <div className={css.topGroup}>
        {/* 1. App Logo / Home */}
        <button
          type="button"
          className={clsx(css.dockBtn, css.homeBtn, activeTab === 'home' && css.active)}
          title="首页 / 新会话"
          onClick={() => {
            onSelectTab?.('home')
            if (activeTab === 'home') onNewChat?.()
          }}
        >
          <svg {...STROKE} aria-hidden="true">
            <path d="M4 10.4a2 2 0 0 1 .7-1.5l6-5.2a2 2 0 0 1 2.6 0l6 5.2a2 2 0 0 1 .7 1.5V18a2.5 2.5 0 0 1-2.5 2.5H6.5A2.5 2.5 0 0 1 4 18z" />
            <path d="M9.5 20.5v-5a1.5 1.5 0 0 1 1.5-1.5h2a1.5 1.5 0 0 1 1.5 1.5v5" />
          </svg>
        </button>

        <button
          type="button"
          className={clsx(css.dockBtn, activeTab === 'skills' && css.active)}
          title="技能工作台"
          aria-label="技能工作台"
          onClick={() => onSelectTab?.('skills')}
        >
          <svg {...STROKE} aria-hidden="true">
            <rect x="4" y="4" width="6.5" height="6.5" rx="2" /><rect x="13.5" y="4" width="6.5" height="6.5" rx="2" />
            <rect x="4" y="13.5" width="6.5" height="6.5" rx="2" /><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="2" />
          </svg>
        </button>

      </div>

      <div className={css.bottomGroup}>
        {/* Settings / Sliders */}
        <button
          type="button"
          className={css.dockBtn}
          title="设置与首选项"
          onClick={onOpenSettings}
        >
          <svg {...STROKE} aria-hidden="true">
            <path d="M10.3 3.9a1.8 1.8 0 0 1 3.4 0l.3.9a1.8 1.8 0 0 0 2.4 1l.9-.4a1.8 1.8 0 0 1 2.4 2.4l-.4.9a1.8 1.8 0 0 0 1 2.4l.9.3a1.8 1.8 0 0 1 0 3.4l-.9.3a1.8 1.8 0 0 0-1 2.4l.4.9a1.8 1.8 0 0 1-2.4 2.4l-.9-.4a1.8 1.8 0 0 0-2.4 1l-.3.9a1.8 1.8 0 0 1-3.4 0l-.3-.9a1.8 1.8 0 0 0-2.4-1l-.9.4a1.8 1.8 0 0 1-2.4-2.4l.4-.9a1.8 1.8 0 0 0-1-2.4l-.9-.3a1.8 1.8 0 0 1 0-3.4l.9-.3a1.8 1.8 0 0 0 1-2.4l-.4-.9a1.8 1.8 0 0 1 2.4-2.4l.9.4a1.8 1.8 0 0 0 2.4-1z" />
            <circle cx="12" cy="12" r="3" />
          </svg>
        </button>

        <button
          type="button"
          className={css.dockBtn}
          title={dark ? '切换为浅色' : '切换为深色'}
          aria-label={dark ? '切换为浅色' : '切换为深色'}
          onClick={onToggleTheme}
        >
          {dark
            ? <svg {...STROKE} aria-hidden="true"><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></svg>
            : <svg {...STROKE} aria-hidden="true"><path d="M19.5 14.6A7.5 7.5 0 0 1 9.4 4.5a.4.4 0 0 0-.5-.5 8.5 8.5 0 1 0 11.1 11.1.4.4 0 0 0-.5-.5z" /></svg>}
        </button>
      </div>
    </aside>
  )
}
