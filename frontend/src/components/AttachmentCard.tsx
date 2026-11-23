import { useCallback, useState } from 'react'
import { FileTypeIcon, ImageLightbox, classifyFileType } from '@deepseek-ai/dsh-client-ui-primitives'
import clsx from 'clsx'
import { t } from '../i18n.ts'
import { previewKind, type OpenFilePreview } from '../filePreview.ts'
import css from './AttachmentCard.module.css'

/** One file a user turn was sent with: the image itself, or just its name. */
export interface UserAttachment {
  readonly name: string
  readonly url: string
  readonly image: boolean
}

/** Characters that fit the footer at its 13px size; CJK counts double. */
const IMAGE_NAME_BUDGET = 26
const FILE_NAME_BUDGET = 34

const wide = (char: string): number => (char.codePointAt(0) ?? 0) > 0x2e80 ? 2 : 1

/** Room for the ending first, then as much of the start as the rest allows. */
function middleShorten(name: string, budget: number): string {
  const chars = [...name]
  if (chars.reduce((total, char) => total + wide(char), 0) <= budget) return name
  let tail = ''
  let used = 1 // the ellipsis itself
  for (let index = chars.length - 1; index >= 0; index -= 1) {
    const char = chars[index] as string
    const width = wide(char)
    if (used + width > budget * 0.6) break
    tail = char + tail
    used += width
  }
  let head = ''
  for (const char of chars) {
    const width = wide(char)
    if (used + width > budget) break
    head += char
    used += width
  }
  return `${head.trimEnd()}…${tail}`
}

/**
 * The files a user message carried, shown above its bubble. An image opens in
 * the lightbox over the page, the way a picture in a reply does — nothing is
 * sent to a new tab; any other file stays a download link. Each row leads with
 * the type's own glyph and the name, shortened in the middle so a screenshot's
 * date and extension survive.
 */
export function AttachmentCards({ items, onFilePreview }: { items: readonly UserAttachment[]; onFilePreview?: OpenFilePreview | undefined }) {
  const [open, setOpen] = useState<UserAttachment | undefined>(undefined)
  const close = useCallback(() => { setOpen(undefined) }, [])
  return (
    <div className={css.attachments}>
      {items.map((file, index) => {
        const footer = (
          <span className={css.foot}>
            <FileTypeIcon
              path={file.name}
              size={14}
              className={clsx(css.glyph, classifyFileType(file.name) === 'other' && css.glyphPlain)}
            />
            <span className={css.name}>{middleShorten(file.name, file.image ? IMAGE_NAME_BUDGET : FILE_NAME_BUDGET)}</span>
          </span>
        )
        if (!file.image) {
          if (onFilePreview && previewKind(file.name) !== 'unsupported') return (
            <div key={index} className={css.fileActions}>
              <button type="button" className={css.card} data-kind="file" title={file.name}
                onClick={() => onFilePreview(file.name, { url: file.url })}>{footer}</button>
              <a className={css.download} href={file.url} download={file.name} aria-label={'下载 ' + file.name} title="下载">↓</a>
            </div>
          )
          return (
            <a key={`${file.url}-${index}`} className={css.card} data-kind="file"
              href={file.url} download={file.name} title={file.name}>
              {footer}
            </a>
          )
        }
        const label = `${t('image.open')} · ${file.name}`
        return (
          <button key={`${file.url}-${index}`} type="button" className={css.card} data-kind="image"
            title={label} aria-label={label} onClick={() => { if (onFilePreview) onFilePreview(file.name, { url: file.url }); else setOpen(file) }}>
            {/* The name below carries the label, so the thumbnail stays decorative. */}
            <img className={css.thumb} src={file.url} alt="" loading="lazy" decoding="async" />
            {footer}
          </button>
        )
      })}
      {open !== undefined && (
        <ImageLightbox
          src={open.url}
          alt={open.name}
          labels={{ dialog: t('image.dialog'), close: t('image.close') }}
          onClose={close}
        />
      )}
    </div>
  )
}
