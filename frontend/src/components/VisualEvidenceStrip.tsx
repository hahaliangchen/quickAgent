import { useCallback, useState } from 'react'
import { ImageLightbox } from '@deepseek-ai/dsh-client-ui-primitives'
import { t } from '../i18n.ts'
import { visualArtifactUrl, type VisualEvidence } from '../visual.ts'
import css from './VisualEvidenceStrip.module.css'

/** A browser loop can name a dozen near-copies; the rest wait behind one chip. */
const VISIBLE = 8

/**
 * The pictures one tool call put in front of the user — the screenshot it took,
 * the image it generated, or the pictures it was asked to analyse — shown in
 * that call's own row. They sit outside the collapsible detail, because "which
 * picture" is the point of the call. Pressing one opens the original in the
 * lightbox; nothing leaves the page.
 */
export function VisualEvidenceStrip({ items }: { items: readonly VisualEvidence[] }) {
  const [open, setOpen] = useState<VisualEvidence | undefined>(undefined)
  const [all, setAll] = useState(false)
  const close = useCallback(() => { setOpen(undefined) }, [])
  const shown = all ? items : items.slice(0, VISIBLE)
  const hidden = items.length - shown.length
  return (
    <div className={css.strip}>
      {shown.map((item, index) => {
        const size = item.width !== undefined && item.height !== undefined ? ` · ${item.width}×${item.height}` : ''
        const label = `${t('image.open')} · ${item.tool}${size}`
        return (
          <button key={item.key} type="button" className={css.thumb} title={label} aria-label={label}
            onClick={() => { setOpen(item) }}>
            <img className={css.image} src={visualArtifactUrl(item.taskId, item.artifactId)} alt=""
              loading="lazy" decoding="async" />
            {/* The vision model is told "images 1-N"; the number keeps them matched. */}
            {items.length > 1 && <span className={css.index} aria-hidden>{index + 1}</span>}
          </button>
        )
      })}
      {hidden > 0 && (
        <button type="button" className={css.more} onClick={() => { setAll(true) }}>+{hidden}</button>
      )}
      {open !== undefined && (
        <ImageLightbox
          src={visualArtifactUrl(open.taskId, open.artifactId)}
          alt={open.tool}
          labels={{ dialog: t('image.dialog'), close: t('image.close') }}
          onClose={close}
        />
      )}
    </div>
  )
}
