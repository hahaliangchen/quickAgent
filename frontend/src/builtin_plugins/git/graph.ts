export interface GitCommit { hash: string; parents: string[]; author: string; email: string; time: number | null; refs: string[]; subject: string }
export interface GitChange { path: string; orig_path: string | null; index: string; worktree: string; conflict: boolean }
export interface GitBranch { name: string; remote: boolean; hash: string; upstream: string | null; current: boolean; time: number | null }
export interface GitStash { name: string; hash: string; time: number | null; subject: string }
export interface GitOverview {
  repository: string; root: string; head: string | null; head_hash: string | null; upstream: string | null
  ahead: number; behind: number; changes: GitChange[]; branches: GitBranch[]; stashes: GitStash[]; commits: GitCommit[]
}
export interface GitFile { path: string; orig_path: string | null; added: number | null; deleted: number | null; untracked?: boolean }
export interface GitCommitDetail {
  hash: string | null; parents?: string[]; author?: string; email?: string; time?: number | null
  refs?: string[]; message?: string; files: GitFile[]
}
export interface GitPatch { commit: string | null; path: string; patch: string; truncated: boolean }

/** One row's slice of the graph: lines entering from above, the dot, lines leaving below. */
export interface GraphRow { column: number; width: number; top: [number, number][]; bottom: [number, number][] }

/** Lanes for commits listed children-first (git's date or topo order). */
export function layoutGraph(commits: readonly GitCommit[]): GraphRow[] {
  const lanes: (string | null)[] = []
  return commits.map(commit => {
    let column = lanes.indexOf(commit.hash)
    if (column < 0) {
      column = lanes.indexOf(null)
      if (column < 0) column = lanes.push(null) - 1
    }
    // Every lane waiting for this commit bends into its dot.
    const top: [number, number][] = []
    lanes.forEach((waiting, lane) => { if (waiting !== null) top.push([lane, waiting === commit.hash ? column : lane]) })
    lanes.forEach((waiting, lane) => { if (waiting === commit.hash) lanes[lane] = null })
    const origin = new Map<number, number>()
    const [first, ...others] = commit.parents
    if (first !== undefined) {
      const existing = lanes.indexOf(first)
      if (existing >= 0) origin.set(existing, column)
      else { lanes[column] = first; origin.set(column, column) }
    }
    for (const parent of others) {
      let lane = lanes.indexOf(parent)
      if (lane < 0) {
        lane = lanes.indexOf(null)
        if (lane < 0 || lane === column) lane = lanes.findIndex((waiting, i) => waiting === null && i !== column)
        if (lane < 0) lane = lanes.push(null) - 1
        lanes[lane] = parent
      }
      origin.set(lane, column)
    }
    const bottom: [number, number][] = []
    lanes.forEach((waiting, lane) => { if (waiting !== null) bottom.push([origin.get(lane) ?? lane, lane]) })
    while (lanes.length && lanes[lanes.length - 1] === null) lanes.pop()
    const width = Math.max(column + 1, ...top.flat().map(n => n + 1), ...bottom.flat().map(n => n + 1))
    return { column, width, top, bottom }
  })
}

export interface DiffLine { kind: 'add' | 'del' | 'ctx' | 'hunk'; text: string; line: number | null }

/** Unified diff to display lines; file headers are dropped, hunks kept. */
export function parsePatch(patch: string): DiffLine[] {
  const out: DiffLine[] = []
  let oldLine = 0, newLine = 0, inHunk = false
  for (const raw of patch.split('\n')) {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw)
    if (hunk) { oldLine = Number(hunk[1]); newLine = Number(hunk[2]); inHunk = true; out.push({ kind: 'hunk', text: raw, line: null }); continue }
    if (raw.startsWith('diff --git')) { inHunk = false; continue }
    if (!inHunk) continue
    if (raw.startsWith('+')) out.push({ kind: 'add', text: raw.slice(1), line: newLine++ })
    else if (raw.startsWith('-')) out.push({ kind: 'del', text: raw.slice(1), line: oldLine++ })
    else if (raw.startsWith('\\')) continue
    else { out.push({ kind: 'ctx', text: raw.slice(1), line: newLine++ }); oldLine++ }
  }
  while (out.length && out[out.length - 1]!.kind === 'ctx' && out[out.length - 1]!.text === '') out.pop()
  return out
}

export function relativeTime(seconds: number | null | undefined, now = Date.now()): string {
  if (!seconds) return ''
  const date = new Date(seconds * 1000)
  const diff = (now - date.getTime()) / 1000
  if (diff < 60) return '刚刚'
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`
  if (diff < 86400 && new Date(now).getDate() === date.getDate()) return `${Math.floor(diff / 3600)} 小时前`
  const hm = `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`
  // Count calendar days, so "yesterday" and "2 days ago" never overlap.
  const midnight = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  const days = Math.round((midnight(new Date(now)) - midnight(date)) / 86400000)
  if (days === 1) return `昨天 ${hm}`
  if (days < 7) return `${days} 天前`
  return date.getFullYear() === new Date(now).getFullYear() ? `${date.getMonth() + 1}月${date.getDate()}日` : date.toLocaleDateString()
}

export function absoluteTime(seconds: number | null | undefined): string {
  if (!seconds) return ''
  const d = new Date(seconds * 1000)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}
