import { useCallback, useState, useSyncExternalStore } from 'react'
import clsx from 'clsx'
import {
  addAttachments, attachmentsOf, clearAttachments, removeAttachment, subscribeAttachments,
  type DraftAttachment,
} from '../composerAttachments.ts'
import { RichFileIcon, fileVisualMeta, formatFileSize } from './RichFileIcon.tsx'
import css from './Attachments.module.css'

export interface AttachmentRef { attachmentId: string; name: string; bytes: number; mediaType?: string; width?: number; height?: number }
export interface AttachmentSubmission { files: readonly File[]; signal: AbortSignal; requestId: string; onProgress: (label: string) => void }

/**
 * The files a session has picked but not sent. They belong to the session, not
 * to the box that shows them: leaving to read another chat (or another page)
 * and coming back finds the picture still attached, and going elsewhere starts
 * clean. Previews are released when a file is removed or the message is sent.
 */
export function useAttachmentDraft(key: string, extensions?: readonly string[]) {
  const items = useSyncExternalStore(subscribeAttachments, () => attachmentsOf(key), () => attachmentsOf(key))
  const [error, setError] = useState('')
  const add = useCallback((files: Iterable<File>) => { setError(addAttachments(key, files, extensions)) }, [key, extensions])
  const remove = useCallback((id: string) => { removeAttachment(key, id) }, [key])
  /** Forget a session's files; a send that ends after a switch forgets the sender's. */
  const clear = useCallback((from: string = key) => { clearAttachments(from); if (from === key) setError('') }, [key])
  return { items, add, remove, clear, error }
}

export function AttachmentRail({
  items,
  onRemove,
  disabled = false,
}: {
  items: readonly DraftAttachment[]
  onRemove: (id: string) => void
  disabled?: boolean
}) {
  if (items.length === 0) return null
  return (
    <div className={css.rail} aria-label="待发送附件">
      {items.map(item => {
        if (item.preview) {
          return (
            <div key={item.id} className={css.imageChip} title={`${item.file.name} · ${formatFileSize(item.file.size)}`}>
              <img src={item.preview} alt={item.file.name} className={css.imageThumb} />
              <button
                type="button"
                className={clsx(css.removeBtn, css.imageRemoveBtn)}
                aria-label={`移除 ${item.file.name}`}
                title="移除"
                onClick={() => onRemove(item.id)}
                disabled={disabled}
              >
                ×
              </button>
            </div>
          )
        }
        const meta = fileVisualMeta(item.file.name)
        const sizeLabel = formatFileSize(item.file.size)
        return (
          <div key={item.id} className={css.fileChip} title={item.file.name}>
            <span className={css.iconTile} data-tone={meta.tone}>
              <RichFileIcon path={item.file.name} size={22} />
            </span>
            <div className={css.meta}>
              <span className={css.name}>{item.file.name}</span>
              <span className={css.sub}>{meta.label}{sizeLabel ? ` · ${sizeLabel}` : ''}</span>
            </div>
            <button
              type="button"
              className={css.removeBtn}
              aria-label={`移除 ${item.file.name}`}
              title="移除"
              onClick={() => onRemove(item.id)}
              disabled={disabled}
            >
              ×
            </button>
          </div>
        )
      })}
    </div>
  )
}
