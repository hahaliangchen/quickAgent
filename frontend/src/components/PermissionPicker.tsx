import { useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import { RiskConfirmation } from '@deepseek-ai/dsh-client-ui-primitives'
import type { PermissionMode } from '../api.ts'
import codexCss from './CodexComposer.module.css'

const FULL_ACCESS_ICON = (
  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.6" strokeLinejoin="round" aria-hidden="true">
    <path d="M9 1.5 3.5 9H8l-1 5.5L12.5 7H8l1-5.5Z" />
  </svg>
)

const PERMISSION_SHIELD =
  'M6.59624 2.14853C7.50155 1.80917 8.49914 1.80919 9.40444 2.14859L13.9245 3.84317V7.11961C13.9245 11.6089 10.5565 13.5975 8.00035 14.5779C5.44423 13.5975 2.07544 11.6089 2.07544 7.11961V3.84317L6.59624 2.14853Z'

const ASK_ICON = (
  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d={PERMISSION_SHIELD} />
    <path d="M6.5 6.2a1.5 1.5 0 1 1 2.46 1.15c-.45.42-.96.6-.96.85" strokeWidth="1.5" />
    <circle cx="8" cy="10.35" r="0.66" fill="currentColor" stroke="none" />
  </svg>
)

const AUTO_REVIEW_ICON = (
  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d={PERMISSION_SHIELD} />
    <path d="M5.55 8.2 7.32 9.97 10.45 6.15" strokeWidth="1.7" />
  </svg>
)

const WORKSPACE_WRITE_ICON = (
  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6" aria-hidden="true">
    <path d="M6.4209 1.68067C7.43922 1.299 8.56177 1.29898 9.58008 1.68067L14.0996 3.375C14.2946 3.44811 14.4236 3.63455 14.4238 3.84278V6.89063C14.1115 6.71853 13.7761 6.58312 13.4238 6.48926V4.18946L9.22852 2.61621C8.43657 2.31947 7.56341 2.31939 6.77148 2.61621L2.5752 4.18946V7.11914C2.5752 11.1796 5.52369 13.056 8 14.0391C8.27653 13.9293 8.55827 13.8067 8.8418 13.6729C9.07101 13.9468 9.33228 14.1929 9.62012 14.4053C9.12409 14.6579 8.63578 14.8696 8.17871 15.0449C8.0637 15.0889 7.93628 15.0889 7.82129 15.0449C5.22011 14.0472 1.5752 11.9381 1.5752 7.11914V3.84278C1.57541 3.63469 1.70463 3.44821 1.89941 3.375L6.4209 1.68067Z" fill="currentColor" />
    <path d="M5.26392 6.60339H10.7361" stroke="currentColor" strokeLinecap="round" />
    <path d="M5.26392 9.86902H8.32833" stroke="currentColor" strokeLinecap="round" />
  </svg>
)

const READ_ONLY_ICON = (
  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6" stroke="currentColor" aria-hidden="true">
    <path d="M5.08545 8.13775L7.18455 10.2368C7.26636 10.3187 7.4003 10.3142 7.47649 10.2271L11.5148 5.61194" strokeLinecap="round" strokeLinejoin="round" />
    <path d={PERMISSION_SHIELD} strokeLinejoin="round" />
  </svg>
)

const CHECK_ICON = (
  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <polyline points="20 6 9 17 4 12" />
  </svg>
)

const CHEVRON_ICON = (
  <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ opacity: 0.7 }}>
    <polyline points="4 6 8 10 12 6" />
  </svg>
)

const LABELS: Readonly<Record<PermissionMode, string>> = {
  read_only: '仅可查看',
  workspace_write: '工作区内修改',
  ask: '执行前询问',
  auto_review: '自动审批',
  full_access: '完全权限',
}

function modeIcon(mode: PermissionMode) {
  switch (mode) {
    case 'full_access': return FULL_ACCESS_ICON
    case 'ask': return ASK_ICON
    case 'auto_review': return AUTO_REVIEW_ICON
    case 'workspace_write': return WORKSPACE_WRITE_ICON
    case 'read_only': return READ_ONLY_ICON
  }
}

const POPOVER_WIDTH = 350

/**
 * Interactive permission-mode pill + popover + full-access confirmation dialog,
 * matching the main Composer's picker while clamping safely inside the viewport
 * when docked near the right edge of the window.
 */
export function PermissionPicker({
  mode,
  disabled = false,
  className,
  onSelect,
}: {
  mode: PermissionMode
  disabled?: boolean | undefined
  className?: string | undefined
  onSelect: (mode: PermissionMode) => void
}) {
  const [menuOpen, setMenuOpen] = useState(false)
  const [menuPos, setMenuPos] = useState<{ top?: number; bottom?: number; left: number }>({ left: 16 })
  const [showRiskConfirm, setShowRiskConfirm] = useState(false)
  const [riskAcknowledged, setRiskAcknowledged] = useState(false)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!menuOpen) return
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null
      if (target !== null && !triggerRef.current?.contains(target) && !menuRef.current?.contains(target)) {
        setMenuOpen(false)
      }
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setMenuOpen(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    window.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [menuOpen])

  const toggleMenu = () => {
    if (disabled) return
    if (menuOpen) {
      setMenuOpen(false)
      return
    }
    const rect = triggerRef.current?.getBoundingClientRect()
    if (rect !== undefined) {
      const maxLeft = Math.max(16, window.innerWidth - POPOVER_WIDTH - 16)
      const left = Math.max(16, Math.min(rect.left, maxLeft))
      if (rect.top > 320) {
        setMenuPos({ bottom: window.innerHeight - rect.top + 8, left })
      } else {
        setMenuPos({ top: rect.bottom + 8, left })
      }
    }
    setMenuOpen(true)
  }

  const handleSelect = (next: PermissionMode) => {
    setMenuOpen(false)
    if (next === 'full_access' && mode !== 'full_access') {
      setRiskAcknowledged(false)
      setShowRiskConfirm(true)
      return
    }
    onSelect(next)
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        disabled={disabled}
        className={clsx(
          codexCss.permPill,
          mode === 'auto_review' || mode === 'workspace_write' || mode === 'read_only' ? codexCss.permPillNeutral : undefined,
          mode === 'ask' && codexCss.permPillSafe,
          className,
        )}
        onClick={toggleMenu}
        title="切换工作区操作权限"
        aria-haspopup="dialog"
        aria-expanded={menuOpen}
      >
        {modeIcon(mode)}
        <span>{LABELS[mode]}</span>
        {CHEVRON_ICON}
      </button>

      {menuOpen && createPortal(
        <div
          ref={menuRef}
          className={codexCss.permPopover}
          style={{ top: menuPos.top, bottom: menuPos.bottom, left: menuPos.left }}
          role="dialog"
          aria-label="工作区操作权限"
        >
          <div className={codexCss.permPopoverHeader}>
            <span className={codexCss.permPopoverTitle}>工作区操作权限</span>
          </div>

          <div className={codexCss.permOptionList}>
            <button type="button" className={codexCss.permOptionItem} onClick={() => handleSelect('ask')}>
              <span className={codexCss.permOptionIcon}>{ASK_ICON}</span>
              <div className={codexCss.permOptionBody}>
                <span className={codexCss.permOptionTitle}>执行前询问</span>
                <span className={codexCss.permOptionDesc}>运行命令、访问跨项目目录或打开浏览器前先询问；敏感凭据与宿主保护规则始终生效。</span>
              </div>
              {mode === 'ask' && <span className={codexCss.permOptionCheck}>{CHECK_ICON}</span>}
            </button>

            <button type="button" className={codexCss.permOptionItem} onClick={() => handleSelect('auto_review')}>
              <span className={codexCss.permOptionIcon}>{AUTO_REVIEW_ICON}</span>
              <div className={codexCss.permOptionBody}>
                <span className={codexCss.permOptionTitle}>自动审批</span>
                <span className={codexCss.permOptionDesc}>需要确认的操作先由 AI 对照你的要求审查：明确该做的直接放行，明确不该做的拒绝，拿不准时再问你。</span>
              </div>
              {mode === 'auto_review' && <span className={codexCss.permOptionCheck}>{CHECK_ICON}</span>}
            </button>

            <button type="button" className={codexCss.permOptionItem} onClick={() => handleSelect('full_access')}>
              <span className={clsx(codexCss.permOptionIcon, codexCss.permOptionIconWarn)}>{FULL_ACCESS_ICON}</span>
              <div className={codexCss.permOptionBody}>
                <span className={codexCss.permOptionTitle}>完全权限</span>
                <span className={codexCss.permOptionDesc}>不再询问，agent 自己运行命令和浏览器。适合说完就交给它、不想被打断的时候</span>
              </div>
              {mode === 'full_access' && <span className={codexCss.permOptionCheck}>{CHECK_ICON}</span>}
            </button>
          </div>
        </div>,
        document.body,
      )}

      {showRiskConfirm && (
        <RiskConfirmation
          open={showRiskConfirm}
          title="切换到完全权限？"
          description="之后 agent 会直接运行命令、打开浏览器，不再逐条询问。命令以你的系统账户运行；敏感凭据与宿主保护规则仍然生效。只需要确认这一次。"
          acknowledgeLabel="我了解，之后不用再问"
          cancelLabel="取消"
          closeLabel="关闭"
          confirmLabel="切换"
          acknowledged={riskAcknowledged}
          onAcknowledgedChange={setRiskAcknowledged}
          onCancel={() => {
            setShowRiskConfirm(false)
            setRiskAcknowledged(false)
          }}
          onConfirm={() => {
            setShowRiskConfirm(false)
            setRiskAcknowledged(false)
            onSelect('full_access')
          }}
        />
      )}
    </>
  )
}
