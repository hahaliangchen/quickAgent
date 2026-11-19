/**
 * The QuickAgent mark: four symmetrical geometric emerald leaves forming
 * a crisp modern pinwheel emblem. Follows --brand-mark for light/dark themes;
 * assets/windmill-mark.svg is the matching favicon and notification icon.
 */
export function BrandMark({ size = 28, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" className={className} aria-hidden="true">
      <g fill="var(--brand-mark, #1d9e75)">
        {/* Top-left leaf */}
        <path d="M12 12C22.5 12 29 18.5 29 29C18.5 29 12 22.5 12 12Z" />
        {/* Top-right leaf */}
        <path d="M52 12C52 22.5 45.5 29 35 29C35 18.5 41.5 12 52 12Z" />
        {/* Bottom-right leaf */}
        <path d="M52 52C41.5 52 35 45.5 35 35C45.5 35 52 41.5 52 52Z" />
        {/* Bottom-left leaf */}
        <path d="M12 52C12 41.5 18.5 35 29 35C29 45.5 22.5 52 12 52Z" />
      </g>
    </svg>
  )
}
