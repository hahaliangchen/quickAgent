import { createContext, useContext, type ReactNode } from 'react'

const ConversationTask = createContext('')

/** The conversation whose artifacts the rows below can show; empty outside one. */
export function ConversationTaskProvider({ taskId, children }: { taskId: string; children: ReactNode }) {
  return <ConversationTask.Provider value={taskId}>{children}</ConversationTask.Provider>
}

/** The task a row belongs to, so a saved picture can be addressed without a prop chain. */
export function useConversationTaskId(): string {
  return useContext(ConversationTask)
}
