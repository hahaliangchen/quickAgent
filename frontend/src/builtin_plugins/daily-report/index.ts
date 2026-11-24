import type { FrontendPlugin } from '../../plugins/protocol.ts'
import Workbench from './page.tsx'

export default {
  name: 'daily-report',
  inject: ['pluginPages'],
  apply(ctx, { owner }) { ctx.pluginPages.register(owner, 'workbench', 'daily-report.workbench', Workbench) },
} satisfies FrontendPlugin
