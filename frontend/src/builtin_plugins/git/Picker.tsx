import { Fragment, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import type { KeyboardEvent, ReactNode } from 'react'
import clsx from 'clsx'
import { Icon } from '../../components/WorkbenchIcon.tsx'
import css from './style.module.css'

/** One choice in a {@link Picker} menu. */
export interface PickerOption {
  /** Value handed back on selection; the empty string is a real choice ('all'). */
  value: string
  /** Visible row text. */
  label: string
  /** Right-aligned muted text (a time, a count) that stays out of the way. */
  hint?: string
  /** Draw a hairline under this row: it ends a group, like the 'all' reset row. */
  divider?: boolean
}

/** Props for {@link Picker}. */
export interface PickerProps {
  /** Leading glyph on the trigger. */
  icon: ReactNode
  /** The chosen value, matched against `options`. */
  value: string
  /** Every choice, in display order. */
  options: readonly PickerOption[]
  /** Selection callback. */
  onChange: (value: string) => void
  /** Trigger text when `value` matches no option. */
  placeholder?: string
  /** Accessible name for the trigger and the menu. */
  label: string
  /** Monospaced values (branch names, hashes). */
  mono?: boolean
  /** Filter box once the list is longer than this many rows (default 8). */
  searchAfter?: number
}

const MENU_WIDTH = 216, MENU_GAP = 6, MENU_MARGIN = 8, MENU_MAX_HEIGHT = 340

/**
 * A filter chip that opens a menu card instead of the browser's `<select>`.
 * The native control draws its own inset field and arrow inside whatever
 * border the label already has, so it reads as two nested boxes; here the
 * trigger is one control and the list is a portal card that cannot be clipped
 * by the panel around it (`overflow: hidden` on the graph body) and that
 * flips above the trigger when the window bottom is near.
 * @param props.icon - leading glyph.
 * @param props.value - the chosen value.
 * @param props.options - every choice, in display order.
 * @param props.onChange - selection callback.
 * @param props.placeholder - trigger text when nothing matches.
 * @param props.label - accessible name.
 * @param props.mono - monospaced values.
 * @param props.searchAfter - row count that turns on the filter box.
 * @returns the trigger plus, while open, its portaled menu.
 */
export function Picker({ icon, value, options, onChange, placeholder, label, mono, searchAfter = 8 }: PickerProps) {
  const [open, setOpen] = useState(false)
  const [needle, setNeedle] = useState('')
  const [active, setActive] = useState(-1)
  const [box, setBox] = useState<{ left: number; top: number; width: number; maxHeight: number }>()
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const filter = useRef<HTMLInputElement>(null)
  const menuId = useId()

  const searchable = options.length > searchAfter
  const visible = useMemo(() => {
    const needleLowerCase = needle.trim().toLowerCase()
    return !searchable || !needleLowerCase ? options : options.filter(option => option.label.toLowerCase().includes(needleLowerCase))
  }, [options, searchable, needle])
  const picked = options.find(option => option.value === value)
  const shown = picked?.label ?? placeholder ?? ''

  // Placement only becomes real once the card has been measured, so its first
  // paint is hidden rather than flashing at the viewport's top-left corner.
  useLayoutEffect(() => {
    if (!open) { setBox(undefined); return }
    const place = () => {
      const anchor = trigger.current, card = menu.current
      if (!anchor || !card) return
      const rect = anchor.getBoundingClientRect()
      const width = Math.max(rect.width, MENU_WIDTH)
      const natural = Math.min(card.scrollHeight, MENU_MAX_HEIGHT)
      const below = window.innerHeight - rect.bottom - MENU_GAP - MENU_MARGIN
      const above = rect.top - MENU_GAP - MENU_MARGIN
      const up = below < Math.min(natural, 180) && above > below
      const maxHeight = Math.max(120, Math.min(MENU_MAX_HEIGHT, Math.floor(up ? above : below)))
      const height = Math.min(natural, maxHeight)
      setBox({
        left: Math.max(MENU_MARGIN, Math.min(rect.left, window.innerWidth - width - MENU_MARGIN)),
        top: Math.round(up ? rect.top - MENU_GAP - height : rect.bottom + MENU_GAP),
        width, maxHeight,
      })
    }
    place()
    // Scrolling inside the card must not move the card; anything else may.
    const follow = (event: Event) => { if (!menu.current?.contains(event.target as Node)) place() }
    window.addEventListener('resize', follow)
    window.addEventListener('scroll', follow, true)
    return () => { window.removeEventListener('resize', follow); window.removeEventListener('scroll', follow, true) }
  }, [open, visible.length, needle, searchable])

  useEffect(() => {
    if (!open) return
    if (searchable) filter.current?.focus()
    const down = (event: PointerEvent) => {
      const target = event.target as Node
      if (trigger.current?.contains(target) || menu.current?.contains(target)) return
      setOpen(false)
    }
    document.addEventListener('pointerdown', down, true)
    return () => document.removeEventListener('pointerdown', down, true)
  }, [open, searchable])

  function reveal(index: number) {
    setActive(index)
    const row = list.current?.querySelectorAll('[role="option"]')[index]
    row?.scrollIntoView({ block: 'nearest' })
  }

  function choose(next: string) {
    setOpen(false); setNeedle(''); setActive(-1); onChange(next)
    trigger.current?.focus()
  }

  function toggle() {
    if (open) { setOpen(false); return }
    setNeedle('')
    setActive(options.findIndex(option => option.value === value))
    setOpen(true)
  }

  function keys(event: KeyboardEvent) {
    if (event.key === 'Escape') { event.preventDefault(); setOpen(false); setNeedle(''); trigger.current?.focus(); return }
    if (!open) return
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      const delta = event.key === 'ArrowDown' ? 1 : -1
      if (!visible.length) return
      reveal(active < 0 ? (delta > 0 ? 0 : visible.length - 1) : (active + delta + visible.length) % visible.length)
      return
    }
    if (event.key === 'Home' || event.key === 'End') { event.preventDefault(); if (visible.length) reveal(event.key === 'Home' ? 0 : visible.length - 1); return }
    if (event.key === 'Enter' || (event.key === ' ' && !searchable)) {
      if (!open) { event.preventDefault(); toggle(); return }
      const option = visible[active]
      if (option) { event.preventDefault(); choose(option.value) }
    }
  }

  return <>
    <button ref={trigger} type="button" className={css.picker} aria-haspopup="listbox" aria-expanded={open}
      aria-controls={open ? menuId : undefined} aria-label={`${label}：${shown}`} title={shown}
      onClick={toggle} onKeyDown={keys}>
      <span className={css.pickerIcon}>{icon}</span>
      <span className={clsx(css.pickerValue, mono && css.pickerMono, !picked && css.pickerPlaceholder)}>{shown}</span>
      <span className={css.pickerChevron}><Icon name="chevronDown" size={14} /></span>
    </button>
    {open && createPortal(<div ref={menu} className={css.menu} onKeyDown={keys}
      style={{ left: box?.left ?? 0, top: box?.top ?? 0, width: box?.width ?? MENU_WIDTH, maxHeight: box?.maxHeight ?? MENU_MAX_HEIGHT, visibility: box ? 'visible' : 'hidden' }}>
      {searchable && <label className={css.menuSearch}><Icon name="search" size={14} />
        <input ref={filter} value={needle} placeholder={`筛选${label}…`} aria-label={`筛选${label}`}
          onChange={event => { setNeedle(event.target.value); setActive(-1) }} />
      </label>}
      <div ref={list} className={css.menuList} id={menuId} role="listbox" aria-label={label}>
        {visible.length === 0 && <p className={css.menuEmpty}>没有匹配的项。</p>}
        {/* The check leads, as in a system menu, so the hints keep one right edge. */}
        {visible.map((option, index) => <Fragment key={option.value || 'all'}>
          <button type="button" role="option" aria-selected={option.value === value} title={option.label}
            className={clsx(css.option, index === active && css.optionActive, option.value === value && css.optionSelected)}
            onPointerMove={() => { if (index !== active) setActive(index) }} onClick={() => choose(option.value)}>
            <span className={css.optionCheck}>{option.value === value && <Icon name="check" size={14} />}</span>
            <span className={clsx(css.optionLabel, mono && css.pickerMono)}>{option.label}</span>
            {option.hint && <span className={css.optionHint}>{option.hint}</span>}
          </button>
          {option.divider && <div className={css.menuSeparator} role="separator" />}
        </Fragment>)}
      </div>
    </div>, document.body)}
  </>
}
