import type { FrontendPlugin } from '../../plugins/protocol.ts'
import GitPage from './page.tsx'

const plugin: FrontendPlugin = {
  name: 'git',
  inject: ['agentApi', 'pluginPages'],
  apply(ctx, { owner }) {
    ctx.pluginPages.register(owner, 'history', 'git.history', GitPage)
  },
}
export default plugin
