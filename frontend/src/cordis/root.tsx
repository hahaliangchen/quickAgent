import type { Context } from '@deepseek-ai/cordis'
import { App } from '../App.tsx'
import type { AgentSlotProps } from './slot-contract.ts'
import { CordisProvider } from './react.tsx'

/** Declare the page's child slots in the original DSH renderer's root tree. */
export const RootPlugin = {
  name: 'agent-root',
  inject: ['slots', 'agentApi', 'pluginPages', 'codeChat'],
  apply(ctx: Context): void {
    ctx.slots.register({
      name: 'root',
      children: {
        'agent.sidebar': { kind: 'single', scope: 'root' },
        'agent.chat': { kind: 'single', scope: 'root' },
        'agent.flow': { kind: 'single', scope: 'root' },
        'agent.composer': { kind: 'single', scope: 'root' },
      },
    }, (props: AgentSlotProps) => (
      <CordisProvider context={ctx}><App {...props} /></CordisProvider>
    ))
  },
}
