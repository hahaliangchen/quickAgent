/** Where the flow graph draws each frame. */
export interface FramePlacement {
  readonly x: number
  readonly y: number
}

export const FRAME_WIDTH = 280
/** One frame per row: the graph reads top to bottom. */
export const FRAME_ROW = 150
/** How far a call sits to the right of the frame whose program made it. */
export const FRAME_INDENT = 24
/** Deeper chains stop indenting, so a long call chain stays one narrow column. */
const DEEPEST_INDENT = 8

/**
 * A vertical call chain: the frames read top to bottom in the order they were
 * made, and every call is indented under the frame whose program made it, so
 * the column never fans out sideways. A frame whose caller is not in the list
 * starts a run of its own, at the left edge.
 */
export function layoutFrameTree(frames: readonly { readonly id: string; readonly caller: string | null | undefined }[]): Map<string, FramePlacement> {
  const ids = new Set(frames.map(frame => frame.id))
  const calls = new Map<string, string[]>()
  const roots: string[] = []
  for (const frame of frames) {
    if (frame.caller !== null && frame.caller !== undefined && frame.caller !== frame.id && ids.has(frame.caller)) {
      calls.set(frame.caller, [...calls.get(frame.caller) ?? [], frame.id])
    } else {
      roots.push(frame.id)
    }
  }
  const placements = new Map<string, FramePlacement>()
  let row = 0
  const place = (id: string, depth: number): void => {
    // A corrupt caller chain must not loop; a frame is placed once.
    if (placements.has(id)) return
    placements.set(id, { x: Math.min(depth, DEEPEST_INDENT) * FRAME_INDENT, y: (row++) * FRAME_ROW })
    // A call's own calls follow it, so one subtree stays together on the way down.
    for (const child of calls.get(id) ?? []) place(child, depth + 1)
  }
  for (const root of roots) place(root, 0)
  // Frames only reachable through a cycle still get a place of their own.
  for (const frame of frames) if (!placements.has(frame.id)) place(frame.id, 0)
  return placements
}
