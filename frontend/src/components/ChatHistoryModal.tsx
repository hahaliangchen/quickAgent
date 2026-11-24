import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import {
  api,
  type HistoryDates,
  type HistoryFile,
  type HistoryImage,
  type HistoryMessage,
} from '../api.ts'
import type { OpenFilePreview } from '../filePreview.ts'
import css from './ChatHistoryModal.module.css'

type Scope = 'session' | 'all'
type Tab = 'all' | 'images' | 'files' | 'dates'

/** What the dialog asks the transcript to reveal. */
export interface HistoryJump {
  readonly task_id: string
  readonly key: string
  readonly turn: number
}

const TABS: readonly { id: Tab; label: string }[] = [
  { id: 'all', label: '全部' },
  { id: 'images', label: '图片' },
  { id: 'files', label: '文件' },
  { id: 'dates', label: '日期' },
]

const WEEKDAYS = ['一', '二', '三', '四', '五', '六', '日']

function pad(value: number): string {
  return value < 10 ? '0' + value : String(value)
}

/** The local calendar day of a millisecond timestamp. */
function dayOf(timestamp: number): string {
  const date = new Date(timestamp)
  return date.getFullYear() + '-' + pad(date.getMonth() + 1) + '-' + pad(date.getDate())
}

function clockOf(timestamp: number): string {
  const date = new Date(timestamp)
  return pad(date.getHours()) + ':' + pad(date.getMinutes())
}

/** 今天 / 昨天 / 2026-11-22, the way a chat app labels a day. */
function dayLabel(day: string): string {
  if (day === dayOf(Date.now())) return '今天'
  if (day === dayOf(Date.now() - 86_400_000)) return '昨天'
  return day
}

function stampLabel(timestamp: number): string {
  if (timestamp <= 0) return ''
  return dayLabel(dayOf(timestamp)) + ' ' + clockOf(timestamp)
}

function sizeLabel(bytes: number | null | undefined): string {
  if (bytes === undefined || bytes === null || bytes <= 0) return ''
  if (bytes < 1024) return bytes + ' B'
  if (bytes < 1024 * 1024) return Math.round(bytes / 1024) + ' KB'
  return (bytes / 1024 / 1024).toFixed(1) + ' MB'
}

/** Splits text into matched and unmatched pieces so a hit is visible at a glance. */
function matched(text: string, needle: string): { text: string; hit: boolean }[] {
  const term = needle.trim()
  if (term === '') return [{ text, hit: false }]
  const pieces: { text: string; hit: boolean }[] = []
  const lower = text.toLowerCase()
  const target = term.toLowerCase()
  let at = 0
  for (;;) {
    const found = lower.indexOf(target, at)
    if (found < 0) break
    if (found > at) pieces.push({ text: text.slice(at, found), hit: false })
    pieces.push({ text: text.slice(found, found + term.length), hit: true })
    at = found + term.length
  }
  if (at < text.length) pieces.push({ text: text.slice(at), hit: false })
  return pieces.length === 0 ? [{ text, hit: false }] : pieces
}

function Highlighted({ text, needle }: { text: string; needle: string }) {
  const pieces = useMemo(() => matched(text, needle), [text, needle])
  return (
    <>
      {pieces.map((piece, index) => piece.hit
        ? <mark key={index} className={css.hit}>{piece.text}</mark>
        : <span key={index}>{piece.text}</span>)}
    </>
  )
}

/** Rows grouped under the day they happened, newest day first. */
function groupByDay<T>(items: readonly T[], stamp: (item: T) => number): { day: string; items: T[] }[] {
  const groups: { day: string; items: T[] }[] = []
  for (const item of items) {
    const day = dayOf(stamp(item) || 0)
    const last = groups.at(-1)
    if (last !== undefined && last.day === day) last.items.push(item)
    else groups.push({ day, items: [item] })
  }
  return groups
}

/** The month's cells, Monday first, with each day's message count. */
function monthCells(month: string, counts: ReadonlyMap<string, number>): { date: string; day: number; count: number }[] {
  const [year, index] = month.split('-').map(Number)
  if (year === undefined || index === undefined) return []
  const lead = (new Date(year, index - 1, 1).getDay() + 6) % 7
  const total = new Date(year, index, 0).getDate()
  const cells: { date: string; day: number; count: number }[] = []
  for (let blank = 0; blank < lead; blank++) cells.push({ date: '', day: 0, count: 0 })
  for (let day = 1; day <= total; day++) {
    const date = month + '-' + pad(day)
    cells.push({ date, day, count: counts.get(date) ?? 0 })
  }
  return cells
}

/** Steps the month string by whole months without leaving the calendar. */
function shiftMonth(month: string, delta: number): string {
  const [year, index] = month.split('-').map(Number)
  if (year === undefined || index === undefined) return month
  const date = new Date(year, index - 1 + delta, 1)
  return date.getFullYear() + '-' + pad(date.getMonth() + 1)
}

/**
 * The WeChat-style "查找聊天内容" dialog: one search box over the whole
 * instance's conversations, with pictures, files and a calendar reading the same
 * events the transcript does. Every row it shows can be revealed in place.
 * @param props.open - whether the dialog is on screen.
 * @param props.onClose - closes the dialog; the caller keeps the open state.
 * @param props.taskId - the conversation on screen, the default search scope.
 * @param props.taskTitle - its name, shown in the header.
 * @param props.onJump - reveals a row in the transcript (switching chats).
 * @param props.onFilePreview - opens a workspace file in the right panel.
 * @returns the portal-rendered dialog, or nothing while closed.
 */
export function ChatHistoryModal({ open, onClose, taskId, taskTitle, onJump, onFilePreview }: {
  open: boolean
  onClose: () => void
  taskId: string | undefined
  taskTitle: string
  onJump: (jump: HistoryJump) => void
  onFilePreview?: OpenFilePreview | undefined
}) {
  const [scope, setScope] = useState<Scope>('session')
  const [tab, setTab] = useState<Tab>('all')
  const [query, setQuery] = useState('')
  const [needle, setNeedle] = useState('')
  const [messages, setMessages] = useState<{ total: number; items: readonly HistoryMessage[] }>()
  const [images, setImages] = useState<{ total: number; items: readonly HistoryImage[] }>()
  const [files, setFiles] = useState<{ total: number; items: readonly HistoryFile[] }>()
  const [calendar, setCalendar] = useState<HistoryDates>()
  const [pickedMonth, setPickedMonth] = useState<string>()
  const [day, setDay] = useState<string>()
  const [lightbox, setLightbox] = useState<number>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string>()
  const searchBox = useRef<HTMLInputElement>(null)

  // A chat that has no id yet can only be searched across everything.
  const scoped = scope === 'session' && taskId !== undefined ? taskId : undefined
  const allScope = scoped === undefined

  // Typing is cheap; a query per keystroke is not.
  useEffect(() => {
    const timer = window.setTimeout(() => setNeedle(query), 200)
    return () => window.clearTimeout(timer)
  }, [query])

  useEffect(() => {
    if (!open) return
    setLightbox(undefined)
    setError(undefined)
    searchBox.current?.focus()
  }, [open])

  useEffect(() => {
    if (!open || (tab !== 'all' && tab !== 'dates')) return
    let live = true
    setLoading(true)
    const date = tab === 'dates' && day !== undefined ? day : undefined
    api.historyMessages({ task_id: scoped, q: needle, date })
      .then(result => {
        if (!live) return
        setMessages(result)
        setError(undefined)
      })
      .catch((reason: unknown) => {
        if (live) setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (live) setLoading(false)
      })
    return () => { live = false }
  }, [open, tab, scoped, needle, day])

  useEffect(() => {
    if (!open || tab !== 'dates') return
    let live = true
    api.historyDates({ task_id: scoped, month: pickedMonth })
      .then(result => { if (live) setCalendar(result) })
      .catch((reason: unknown) => {
        if (live) setError(reason instanceof Error ? reason.message : String(reason))
      })
    return () => { live = false }
  }, [open, tab, scoped, pickedMonth])

  useEffect(() => {
    if (!open || (tab !== 'images' && tab !== 'files')) return
    let live = true
    setLoading(true)
    api.historyMedia({ kind: tab === 'images' ? 'images' : 'files', task_id: scoped, q: needle })
      .then(result => {
        if (!live) return
        // One shape per tab: the server fills the fields that tab reads.
        if (tab === 'images') setImages({ total: result.total, items: result.items as unknown as readonly HistoryImage[] })
        else setFiles({ total: result.total, items: result.items as unknown as readonly HistoryFile[] })
        setError(undefined)
      })
      .catch((reason: unknown) => {
        if (live) setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (live) setLoading(false)
      })
    return () => { live = false }
  }, [open, tab, scoped, needle])

  // Esc leaves the lightbox first, then the dialog.
  useEffect(() => {
    if (!open) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      if (lightbox !== undefined) {
        event.stopPropagation()
        setLightbox(undefined)
        return
      }
      onClose()
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [open, lightbox, onClose])

  // A picture or a file has no row of its own, so it points at the message that
  // carried it: the transcript resolves the turn when the exact key is absent.
  const reveal = useCallback((row: { task_id: string; turn: number; key?: string }) => {
    onJump({ task_id: row.task_id, key: row.key ?? '', turn: row.turn })
    setLightbox(undefined)
    onClose()
  }, [onJump, onClose])

  const shownMonth = pickedMonth ?? calendar?.month
  const counts = useMemo(() => new Map((calendar?.days ?? []).map(entry => [entry.date, entry.count])), [calendar])
  const cells = useMemo(() => shownMonth === undefined ? [] : monthCells(shownMonth, counts), [shownMonth, counts])
  const openImage = lightbox !== undefined ? images?.items[lightbox] : undefined
  const step = useCallback((delta: number) => {
    const total = images?.items.length ?? 0
    if (total === 0 || lightbox === undefined) return
    setLightbox((lightbox + delta + total) % total)
  }, [images, lightbox])

  if (!open) return null

  const messageRows = (rows: readonly HistoryMessage[]) => groupByDay(rows, row => row.timestamp).map(group => (
    <section key={group.day} className={css.group}>
      <h3 className={css.groupHead}>{dayLabel(group.day)}</h3>
      {group.items.map(row => (
        <button type="button" key={row.task_id + ':' + row.key} className={css.messageRow}
          onClick={() => reveal(row)} title="在对话中定位这条消息">
          <span className={clsx(css.role, row.role === 'assistant' ? css.roleAi : css.roleUser)}>
            {row.role === 'assistant' ? 'AI' : '我'}
          </span>
          <span className={css.messageBody}>
            {allScope && row.task_title !== '' && <span className={css.taskPill}>{row.task_title}</span>}
            <span className={css.messageText}>
              <Highlighted text={row.snippet} needle={needle} />
            </span>
          </span>
          <span className={css.messageTime}>{clockOf(row.timestamp)}</span>
        </button>
      ))}
    </section>
  ))

  return createPortal(
    <div className={css.overlay} role="dialog" aria-modal="true" aria-label="查找会话记录与素材" data-chat-history>
      <div className={css.mask} onClick={onClose} />
      <div className={css.panel}>
        <header className={css.head}>
          <div className={css.headTitle}>
            <span className={css.title}>{scoped === undefined ? '全部会话记录' : taskTitle}</span>
            <span className={css.scope} role="tablist" aria-label="搜索范围">
              <button type="button" role="tab" aria-selected={!allScope} disabled={taskId === undefined}
                data-history-scope="session"
                className={clsx(css.scopeBtn, !allScope && css.scopeOn)} onClick={() => setScope('session')}>
                当前会话
              </button>
              <button type="button" role="tab" aria-selected={allScope}
                data-history-scope="all"
                className={clsx(css.scopeBtn, allScope && css.scopeOn)} onClick={() => setScope('all')}>
                全部会话
              </button>
            </span>
          </div>
          <button type="button" className={css.close} onClick={onClose} aria-label="关闭" title="关闭（Esc）">
            <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round">
              <path d="m4 4 8 8M12 4l-8 8" />
            </svg>
          </button>
        </header>

        <div className={css.searchRow}>
          <svg className={css.searchIcon} width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round">
            <circle cx="7" cy="7" r="4.4" />
            <path d="m10.4 10.4 3 3" />
          </svg>
          <input ref={searchBox} className={css.search} value={query} placeholder="搜索聊天记录、图片、文件…"
            onChange={event => setQuery(event.target.value)} aria-label="搜索聊天记录与素材" />
          {query !== '' && (
            <button type="button" className={css.clear} onClick={() => { setQuery(''); searchBox.current?.focus() }} aria-label="清空搜索">
              <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round">
                <path d="m4 4 8 8M12 4l-8 8" />
              </svg>
            </button>
          )}
        </div>

        <div className={css.tabs} role="tablist" aria-label="内容分类">
          {TABS.map(entry => {
            const count = entry.id === 'all' ? messages?.total
              : entry.id === 'images' ? images?.total
                : entry.id === 'files' ? files?.total
                  : calendar?.days.reduce((sum, item) => sum + item.count, 0)
            return (
              <button type="button" key={entry.id} role="tab" aria-selected={tab === entry.id}
                data-history-tab={entry.id}
                className={clsx(css.tab, tab === entry.id && css.tabOn)}
                onClick={() => { setTab(entry.id); if (entry.id !== 'dates') setDay(undefined) }}>
                {entry.label}
                {count !== undefined && count > 0 && <span className={css.tabCount}>{count}</span>}
              </button>
            )
          })}
        </div>

        <div className={css.body} data-loading={loading ? '' : undefined}>
          {error !== undefined && <p className={css.note} role="status">读取失败：{error}</p>}
          {error === undefined && loading && messages === undefined && images === undefined && files === undefined && (
            <p className={css.note} role="status">正在读取…</p>
          )}

          {(tab === 'all' || tab === 'dates') && (
            <>
              {tab === 'dates' && shownMonth !== undefined && (
                <div className={css.calendar}>
                  <div className={css.calendarHead}>
                    <button type="button" className={css.calendarStep} aria-label="上个月"
                      disabled={calendar?.months[0] === shownMonth}
                      onClick={() => { setPickedMonth(shiftMonth(shownMonth, -1)); setDay(undefined) }}>‹</button>
                    <span className={css.calendarMonth}>{shownMonth}</span>
                    <button type="button" className={css.calendarStep} aria-label="下个月"
                      disabled={calendar?.months.at(-1) === shownMonth}
                      onClick={() => { setPickedMonth(shiftMonth(shownMonth, 1)); setDay(undefined) }}>›</button>
                  </div>
                  <div className={css.calendarGrid}>
                    {WEEKDAYS.map(name => <span key={name} className={css.weekday}>{name}</span>)}
                    {cells.map((cell, index) => cell.day === 0
                      ? <span key={'blank' + index} />
                      : <button type="button" key={cell.date} className={clsx(css.day, cell.count > 0 && css.dayBusy, day === cell.date && css.dayOn)}
                        disabled={cell.count === 0} title={cell.date + ' · ' + cell.count + ' 条消息'}
                        onClick={() => setDay(day === cell.date ? undefined : cell.date)}>
                        {cell.day}
                        {cell.count > 0 && <span className={css.dayDot} />}
                      </button>)}
                  </div>
                  {day === undefined && <p className={css.note}>选择一天，查看当天的聊天记录</p>}
                </div>
              )}
              {tab === 'dates' && day === undefined
                ? null
                : messages === undefined
                  ? null
                  : messages.items.length === 0
                    ? <p className={css.note}>{needle === '' ? '这里还没有聊天记录' : '没有找到包含「' + needle + '」的消息'}</p>
                    : messageRows(messages.items)}
            </>
          )}

          {tab === 'images' && (
            images === undefined ? null
              : images.items.length === 0
                ? <p className={css.note}>{needle === '' ? '这里还没有图片' : '没有找到匹配的图片'}</p>
                : groupByDay(images.items, item => item.timestamp).map(group => (
                  <section key={group.day} className={css.group}>
                    <h3 className={css.groupHead}>{dayLabel(group.day)}</h3>
                    <div className={css.grid}>
                      {group.items.map(item => {
                        const index = images.items.indexOf(item)
                        return (
                          <button type="button" key={item.task_id + ':' + index} className={css.thumb}
                            onClick={() => setLightbox(index)} title={item.label}>
                            <img src={item.thumb_url} alt={item.label} loading="lazy" />
                            <span className={css.thumbBar}>
                              <span className={css.thumbLabel}>{item.source === 'upload' ? '上传' : item.label}</span>
                              <span className={css.locate} role="button" tabIndex={0}
                                onClick={event => { event.stopPropagation(); reveal(item) }}
                                onKeyDown={event => { if (event.key === 'Enter') { event.stopPropagation(); reveal(item) } }}>
                                定位
                              </span>
                            </span>
                          </button>
                        )
                      })}
                    </div>
                  </section>
                ))
          )}

          {tab === 'files' && (
            files === undefined ? null
              : files.items.length === 0
                ? <p className={css.note}>{needle === '' ? '这里还没有文件' : '没有找到匹配的文件'}</p>
                : groupByDay(files.items, item => item.timestamp).map(group => (
                  <section key={group.day} className={css.group}>
                    <h3 className={css.groupHead}>{dayLabel(group.day)}</h3>
                    {group.items.map(item => {
                      const sameTask = item.task_id === taskId
                      const open = () => {
                        if (item.source === 'workspace_change') {
                          if (sameTask) onFilePreview?.(item.path)
                          else reveal(item)
                          return
                        }
                        if (sameTask && onFilePreview !== undefined) onFilePreview(item.name, { url: item.download_url })
                        else reveal(item)
                      }
                      return (
                        <div key={item.task_id + ':' + group.day + ':' + item.path + ':' + item.seq}
                          className={clsx(css.fileRow, item.removed === true && css.fileGone)}>
                          <button type="button" className={css.fileMain} onClick={open} title={item.path}>
                            <span className={css.fileIcon} data-kind={item.source}>
                              {item.source === 'workspace_change' ? '⌘' : '＊'}
                            </span>
                            <span className={css.fileText}>
                              <span className={css.fileName}>
                                {item.name}
                                {allScope && item.task_title !== '' && <span className={css.taskPill}>{item.task_title}</span>}
                              </span>
                              <span className={css.filePath}>
                                {item.source === 'workspace_change'
                                  ? item.path + (item.removed === true ? ' · 已删除' : '')
                                  : (item.media_type ?? '') + (sizeLabel(item.bytes) === '' ? '' : ' · ' + sizeLabel(item.bytes))}
                              </span>
                            </span>
                            {item.source === 'workspace_change' && (item.added !== 0 || item.deleted !== 0) && (
                              <span className={css.stats}>
                                <span className={css.added}>+{item.added ?? 0}</span>
                                <span className={css.deleted}>−{item.deleted ?? 0}</span>
                              </span>
                            )}
                          </button>
                          <span className={css.fileTime}>{clockOf(item.timestamp)}</span>
                          <button type="button" className={css.locateBtn} onClick={() => reveal(item)} title="定位到原消息">定位</button>
                          <a className={css.locateBtn} href={item.download_url} download aria-label="下载" title="下载">↓</a>
                        </div>
                      )
                    })}
                  </section>
                ))
          )}
        </div>

        {openImage !== undefined && images !== undefined && (
          <div className={css.lightbox} role="dialog" aria-modal="true" aria-label="图片预览">
            <div className={css.lightboxMask} onClick={() => setLightbox(undefined)} />
            <div className={css.lightboxPanel}>
              <img src={openImage.url} alt={openImage.label} />
              <div className={css.lightboxBar}>
                <span className={css.lightboxLabel}>{openImage.label}</span>
                <span className={css.lightboxMeta}>
                  {openImage.width !== null && openImage.width !== undefined ? openImage.width + ' × ' + openImage.height + ' · ' : ''}
                  {stampLabel(openImage.timestamp)}
                </span>
                <button type="button" className={css.lightboxAction} onClick={() => reveal(openImage)}>定位到原消息</button>
                <button type="button" className={css.lightboxAction} onClick={() => setLightbox(undefined)}>关闭</button>
              </div>
              {images.items.length > 1 && (
                <>
                  <button type="button" className={clsx(css.lightboxStep, css.lightboxPrev)} aria-label="上一张" onClick={() => step(-1)}>‹</button>
                  <button type="button" className={clsx(css.lightboxStep, css.lightboxNext)} aria-label="下一张" onClick={() => step(1)}>›</button>
                </>
              )}
            </div>
          </div>
        )}
      </div>
    </div>,
    document.body,
  )
}

/* The flash the dialog leaves behind: the row it revealed, fading out. It lives
   here because the dialog is what asks for it, and the animation is never used
   anywhere else. */
export const flashCss = css
