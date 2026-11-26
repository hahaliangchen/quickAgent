import type React from 'react'

export interface HorizontalResizeOptions {
  event: React.PointerEvent<HTMLElement>
  startWidth: number
  /** +1 when dragging right widens the panel (left sidebar); -1 when dragging left widens it (right dock/panel). */
  direction: 1 | -1
  min: number
  max: number | (() => number)
  onWidth: (width: number) => void
  onCommit?: (width: number) => void
}

/**
 * Smooth, iframe-safe horizontal drag resize for split panes.
 * Places a full-viewport capture shield above embedded iframes (Office/DB/PDF),
 * captures the pointer, coalesces React state updates per animation frame, and
 * guards against lost pointerup events via `buttons === 0`, `pointercancel`,
 * `lostpointercapture`, and `window.blur`.
 */
export function startHorizontalResize({
  event,
  startWidth,
  direction,
  min,
  max,
  onWidth,
  onCommit,
}: HorizontalResizeOptions): void {
  if (event.button !== 0) return
  event.preventDefault()

  const handle = event.currentTarget
  const pointerId = event.pointerId
  const startX = event.clientX
  const clamp = (raw: number) => {
    const hi = typeof max === 'function' ? max() : max
    return Math.round(Math.min(hi, Math.max(min, raw)))
  }

  let current = clamp(startWidth)
  let pending: number | null = null
  let rafId = 0
  let finished = false

  // Transparent top-most overlay blocks iframes from stealing pointermove/pointerup
  // and keeps the col-resize cursor steady across the entire viewport.
  const shield = document.createElement('div')
  shield.style.position = 'fixed'
  shield.style.inset = '0'
  shield.style.zIndex = '2147483647'
  shield.style.cursor = 'col-resize'
  shield.style.userSelect = 'none'
  shield.style.touchAction = 'none'
  shield.style.background = 'transparent'
  document.body.appendChild(shield)

  const prevUserSelect = document.body.style.userSelect
  document.body.style.userSelect = 'none'

  try {
    handle.setPointerCapture(pointerId)
  } catch {
    // Some synthetic/headless pointers cannot be captured; shield + window listeners still handle them.
  }

  const flush = () => {
    rafId = 0
    if (pending !== null && !finished) {
      const value = pending
      pending = null
      onWidth(value)
    }
  }

  const finish = () => {
    if (finished) return
    finished = true
    if (rafId) {
      cancelAnimationFrame(rafId)
      rafId = 0
    }
    if (pending !== null) {
      current = pending
      pending = null
      onWidth(current)
    }
    window.removeEventListener('pointermove', onMove, true)
    window.removeEventListener('pointerup', finish, true)
    window.removeEventListener('pointercancel', finish, true)
    window.removeEventListener('blur', finish)
    handle.removeEventListener('lostpointercapture', finish)
    shield.removeEventListener('pointerup', finish)
    shield.removeEventListener('mouseup', finish)
    shield.remove()
    document.body.style.userSelect = prevUserSelect
    try {
      if (handle.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId)
    } catch {
      // Ignore if capture was already released.
    }
    onCommit?.(current)
  }

  const onMove = (moveEvt: PointerEvent) => {
    if (finished) return
    // If the mouse button was released outside the window or during an iframe/WM transition,
    // immediately end the drag so the handle never sticks.
    if (moveEvt.pointerType === 'mouse' && (moveEvt.buttons & 1) === 0) {
      finish()
      return
    }
    const next = clamp(startWidth + direction * (moveEvt.clientX - startX))
    if (next === current && pending === null) return
    current = next
    pending = next
    if (!rafId) rafId = requestAnimationFrame(flush)
  }

  window.addEventListener('pointermove', onMove, true)
  window.addEventListener('pointerup', finish, true)
  window.addEventListener('pointercancel', finish, true)
  window.addEventListener('blur', finish)
  handle.addEventListener('lostpointercapture', finish)
  shield.addEventListener('pointerup', finish)
  shield.addEventListener('mouseup', finish)
}
