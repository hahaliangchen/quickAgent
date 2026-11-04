import { usePluginPages } from '../plugins/react.tsx'

/** One plugin page shown on its own, outside the plugin list. */
export function PluginPageHost({ owner, page, workspaceRoot }: { owner: string; page: string; workspaceRoot: string }) {
  const { packages, pages, errors } = usePluginPages()
  const installed = packages.find(item => item.id === owner)
  const contribution = pages.find(item => item.owner === owner && item.scope === installed?.scope && item.id === page)
  if (!installed?.enabled || !contribution) {
    const reason = errors[owner] ?? (installed && !installed.enabled ? '插件已停用，可在技能工作台的插件页面启用。' : '正在加载插件…')
    return <p style={{ padding: 32, color: 'var(--codex-text-muted)' }}>{reason}</p>
  }
  const Page = contribution.component
  return <Page key={`${owner}:${installed.scope}:${page}`} owner={installed} pageId={page} workspaceRoot={workspaceRoot} />
}
