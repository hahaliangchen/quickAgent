const PATHS = {
  folder: <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2h8.5A1.5 1.5 0 0 1 21 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z" />,
  file: <><path d="M6.5 3h7.5l4.5 4.5V20a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" /><path d="M14 3v4.5h4.5M9 12.5h6M9 16h4.5" /></>,
  search: <><circle cx="10.5" cy="10.5" r="6.5" /><path d="m15.5 15.5 4.5 4.5" /></>,
  code: <path d="m8.5 7-5 5 5 5m7-10 5 5-5 5" />,
  user: <><circle cx="12" cy="8.5" r="3.6" /><path d="M5 20c.7-3.6 3.5-5.6 7-5.6s6.3 2 7 5.6" /></>,
  chat: <path d="M5 5.5h14a1.5 1.5 0 0 1 1.5 1.5v8.5A1.5 1.5 0 0 1 19 17h-8.5L6 20.5V17H5a1.5 1.5 0 0 1-1.5-1.5V7A1.5 1.5 0 0 1 5 5.5z" />,
  external: <><path d="M13.5 4.5H19.5v6M19.5 4.5 11 13" /><path d="M17.5 13.5v5a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-11a1 1 0 0 1 1-1h5" /></>,
  copy: <><rect x="8.5" y="8.5" width="11" height="11" rx="1.5" /><path d="M15.5 8.5v-3a1 1 0 0 0-1-1h-9a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h3" /></>,
  check: <path d="m5.5 12.5 4 4 9-9" />,
  spark: <path d="M12 3.5 13.9 9l5.6 1.9-5.6 1.9L12 18.5l-1.9-5.7-5.6-1.9L10.1 9z" />,
  send: <path d="M4.5 12 19.5 5l-4 14.5-3.5-6.2zm7.5 1.3 7.5-8.3" />,
  plug: <><path d="m12 3 8 4.5v9L12 21l-8-4.5v-9z" /><path d="m4 7.5 8 4.5 8-4.5M12 12v9" /></>,
  chevron: <path d="m9.5 6 6 6-6 6" />,
  chevronDown: <path d="m6 9.5 6 6 6-6" />,
  play: <path d="M8 5.5v13l10.5-6.5z" />,
  info: <><circle cx="12" cy="12" r="8.5" /><path d="M12 11v5.5M12 7.8v.2" /></>,
  close: <path d="m6.5 6.5 11 11m0-11-11 11" />,
  minus: <path d="M6 12h12" />,
  expand: <path d="M14 4.5h5.5V10M10 19.5H4.5V14M19.5 4.5l-6 6M4.5 19.5l6-6" />,
  shrink: <path d="M19.5 10H14V4.5M4.5 14H10v5.5M14 10l6-6M10 14l-6 6" />,
  pause: <><circle cx="12" cy="12" r="8.5" /><path d="M10 9v6M14 9v6" /></>,
  bars: <path d="M9.5 7.5v9M14.5 7.5v9" />,
  alert: <><circle cx="12" cy="12" r="8.5" /><path d="m9 9 6 6m0-6-6 6" /></>,
  question: <><circle cx="12" cy="12" r="8.5" /><path d="M9.8 9.6a2.3 2.3 0 0 1 4.4.9c0 1.6-2.2 1.9-2.2 3.4M12 16.6v.2" /></>,
  ok: <><circle cx="12" cy="12" r="8.5" /><path d="m8.3 12.3 2.5 2.5 5-5.2" /></>,
  list: <path d="M4.5 6.5h15M4.5 12h15M4.5 17.5h15" />,
  panel: <><rect x="3.5" y="4.5" width="17" height="15" rx="1.5" /><path d="M14.5 4.5v15" /></>,
  stop: <rect x="7" y="7" width="10" height="10" rx="1.5" />,
  clip: <path d="m19 11.5-6.8 6.8a4.2 4.2 0 0 1-6-6l7.3-7.3a2.8 2.8 0 0 1 4 4l-7.3 7.3a1.4 1.4 0 0 1-2-2l6.6-6.6" />,
  box: <><path d="m12 3 8 4.5v9L12 21l-8-4.5v-9z" /><path d="m4 7.5 8 4.5 8-4.5M12 12v9" /></>,
} as const

export type IconName = keyof typeof PATHS

/** The workbench's line icons, drawn in the current text colour. */
export function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{PATHS[name]}</svg>
}
