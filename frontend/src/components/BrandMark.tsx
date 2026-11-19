/**
 * The QuickAgent mark: four pointed geometric emerald leaves radiating
 * diagonally from an open center — two fuller primary leaves (NE/SW)
 * paired with two sleek cross leaves (NW/SE). Follows --brand-mark for
 * light and dark themes; assets/windmill-mark.svg is the matching favicon.
 */
export function BrandMark({ size = 28, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" className={className} aria-hidden="true">
      {/* Primary fuller diagonal leaves (top-right & bottom-left) */}
      <g fill="var(--brand-mark, #1d9e75)">
        <path d="M34 30C33.5 17 41.5 9 54.5 9.5C55 22.5 47 30.5 34 30Z" />
        <path d="M30 34C30.5 47 22.5 55 9.5 54.5C9 41.5 17 33.5 30 34Z" />
      </g>
      {/* Sleeker cross leaves (top-left & bottom-right) */}
      <g fill="var(--brand-mark-secondary, #127c5c)">
        <path d="M29 29C19.5 29 12.5 22.5 12 12C22.5 12.5 29 19.5 29 29Z" />
        <path d="M35 35C44.5 35 51.5 41.5 52 52C41.5 51.5 35 44.5 35 35Z" />
      </g>
    </svg>
  )
}
