// Drawn on a 24px grid with a 2px round stroke and generous corner radii, so the
// shapes read full rather than spindly, as on the prototype.
const PATHS = {
  folder: <path d="M3.5 7.5a2.5 2.5 0 0 1 2.5-2.5h3.3a2 2 0 0 1 1.4.6L12.2 7H18a2.5 2.5 0 0 1 2.5 2.5V17a2.5 2.5 0 0 1-2.5 2.5H6A2.5 2.5 0 0 1 3.5 17z" />,
  file: <><path d="M14 3.5H7.5A2.5 2.5 0 0 0 5 6v12a2.5 2.5 0 0 0 2.5 2.5h9A2.5 2.5 0 0 0 19 18V8.5z" /><path d="M14 3.5V7a1.5 1.5 0 0 0 1.5 1.5H19M9 13h6M9 16.5h4" /></>,
  search: <><circle cx="11" cy="11" r="6.5" /><path d="m16 16 4 4" /></>,
  code: <path d="m8.5 7.5-4.5 4.5 4.5 4.5m7-9 4.5 4.5-4.5 4.5" />,
  user: <><circle cx="12" cy="8.5" r="4" /><path d="M4.5 20a7.5 7.5 0 0 1 15 0" /></>,
  chat: <path d="M4 7a3 3 0 0 1 3-3h10a3 3 0 0 1 3 3v7a3 3 0 0 1-3 3h-6l-4.2 3.2a.5.5 0 0 1-.8-.4V17a3 3 0 0 1-2-2.8z" />,
  external: <><path d="M14 4h6v6M20 4l-8.5 8.5" /><path d="M18 13.5V17a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3V9a3 3 0 0 1 3-3h3.5" /></>,
  copy: <><rect x="8.5" y="8.5" width="11.5" height="11.5" rx="2.5" /><path d="M15.5 8.5V6.5a2.5 2.5 0 0 0-2.5-2.5H6.5A2.5 2.5 0 0 0 4 6.5V13a2.5 2.5 0 0 0 2.5 2.5h2" /></>,
  check: <path d="m5.5 12.5 4 4 9-9" />,
  spark: <path d="M12 3.5c.5 3.9 1.6 5 5.5 5.5-3.9.5-5 1.6-5.5 5.5-.5-3.9-1.6-5-5.5-5.5 3.9-.5 5-1.6 5.5-5.5zm6 11c.2 1.6.9 2.3 2.5 2.5-1.6.2-2.3.9-2.5 2.5-.2-1.6-.9-2.3-2.5-2.5 1.6-.2 2.3-.9 2.5-2.5z" />,
  send: <path d="M20.2 4.2 3.8 10.6a.6.6 0 0 0 0 1.1l6.4 2.1 2.1 6.4a.6.6 0 0 0 1.1 0l6.4-16.4a.6.6 0 0 0-.8-.8zM10.2 13.8l4.3-4.3" />,
  plug: <><path d="M11 3.6a2 2 0 0 1 2 0l6 3.4a2 2 0 0 1 1 1.8v6.4a2 2 0 0 1-1 1.8l-6 3.4a2 2 0 0 1-2 0l-6-3.4a2 2 0 0 1-1-1.8V8.8a2 2 0 0 1 1-1.8z" /><path d="m4.4 7.8 7.6 4.4 7.6-4.4M12 12.2v8.4" /></>,
  chevron: <path d="m9.5 6 6 6-6 6" />,
  chevronDown: <path d="m6 9.5 6 6 6-6" />,
  play: <path d="M8 6.2v11.6a1.2 1.2 0 0 0 1.8 1l9.2-5.8a1.2 1.2 0 0 0 0-2L9.8 5.2A1.2 1.2 0 0 0 8 6.2z" />,
  info: <><circle cx="12" cy="12" r="8.5" /><path d="M12 11v5M12 7.9v.1" /></>,
  close: <path d="m7 7 10 10M17 7 7 17" />,
  minus: <path d="M6 12h12" />,
  expand: <path d="M14.5 4.5h5v5M9.5 19.5h-5v-5M19.5 4.5 14 10M4.5 19.5 10 14" />,
  shrink: <path d="M19.5 9.5h-5v-5M4.5 14.5h5v5M14.5 9.5l5-5M9.5 14.5l-5 5" />,
  pause: <><circle cx="12" cy="12" r="8.5" /><path d="M10 9.5v5M14 9.5v5" /></>,
  bars: <path d="M9.5 7.5v9M14.5 7.5v9" />,
  alert: <><circle cx="12" cy="12" r="8.5" /><path d="m9.2 9.2 5.6 5.6m0-5.6-5.6 5.6" /></>,
  question: <><circle cx="12" cy="12" r="8.5" /><path d="M9.8 9.7a2.3 2.3 0 0 1 4.4.9c0 1.5-2.2 1.9-2.2 3.3M12 16.8v.1" /></>,
  ok: <><circle cx="12" cy="12" r="8.5" /><path d="m8.4 12.3 2.4 2.4 4.8-5" /></>,
  list: <path d="M5 6.5h14M5 12h14M5 17.5h14" />,
  panel: <><rect x="3.5" y="4.5" width="17" height="15" rx="3" /><path d="M14.5 4.5v15" /></>,
  stop: <rect x="7" y="7" width="10" height="10" rx="2.5" />,
  clip: <path d="m19 11.5-6.8 6.8a4.2 4.2 0 0 1-6-6l7.3-7.3a2.8 2.8 0 0 1 4 4l-7.3 7.3a1.4 1.4 0 0 1-2-2l6.6-6.6" />,
  back: <path d="M19 12H5m6-6.5L4.5 12l6.5 6.5" />,
  refresh: <><path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" /><path d="M19.5 4.5v4.2h-4.2" /></>,
  power: <><path d="M12 3.5V11" /><path d="M7.2 6.3a7.5 7.5 0 1 0 9.6 0" /></>,
  trash: <><path d="M4.5 6.5h15M9.5 6.5V5a1.5 1.5 0 0 1 1.5-1.5h2A1.5 1.5 0 0 1 14.5 5v1.5" /><path d="M6.5 6.5l.8 11.6a2.5 2.5 0 0 0 2.5 2.4h4.4a2.5 2.5 0 0 0 2.5-2.4l.8-11.6M10 10.5v6M14 10.5v6" /></>,
  link: <><path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1" /><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" /></>,
  box: <><path d="M11 3.6a2 2 0 0 1 2 0l6 3.4a2 2 0 0 1 1 1.8v6.4a2 2 0 0 1-1 1.8l-6 3.4a2 2 0 0 1-2 0l-6-3.4a2 2 0 0 1-1-1.8V8.8a2 2 0 0 1 1-1.8z" /><path d="m4.4 7.8 7.6 4.4 7.6-4.4M12 12.2v8.4" /></>,
} as const

export type IconName = keyof typeof PATHS

/** The workbench's line icons, drawn in the current text colour. */
export function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  // Small icons keep a visible stroke: 2 units at 24px is only 1.2px at 14px.
  const stroke = size <= 15 ? 2.2 : 2
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={stroke}
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{PATHS[name]}</svg>
}
