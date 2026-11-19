/**
 * The QuickAgent mark: four pointed geometric emerald leaves radiating
 * diagonally from an open center, each with a crisp transparent center vein.
 * Follows --brand-mark for light and dark themes; assets/windmill-mark.svg
 * is the matching favicon.
 */
export function BrandMark({ size = 28, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" className={className} aria-hidden="true">
      <g fill="var(--brand-mark, #1d9e75)">
        {/* Top-right primary leaf with transparent inner vein notch */}
        <path d="M32.4 29.0C31.5 13.5 39.5 8.5 55.5 8.5C55.5 24.5 50.5 32.5 35.0 31.6L47.2 16.8Z" />
        {/* Bottom-left primary leaf (180 deg rotation) */}
        <path
          d="M32.4 29.0C31.5 13.5 39.5 8.5 55.5 8.5C55.5 24.5 50.5 32.5 35.0 31.6L47.2 16.8Z"
          transform="rotate(180 32 32)"
        />
        {/* Top-left sleeker cross leaf with transparent inner vein notch */}
        <path d="M29.8 28.4C29.2 17.5 22.0 11.2 10.5 10.5C11.2 22.0 17.5 29.2 28.4 29.8L18.4 18.4Z" />
        {/* Bottom-right sleeker cross leaf (180 deg rotation) */}
        <path
          d="M29.8 28.4C29.2 17.5 22.0 11.2 10.5 10.5C11.2 22.0 17.5 29.2 28.4 29.8L18.4 18.4Z"
          transform="rotate(180 32 32)"
        />
      </g>
    </svg>
  )
}
