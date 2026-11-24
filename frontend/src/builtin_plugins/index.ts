import type { FrontendPlugin } from '../plugins/protocol.ts'

/** Explicit project membership. Downloaded modules are never globbed into it. */
export const builtinFrontends: Record<string, () => Promise<{ default: FrontendPlugin }>> = {
  blueprint: () => import('./blueprint/index.ts'),
  git: () => import('./git/index.ts'),
  ssh: () => import('./ssh/index.ts'),
}
