import type { CSSProperties } from 'react'

export interface ParchmentQuillIconProps {
  readonly size?: number | undefined
  /** `line`: 2px stroke matching CodexDock/toolbar icons. `emblem`: warm parchment + emerald quill hero illustration. */
  readonly variant?: 'line' | 'emblem' | undefined
  readonly className?: string | undefined
  readonly style?: CSSProperties | undefined
}

/**
 * Unified Blueprint emblem: rolled parchment scroll + feather quill pen.
 */
export function ParchmentQuillIcon({
  size = 20,
  variant = 'line',
  className,
  style,
}: ParchmentQuillIconProps) {
  if (variant === 'emblem') {
    return (
      <svg
        width={size}
        height={size}
        viewBox="0 0 64 64"
        fill="none"
        className={className}
        style={{ display: 'block', ...style }}
        aria-hidden="true"
      >
        {/* Warm parchment scroll body */}
        <path
          d="M14 14C14 10.686 16.686 8 20 8H44C47.314 8 50 10.686 50 14V18H45V50C45 53.314 42.314 56 39 56H15C11.686 56 9 53.314 9 50V47H14V14Z"
          fill="var(--bp-parchment-bg, #fbf5e6)"
          stroke="var(--bp-parchment-stroke, #b58d52)"
          strokeWidth="2.2"
          strokeLinejoin="round"
        />
        {/* Top curl roll */}
        <path
          d="M44 8C47.314 8 50 10.686 50 14C50 16.209 48.209 18 46 18H45V12C45 9.791 44.5 8.5 44 8Z"
          fill="var(--bp-parchment-roll, #ead7b0)"
          stroke="var(--bp-parchment-stroke, #b58d52)"
          strokeWidth="2"
          strokeLinejoin="round"
        />
        {/* Bottom curl roll */}
        <path
          d="M9 47H35C35 51.97 36.8 56 39 56H15C11.686 56 9 53.314 9 50V47Z"
          fill="var(--bp-parchment-roll, #ead7b0)"
          stroke="var(--bp-parchment-stroke, #b58d52)"
          strokeWidth="2"
          strokeLinejoin="round"
        />
        {/* Subtle parchment blueprint lines */}
        <path
          d="M20 20H34M20 27H32M20 34H29M20 41H26"
          stroke="var(--bp-parchment-ink, #9c7a47)"
          strokeWidth="2"
          strokeLinecap="round"
          opacity="0.72"
        />
        {/* Feather quill pen (emerald accent matching QuickAgent brand) */}
        <path
          d="M55.5 9.5C47.5 11.2 40.2 17.5 36.2 26.2L32.8 33.8L38.2 35.6C46.4 31.8 53.4 23.6 55.8 13.6C56.1 12.1 56 10.6 55.5 9.5Z"
          fill="var(--codex-blue-primary, #1d9e75)"
          fillOpacity="0.18"
          stroke="var(--codex-blue-primary, #1d9e75)"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        {/* Quill rachis (central spine) & nib */}
        <path
          d="M54 11.5L31.5 39.5M32.8 33.8L29.5 42L38.2 35.6"
          stroke="var(--codex-blue-primary, #1d9e75)"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        {/* Feather notches */}
        <path
          d="M46.5 17L42 18.5M42 23.5L37.5 25"
          stroke="var(--codex-blue-primary, #1d9e75)"
          strokeWidth="1.8"
          strokeLinecap="round"
        />
      </svg>
    )
  }

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      style={{ display: 'block', ...style }}
      aria-hidden="true"
    >
      {/* Rolled parchment scroll */}
      <path d="M5 17V5.5A2.5 2.5 0 0 1 7.5 3H16a2 2 0 0 1 2 2v1.5" />
      <path d="M16 14.5V18a3 3 0 0 1-3 3H5.5A2.5 2.5 0 0 1 3 18.5V17h9.5a2 2 0 0 1 2 2" />
      <path d="M8.5 7.5H13M8.5 11H11.5" />
      {/* Diagonal feather quill pen */}
      <path d="M21 5c-4.2.5-7.3 3.2-9 7.6l-.8 2.2 2.2-.8C17.8 12.3 20.5 9.2 21 5Z" />
      <path d="M11.2 14.8 19 7" />
    </svg>
  )
}
