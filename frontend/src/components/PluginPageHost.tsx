import type { PendingOpen, PreviewTheme } from '../filePreview.ts'
import type { StudioAskAiRequest, StudioViewContext } from '../plugins/protocol.ts'
import { usePluginPages } from '../plugins/react.tsx'

/** One plugin page shown on its own, outside the plugin list. */
export function PluginPageHost({ owner, page, workspaceRoot, pendingOpen, theme, onStudioContext, onAskAi }: {
  owner: string
  page: string
  workspaceRoot: string
  pendingOpen?: PendingOpen | undefined
  theme?: PreviewTheme | undefined
  onStudioContext?: ((context: StudioViewContext | null) => void) | undefined
  onAskAi?: ((request: StudioAskAiRequest) => void) | undefined
}) {
  const { packages, pages, errors } = usePluginPages()
  const installed = packages.find(item => item.id === owner)
  const contribution = pages.find(item => item.owner === owner && item.scope === installed?.scope && item.id === page)
  if (!installed?.enabled || !contribution) {
    const reason = errors[owner] ?? '正在加载插件…'
    return <p style={{ padding: 32, color: 'var(--codex-text-muted)' }}>{reason}</p>
  }
  const Page = contribution.component
  return <Page key={`${owner}:${installed.scope}:${page}`} owner={installed} pageId={page} workspaceRoot={workspaceRoot} pendingOpen={pendingOpen} theme={theme} onStudioContext={onStudioContext} onAskAi={onAskAi} />
}
