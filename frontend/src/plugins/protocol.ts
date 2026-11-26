import type { Context } from '@deepseek-ai/cordis'
import type { ComponentType } from 'react'
import type { SkillPackage } from '../api.ts'

import type { PendingOpen, PreviewTheme } from '../filePreview.ts'

export interface StudioViewContext {
  readonly plugin: string
  readonly id: string
  readonly badge: string
  readonly detail: string
  readonly text: string
}
export interface StudioAskAiRequest {
  readonly plugin: string
  readonly prompt?: string | undefined
}
export const DB_SYNC_EVENT = 'quickagent:db-sync'
export interface DbStudioSyncAction {
  readonly action: 'save_connection' | 'inspect' | 'query' | 'explain' | 'execute' | 'focus_view'
  readonly connectionId?: string | undefined
  readonly sqlitePath?: string | undefined
  readonly schema?: string | undefined
  readonly table?: string | undefined
  readonly sql?: string | undefined
  readonly column?: string | undefined
  readonly row?: number | undefined
  readonly offset?: number | undefined
  readonly range?: string | undefined
  readonly findColumn?: string | undefined
  readonly findValue?: string | undefined
}
export const EXCEL_SYNC_EVENT = 'quickagent:excel-sync'
export interface ExcelStudioSyncAction {
  readonly path?: string | undefined
  readonly sheet?: string | undefined
  readonly range?: string | undefined
  readonly column?: string | undefined
  readonly row?: number | undefined
  readonly findColumn?: string | undefined
  readonly findValue?: string | undefined
}
export const DAILY_REPORT_SYNC_EVENT = 'quickagent:daily-report-sync'
export interface DailyReportSyncAction {
  readonly action: 'prepare_draft' | 'write_guide' | 'submit' | 'today_activity' | 'list'
  readonly reportDate?: string | undefined
  readonly content?: string | undefined
}
export interface PluginPageProps {
  workspaceRoot: string
  owner: SkillPackage
  pageId: string
  pendingOpen?: PendingOpen | undefined
  theme?: PreviewTheme | undefined
  onStudioContext?: ((context: StudioViewContext | null) => void) | undefined
  onAskAi?: ((request: StudioAskAiRequest) => void) | undefined
}
export interface FrontendConfig { owner: SkillPackage }
/** Same Cordis registration contract for project modules and installed modules. */
export interface FrontendPlugin {
  name: string
  inject: string[]
  apply(ctx: Context, config: FrontendConfig): void | Promise<void>
}
export interface PageContribution {
  owner: string
  scope: number
  id: string
  entry: string
  component: ComponentType<PluginPageProps>
}
