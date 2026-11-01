import type { ComponentProps } from 'react'
import type { PropsRenderSlots } from '@deepseek-ai/dsh-client-ui-slots'
import type { ChatView } from '../components/ChatView.tsx'
import type { Composer } from '../components/Composer.tsx'
import type { FlowView } from '../components/FlowView.tsx'
import type { Sidebar } from '../components/Sidebar.tsx'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface SlotMap {
    'agent.sidebar': { kind: 'single'; scope: 'root'; owner: ComponentProps<typeof Sidebar> }
    'agent.chat': { kind: 'single'; scope: 'root'; owner: ComponentProps<typeof ChatView> }
    'agent.flow': { kind: 'single'; scope: 'root'; owner: ComponentProps<typeof FlowView> }
    'agent.composer': { kind: 'single'; scope: 'root'; owner: ComponentProps<typeof Composer> }
  }
}

export type AgentSlotProps = PropsRenderSlots<
  'agent.sidebar' | 'agent.chat' | 'agent.flow' | 'agent.composer'
>
