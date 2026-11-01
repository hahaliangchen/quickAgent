/** Where the flow graph draws each frame. */
export interface FramePlacement {
  readonly x: number
  readonly y: number
}

export const FRAME_WIDTH = 280
const COLUMN = FRAME_WIDTH + 36
const ROW = 190

/**
 * A tidy tree: each frame one row below its caller, the calls of one frame
 * side by side in the order they were made, and a caller centred over its
 * calls. A frame whose caller is not in the list starts a tree of its own.
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
  let next = 0
  const place = (id: string, depth: number): number => {
    // A corrupt caller chain must not loop; a frame is placed once.
    if (placements.has(id)) return placements.get(id)!.x
    placements.set(id, { x: 0, y: 0 })
    const children = (calls.get(id) ?? []).filter(child => !placements.has(child))
    const xs = children.map(child => place(child, depth + 1))
    const x = xs.length === 0 ? (next++) * COLUMN : (xs[0]! + xs.at(-1)!) / 2
    placements.set(id, { x, y: depth * ROW })
    return x
  }
  for (const root of roots) place(root, 0)
  // Frames only reachable through a cycle still get a place of their own.
  for (const frame of frames) if (!placements.has(frame.id)) place(frame.id, 0)
  return placements
}
