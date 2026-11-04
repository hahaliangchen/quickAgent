/** Workspace-relative paths a page offers the floating chat: the file in view and those dropped in. */
export interface CodeChatSnapshot { current: string | undefined; tags: readonly string[] }

/** Drag data type for a workspace path; system files keep using `Files`. */
export const WORKSPACE_PATH_TYPE = 'application/x-workspace-path'

export class CodeChat {
  private snapshot: CodeChatSnapshot = { current: undefined, tags: [] }
  private listeners = new Set<() => void>()
  getSnapshot = (): CodeChatSnapshot => this.snapshot
  subscribe = (listener: () => void): (() => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener) } }
  private publish(next: Partial<CodeChatSnapshot>) { this.snapshot = { ...this.snapshot, ...next }; for (const listener of this.listeners) listener() }
  setCurrent(path: string | undefined) { if (path !== this.snapshot.current) this.publish({ current: path }) }
  add(path: string) { if (!this.snapshot.tags.includes(path)) this.publish({ tags: [...this.snapshot.tags, path] }) }
  remove(path: string) { this.publish({ tags: this.snapshot.tags.filter(tag => tag !== path) }) }
  clear() { this.publish({ tags: [] }) }
}

declare module '@deepseek-ai/cordis' { interface Context { codeChat: CodeChat } }
