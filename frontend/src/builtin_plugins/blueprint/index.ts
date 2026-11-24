import type { FrontendPlugin } from '../../plugins/protocol.ts'
import Workbench from './page.tsx'

export default {
  name: 'blueprint',
  inject: ['pluginPages'],
  apply(ctx, { owner }) { ctx.pluginPages.register(owner, 'workbench', 'blueprint.workbench', Workbench) },
} satisfies FrontendPlugin

