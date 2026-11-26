import { useEffect, useRef, useState, type FormEvent } from 'react'
import css from './AppDialog.module.css'

export interface ConfirmOptions {
  title?: string
  message: string
  confirmText?: string
  cancelText?: string
  danger?: boolean
}

export interface PromptOptions {
  title: string
  message?: string
  defaultValue?: string
  placeholder?: string
  confirmText?: string
  cancelText?: string
}

type Request =
  | { kind: 'confirm'; options: ConfirmOptions; resolve: (value: boolean) => void }
  | { kind: 'prompt'; options: PromptOptions; resolve: (value: string | null) => void }

type Listener = (request: Request) => void
let activeListener: Listener | null = null
const pendingQueue: Request[] = []

function enqueue(request: Request) {
  if (activeListener) {
    activeListener(request)
  } else {
    pendingQueue.push(request)
  }
}

/**
 * In-app confirmation modal (replaces native `window.confirm`, which spawns a
 * GTK3 `GtkMessageDialog` under WebKitGTK and can crash on Ubuntu 26.04 `libglycin-2`).
 */
export function appConfirm(options: ConfirmOptions | string): Promise<boolean> {
  const normalized: ConfirmOptions = typeof options === 'string' ? { message: options } : options
  return new Promise<boolean>(resolve => {
    enqueue({ kind: 'confirm', options: normalized, resolve })
  })
}

/**
 * In-app text prompt modal (replaces native `window.prompt`).
 */
export function appPrompt(options: PromptOptions): Promise<string | null> {
  return new Promise<string | null>(resolve => {
    enqueue({ kind: 'prompt', options, resolve })
  })
}

/** Root modal host mounted once in `App.tsx`. Uses HTML `<dialog>` top-layer (pure DOM). */
export function AppDialogHost() {
  const [queue, setQueue] = useState<Request[]>([])
  const [value, setValue] = useState('')
  const dialogRef = useRef<HTMLDialogElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const confirmRef = useRef<HTMLButtonElement>(null)
  const current = queue[0]

  useEffect(() => {
    const push: Listener = request => {
      setQueue(prev => [...prev, request])
    }
    activeListener = push
    if (pendingQueue.length > 0) {
      const drained = pendingQueue.splice(0, pendingQueue.length)
      setQueue(prev => [...prev, ...drained])
    }
    return () => {
      if (activeListener === push) activeListener = null
    }
  }, [])

  useEffect(() => {
    const dialog = dialogRef.current
    if (!current) {
      if (dialog?.open) dialog.close()
      return
    }
    if (current.kind === 'prompt') {
      setValue(current.options.defaultValue ?? '')
    }
    if (dialog && !dialog.open) {
      dialog.showModal()
    }
    const frame = requestAnimationFrame(() => {
      if (current.kind === 'prompt') {
        inputRef.current?.focus()
        inputRef.current?.select()
      } else {
        confirmRef.current?.focus()
      }
    })
    return () => cancelAnimationFrame(frame)
  }, [current])

  function dismiss() {
    if (!current) return
    if (current.kind === 'confirm') current.resolve(false)
    else current.resolve(null)
    setQueue(prev => prev.slice(1))
  }

  function submit(event?: FormEvent) {
    event?.preventDefault()
    if (!current) return
    if (current.kind === 'confirm') current.resolve(true)
    else current.resolve(value)
    setQueue(prev => prev.slice(1))
  }

  const title = current
    ? current.kind === 'confirm'
      ? current.options.title ?? '确认操作'
      : current.options.title
    : ''

  return (
    <dialog
      ref={dialogRef}
      className={css.dialog}
      aria-labelledby="app-dialog-title"
      onCancel={event => {
        event.preventDefault()
        dismiss()
      }}
      onClick={event => {
        if (event.target === dialogRef.current) dismiss()
      }}
    >
      {current && (
        <form onSubmit={submit} className={css.form}>
          <header className={css.header}>
            <span
              className={css.ringIcon}
              data-tone={current.kind === 'confirm' && current.options.danger ? 'danger' : 'brand'}
              aria-hidden="true"
            >
              {current.kind === 'confirm' && current.options.danger ? (
                <svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
                  <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
                  <line x1="12" y1="9" x2="12" y2="13" />
                  <line x1="12" y1="17" x2="12.01" y2="17" />
                </svg>
              ) : (
                <svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
                  <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
                  <polyline points="9 12 11 14 15 10" />
                </svg>
              )}
            </span>
            <button type="button" className={css.close} aria-label="关闭确认弹窗" onClick={dismiss}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </header>
          <div className={css.body}>
            <h2 id="app-dialog-title" className={css.title}>{title}</h2>
            {current.options.message && <p className={css.message}>{current.options.message}</p>}
            {current.kind === 'prompt' && (
              <input
                ref={inputRef}
                className={css.input}
                type="text"
                value={value}
                placeholder={current.options.placeholder}
                onChange={event => setValue(event.target.value)}
              />
            )}
          </div>
          <footer className={css.actions}>
            <button type="button" className={css.cancel} onClick={dismiss}>
              {current.options.cancelText ?? '取消'}
            </button>
            <button
              ref={confirmRef}
              type="submit"
              className={css.confirm}
              data-danger={current.kind === 'confirm' && !!current.options.danger}
            >
              {current.options.confirmText ?? '确认'}
            </button>
          </footer>
        </form>
      )}
    </dialog>
  )
}
