import type { Context } from '@deepseek-ai/cordis'
import { api } from '../api.ts'
import { ChatView } from '../components/ChatView.tsx'
import { Composer } from '../components/Composer.tsx'
import { FlowView } from '../components/FlowView.tsx'
import { Sidebar } from '../components/Sidebar.tsx'

declare module '@deepseek-ai/cordis' { interface Context { agentApi: typeof api } }
export function AgentApiPlugin(ctx: Context): void { ctx.provide('agentApi', api) }
export const shellPlugins = [
  { name: 'agent-sidebar', inject: ['slots'], apply(ctx: Context) { ctx.slots.register({ name: 'agent.sidebar' }, Sidebar) } },
  { name: 'agent-chat', inject: ['slots', 'agentApi'], apply(ctx: Context) { ctx.slots.register({ name: 'agent.chat' }, ChatView) } },
  // The flow graph reads the agent's frames itself; it is given only which conversation to show.
  { name: 'agent-flow', inject: ['slots', 'agentApi'], apply(ctx: Context) { ctx.slots.register({ name: 'agent.flow' }, FlowView) } },
  { name: 'agent-composer', inject: ['slots', 'agentApi'], apply(ctx: Context) { ctx.slots.register({ name: 'agent.composer' }, Composer) } },
]
