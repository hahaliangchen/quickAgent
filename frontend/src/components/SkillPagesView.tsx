import { useState } from 'react'
import clsx from 'clsx'
import { useAgentApi } from '../cordis/react.tsx'
import { usePluginPages } from '../plugins/react.tsx'
import { Icon } from './WorkbenchIcon.tsx'
import styles from './SkillPagesView.module.css'

/** Installed skill packages with their pages, and the forms that add more. */
export function SkillPagesView({ workspaceRoot, onBack }: { workspaceRoot: string; onBack: () => void }) {
  const api = useAgentApi()
  const { packages, pages, errors } = usePluginPages()
  const [selected, setSelected] = useState<{ owner: string; page: string }>()
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [installPath, setInstallPath] = useState('')
  const [remoteUrl, setRemoteUrl] = useState('')
  const [remoteToken, setRemoteToken] = useState('')
  // An open page needs the width; the list folds away and comes back from the page bar.
  const [listOpen, setListOpen] = useState(true)
  const owner = packages.find(item => item.id === selected?.owner && item.enabled)
  const page = owner?.pages.find(item => item.id === selected?.page)
  const contribution = pages.find(item => item.owner === owner?.id && item.scope === owner?.scope && item.id === page?.id)
  const Page = contribution?.component
  const problem = error || Object.values(errors).join('；')
  async function change(action: () => Promise<unknown>) {
    setBusy(true); setError('')
    try { await action() }
    catch (reason) { setError(String(reason)) }
    finally { setBusy(false) }
  }

  return <div className={styles.root}>
    <header className={styles.header}>
      <button type="button" className={styles.back} onClick={onBack}><Icon name="back" size={16} />返回对话</button>
      <span className={styles.divider} />
      <h1>插件与技能页面</h1>
      <span className={styles.headerNote}>页面和工具随所属插件停用；打开页面不会给 Agent 自动加载技能。</span>
    </header>

    <div className={styles.body}>
      <aside className={clsx(styles.side, page && owner && !listOpen && styles.sideHidden)} aria-label="技能插件">
        <div className={styles.sectionHead}><h2>已安装</h2><span>{packages.length}</span></div>
        {problem && <p role="alert" className={styles.error}>{problem}</p>}
        {packages.length === 0 && <p className={styles.hint}>还没有安装插件。</p>}
        <div className={styles.cards}>
          {packages.map(item => <article key={item.id} className={clsx(styles.card, !item.enabled && styles.cardOff)}>
            <div className={styles.cardHead}>
              <span className={styles.cardIcon}><Icon name={item.origin === 'builtin' ? 'box' : 'plug'} size={19} /></span>
              <div className={styles.cardTitle}>
                <strong title={item.title}>{item.title}</strong>
                <span className={styles.badges}>
                  <span className={styles.badge}>{item.origin === 'builtin' ? '内置' : item.remote ? '远程' : '外部'}</span>
                  <span className={clsx(styles.state, item.enabled && styles.stateOn)}>{item.enabled ? '已启用' : '已停用'}</span>
                </span>
              </div>
            </div>
            {item.description && <p className={styles.description} title={item.description}>{item.description}</p>}
            <p className={styles.meta}>{item.tools.length} 个工具 · {item.pages.length} 个页面</p>
            {item.enabled && item.pages.length > 0 && <div className={styles.pages}>
              {item.pages.map(entry => {
                const active = selected?.owner === item.id && selected.page === entry.id
                return <button key={entry.id} type="button" aria-pressed={active} className={clsx(styles.pageChip, active && styles.pageChipActive)}
                  onClick={() => { setSelected({ owner: item.id, page: entry.id }); setListOpen(false) }}><Icon name="file" size={14} />{entry.title}</button>
              })}
            </div>}
            <div className={styles.actions}>
              {/* Built-in plugins are part of the agent and always run. */}
              {item.origin !== 'builtin' && <button type="button" disabled={busy} onClick={() => { void change(() => api.setPackageEnabled(item.id, !item.enabled)) }}>
                <Icon name="power" size={14} />{item.enabled ? '停用' : '启用'}</button>}
              <button type="button" disabled={busy} onClick={() => { void change(() => api.reloadPackage(item.id)) }}><Icon name="refresh" size={14} />重载</button>
              {item.origin === 'external' && <button type="button" className={styles.danger} disabled={busy}
                onClick={() => { void change(() => api.removePackage(item.id)) }}><Icon name="trash" size={14} />移除</button>}
            </div>
          </article>)}
        </div>

        <div className={styles.sectionHead}><h2>添加插件</h2></div>
        <form className={styles.form} onSubmit={event => { event.preventDefault(); void change(() => api.installPackage(installPath.trim())) }}>
          <label className={styles.formTitle}><Icon name="folder" size={16} />本地技能包</label>
          <p className={styles.formHint}>包含 plugin.json 的目录。</p>
          <input aria-label="本地技能包目录" placeholder="/path/to/skill-package" value={installPath} onChange={event => setInstallPath(event.target.value)} />
          <button type="submit" className={styles.primary} disabled={busy || !installPath.trim()}>安装技能包</button>
        </form>
        <form className={styles.form} onSubmit={event => { event.preventDefault(); void change(async () => { await api.connectRemotePackage(remoteUrl.trim(), remoteToken); setRemoteToken('') }) }}>
          <label className={styles.formTitle}><Icon name="link" size={16} />远程插件服务</label>
          <p className={styles.formHint}>例如 PPT 服务的 …/aiPpt/plugin 地址。</p>
          <input aria-label="远程插件服务地址" placeholder="https://…/aiPpt/plugin" value={remoteUrl} onChange={event => setRemoteUrl(event.target.value)} />
          <input type="password" aria-label="远程服务凭证" placeholder="服务凭证" value={remoteToken} onChange={event => setRemoteToken(event.target.value)} />
          <button type="submit" className={styles.primary} disabled={busy || !remoteUrl.trim()}>握手并连接</button>
        </form>
      </aside>

      <main className={styles.main}>
        {page && owner ? <>
          <div className={styles.pageBar}>
            <button type="button" className={styles.iconButton} aria-pressed={listOpen} aria-label={listOpen ? '收起插件列表' : '展开插件列表'}
              title={listOpen ? '收起插件列表' : '展开插件列表'} onClick={() => setListOpen(value => !value)}><Icon name="panel" size={16} /></button>
            <span className={styles.divider} />
            <span className={styles.crumb}>{owner.title}<Icon name="chevron" size={13} /><strong>{page.title}</strong></span>
            <button type="button" className={styles.iconButton} aria-label="关闭页面" title="关闭页面" onClick={() => { setSelected(undefined); setListOpen(true) }}><Icon name="close" size={15} /></button>
          </div>
          <div className={styles.pageBody}>
            {Page ? <Page key={`${owner.id}:${owner.scope}:${page.id}`} owner={owner} pageId={page.id} workspaceRoot={workspaceRoot} />
              : <p className={styles.hint}>等待插件注册页面…</p>}
          </div>
        </> : <div className={styles.empty}>
          <span className={styles.emptyIcon}><Icon name="file" size={26} /></span>
          <strong>选择一个插件页面</strong>
          <span>在左侧已启用的插件下点页面名称，在这里打开。</span>
        </div>}
      </main>
    </div>
  </div>
}
