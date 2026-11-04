/** Sails on the diagonals, each a stock with its cloth on one side. */
const SAILS = [45, 135, 225, 315]

/**
 * The QuickAgent mark: the windmill of the welcome illustration as a silhouette.
 * It takes --brand-mark, so it follows the app's theme rather than the system's;
 * assets/windmill-mark.svg is the same drawing for the favicon and notifications.
 */
export function BrandMark({ size = 28, className }: { size?: number; className?: string }) {
  return <svg width={size} height={size} viewBox="0 0 96 96" className={className} aria-hidden="true">
    <g fill="var(--brand-mark, #4a8f2e)">
      {/* Roof and tower as one outline, with the window and door cut out. */}
      <path fillRule="evenodd" d="M33 44 48 14 63 44h-4l6 44H31l6-44ZM45 58a3 3 0 0 1 6 0v2a3 3 0 0 1-6 0ZM43 88v-8a5 5 0 0 1 10 0v8Z" />
      {SAILS.map(angle => <g key={angle} transform={`translate(48 38) rotate(${angle})`}>
        <rect x="-1.6" y="-41" width="3.2" height="38" rx="1.6" />
        <rect x="1" y="-41" width="10" height="29" rx="1.8" />
      </g>)}
      <circle cx="48" cy="38" r="4.2" />
    </g>
  </svg>
}
