/**
 * The folder a project row draws.
 *
 * One closed tabbed outline — a tab flush at the top-left, a short shoulder, a
 * rounded body — drawn at a single weight so a list of projects reads as a list
 * of folders rather than as a row of icons each saying something different. The
 * chevron beside it says whether the project is open, so the folder itself
 * never changes shape.
 */
export function FolderGlyph({ size = 16, className }: { size?: number | undefined; className?: string | undefined }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      className={className}
      fill="none"
      stroke="currentColor"
      strokeWidth="1.35"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M2.8 3H5.4C5.95 3 6.45 3.2 6.85 3.55L7.7 4.6H12.9C13.62 4.6 14.2 5.18 14.2 5.9V11.7C14.2 12.42 13.62 13 12.9 13H3.1C2.38 13 1.8 12.42 1.8 11.7V4C1.8 3.45 2.25 3 2.8 3Z" />
    </svg>
  )
}
