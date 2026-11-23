import type { FrontendPlugin } from '../../plugins/protocol.ts'
import Workbench from './page.tsx'

export default {
  name: 'ssh',
  inject: ['pluginPages'],
  apply(ctx, { owner }) { ctx.pluginPages.register(owner, 'workbench', 'ssh.workbench', Workbench) },
} satisfies FrontendPlugin
