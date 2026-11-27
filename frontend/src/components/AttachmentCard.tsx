import { useCallback, useState } from 'react'
import { ImageLightbox } from '@deepseek-ai/dsh-client-ui-primitives'
import { t } from '../i18n.ts'
import { previewKind, type OpenFilePreview } from '../filePreview.ts'
import { RichFileIcon, fileVisualMeta } from './RichFileIcon.tsx'
import css from './AttachmentCard.module.css'

/** One file a user turn was sent with: the image itself, or just its name. */
export interface UserAttachment {
  readonly name: string
  readonly url: string
  readonly image: boolean
}

/** Characters that fit the footer at its 13px size; CJK counts double. */
const IMAGE_NAME_BUDGET = 24
const FILE_NAME_BUDGET = 28

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

const DOWNLOAD_ICON = (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
    <polyline points="7 10 12 15 17 10" />
    <line x1="12" y1="15" x2="12" y2="3" />
  </svg>
)

/**
 * The files a user message carried, shown above its bubble.
 * - Images render as crisp rounded thumbnail cards with a colorful footer.
 * - Documents / code / archives render as elevated file cards with colorful
 *   Material Icon tiles, file-type badges, click-to-preview, and an integrated download button.
 */
export function AttachmentCards({ items, onFilePreview }: { items: readonly UserAttachment[]; onFilePreview?: OpenFilePreview | undefined }) {
  const [open, setOpen] = useState<UserAttachment | undefined>(undefined)
  const close = useCallback(() => { setOpen(undefined) }, [])
  return (
    <div className={css.attachments}>
      {items.map((file, index) => {
        const meta = fileVisualMeta(file.name)
        if (!file.image) {
          const canPreview = Boolean(onFilePreview && previewKind(file.name) !== 'unsupported')
          const cardBody = (
            <>
              <span className={css.fileIconTile} data-tone={meta.tone}>
                <RichFileIcon path={file.name} size={24} />
              </span>
              <span className={css.fileInfo}>
                <span className={css.name}>{middleShorten(file.name, FILE_NAME_BUDGET)}</span>
                <span className={css.fileMetaRow} aria-hidden="true">
                  <span className={css.fileTypePill} data-tone={meta.tone}>{meta.label}</span>
                  <span className={css.fileActionHint}>{canPreview ? '点击预览' : '点击下载'}</span>
                </span>
              </span>
            </>
          )
          if (canPreview && onFilePreview) {
            return (
              <div key={`${file.url}-${index}`} className={css.fileCardShell}>
                <button
                  type="button"
                  className={css.fileCardMain}
                  aria-label={file.name}
                  title={`预览 ${file.name}`}
                  onClick={() => onFilePreview(file.name, { url: file.url })}
                >
                  {cardBody}
                </button>
                <a
                  className={css.fileDownloadBtn}
                  href={file.url}
                  download={file.name}
                  aria-label={'下载 ' + file.name}
                  title={`下载 ${file.name}`}
                >
                  {DOWNLOAD_ICON}
                </a>
              </div>
            )
          }
          return (
            <a
              key={`${file.url}-${index}`}
              className={css.fileCardLink}
              href={file.url}
              download={file.name}
              title={`下载 ${file.name}`}
            >
              {cardBody}
              <span className={css.fileDownloadGlyph} aria-hidden="true">{DOWNLOAD_ICON}</span>
            </a>
          )
        }
        const label = `${t('image.open')} · ${file.name}`
        return (
          <button
            key={`${file.url}-${index}`}
            type="button"
            className={css.card}
            data-kind="image"
            title={label}
            aria-label={label}
            onClick={() => { if (onFilePreview) onFilePreview(file.name, { url: file.url }); else setOpen(file) }}
          >
            <span className={css.thumbWrap}>
              <img className={css.thumb} src={file.url} alt="" loading="lazy" decoding="async" />
            </span>
            <span className={css.foot}>
              <RichFileIcon path={file.name} size={15} className={css.glyph} />
              <span className={css.name}>{middleShorten(file.name, IMAGE_NAME_BUDGET)}</span>
            </span>
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
