import { useCallback, useLayoutEffect, useRef, useState, type RefObject } from 'react'
import clsx from 'clsx'
import css from './ScrollToBottom.module.css'

/** Distance from the newest line, in pixels, that still counts as "at the bottom". */
const DEFAULT_THRESHOLD = 72

/**
 * Which element actually scrolls under a host frame. The conversation scrollport
 * owns the overflow in the main conversation; a dock's frame leaves ChatView's own
 * scroller in charge. The host's own box is the fallback, so a frame that scrolls
 * itself needs no marker at all.
 * @param host - the frame the host handed over (the transcript's outer box).
 * @returns the element whose scrollTop walks the transcript.
 */
export function transcriptScroller(host: HTMLElement): HTMLElement {
  const inner = host.querySelector<HTMLElement>('[data-chat-scroll]')
  if (inner !== null && inner !== host) {
    const overflow = getComputedStyle(inner).overflowY
    if ((overflow === 'auto' || overflow === 'scroll') && inner.scrollHeight - inner.clientHeight > 1) return inner
  }
  return host
}

/**
 * Tells whether a host's transcript still has content below the fold, and puts it
 * back on the newest line. A scroll listener alone is not enough: while a turn
 * streams the content grows under an unmoved scrollbar, so the frame, the scroller
 * and their children are observed too and the control appears without a scroll.
 * @param scroller - the transcript's outer box, not necessarily the element that scrolls.
 * @param active - false while the transcript is unmounted (a closed dock, a hidden
 *   panel), so listeners rebind when it appears instead of binding to a null ref once.
 * @param threshold - how close to the end still counts as following along.
 * @returns `away` for the control's visibility, `jump` to rejoin the tail, and
 *   `following` for hosts that grow the content themselves.
 */
export function useScrollToBottom(scroller: RefObject<HTMLElement | null>, active = true, threshold = DEFAULT_THRESHOLD) {
  const [away, setAway] = useState(false)
  /** Read by hosts that grow the content themselves: only carry along a reader at the tail. */
  const following = useRef(true)
  useLayoutEffect(() => {
    const frame = scroller.current
    if (frame === null || !active) return
    // The narrow survives into the observer callbacks through this binding.
    const host: HTMLElement = frame
    let box = transcriptScroller(host)
    const observer = new ResizeObserver(() => measure())
    const watch = () => {
      observer.disconnect()
      observer.observe(host)
      for (const node of [box, ...Array.from(box.children)]) observer.observe(node)
    }
    function measure() {
      // A frame that mounts empty only starts overflowing later, so the scroller is
      // re-resolved instead of being fixed to whatever was there on the first paint.
      const next = transcriptScroller(host)
      if (next !== box) {
        box = next
        watch()
      }
      const distance = box.scrollHeight - box.scrollTop - box.clientHeight
      following.current = distance <= threshold
      setAway(!following.current)
    }
    watch()
    measure()
    // Capture reaches the transcript's own scroller even when the frame never scrolls.
    host.addEventListener('scroll', measure, { passive: true, capture: true })
    window.addEventListener('resize', measure)
    return () => {
      host.removeEventListener('scroll', measure, true)
      window.removeEventListener('resize', measure)
      observer.disconnect()
    }
  }, [scroller, active, threshold])
  const jump = useCallback(() => {
    const host = scroller.current
    if (host === null) return
    const box = transcriptScroller(host)
    following.current = true
    setAway(false)
    box.scrollTo({ top: box.scrollHeight, behavior: 'smooth' })
  }, [scroller])
  return { away, jump, following }
}

/**
 * The floating "back to bottom" control. It belongs to a positioned, non-scrolling
 * ancestor next to the transcript, so it never travels with the text it undoes.
 * @param props.show - visible only while older content is scrolled out of view.
 * @param props.onJump - scroll the transcript back to its newest line.
 * @param props.className - placement overrides from the host's own stylesheet.
 * @param props.label - accessible name, also the tooltip.
 * @returns a circular icon button that fades in place.
 */
export function ScrollToBottom({ show, onJump, className, label = '回到底部' }: {
  show: boolean
  onJump: () => void
  className?: string | undefined
  label?: string | undefined
}) {
  return (
    <button
      type="button"
      className={clsx(css.jump, show && css.shown, className)}
      aria-label={label}
      aria-hidden={!show}
      title={label}
      tabIndex={show ? 0 : -1}
      data-scroll-to-bottom={show ? '' : undefined}
      onClick={onJump}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M8 3.4v9.2" />
        <path d="m4.6 9.6 3.4 3.4 3.4-3.4" />
      </svg>
    </button>
  )
}
