import { useEffect, useRef, useState, type FormEvent } from 'react'
import { type Auth, type Details, type Server, type SshError, type Tier, message, ssh } from './api.ts'
import css from './style.module.css'

export default function ServerModal({ server, keyError, onSaved, onClose }: { server?: Server | undefined; keyError?: SshError | undefined; onSaved: (server: Server) => void; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [name, setName] = useState(server?.name ?? '')
  const [host, setHost] = useState(server?.host ?? '')
  const [port, setPort] = useState(server?.port ?? 22)
  const [username, setUsername] = useState(server?.username ?? '')
  const [auth, setAuth] = useState<Auth>(server?.auth_type ?? 'agent')
  const [secret, setSecret] = useState('')
  const [clear, setClear] = useState(false)
  const [path, setPath] = useState('')
  const [tier, setTier] = useState<Tier>(server?.env_tier ?? 'dev')
  const [approval, setApproval] = useState(server?.require_approval ?? false)
  const [directory, setDirectory] = useState(server?.default_dir ?? '')
  const [tags, setTags] = useState(server?.tags.join(', ') ?? '')
  const [notes, setNotes] = useState(server?.notes ?? '')
  const [error, setError] = useState('')
  const [saving, setSaving] = useState(false)
  useEffect(() => { const element = dialog.current; element?.showModal(); return () => element?.close() }, [])

  async function save(event: FormEvent) {
    event.preventDefault(); setSaving(true); setError('')
    const details: Details = { name, env_tier: tier, require_approval: tier === 'prod' || approval, default_dir: directory.trim() || null, tags: tags.split(',').map(tag => tag.trim()).filter(Boolean), notes: notes.trim() || null }
    try {
      if (server) {
        if (auth !== 'agent' && (secret || clear)) await ssh.credential(server.id, clear ? null : secret)
        onSaved(await ssh.update(server.id, details))
      } else {
        onSaved(await ssh.create({ ...details, host, port, username, auth_type: auth, ...(auth === 'key_file' ? { key_path: path } : {}), ...(auth !== 'agent' && secret ? { credential: secret } : {}) }))
      }
    } catch (error) { setError(message(error)) } finally { setSaving(false) }
  }
  async function trust() {
    if (!server || !keyError?.keyChanged || !window.confirm('请通过独立渠道核对新指纹。要替换已信任的主机指纹吗？')) return
    setSaving(true); setError('')
    try { await ssh.trust(server.id, keyError.detail); onSaved(await ssh.test(server.id)) } catch (error) { setError(message(error)) } finally { setSaving(false) }
  }
  return <dialog ref={dialog} className={css.dialog} onCancel={onClose} aria-labelledby="ssh-server-dialog-title">
    <header className={css.panelHeader}><h2 id="ssh-server-dialog-title">{server ? '服务器设置' : '添加服务器'}</h2><button type="button" onClick={onClose} aria-label="关闭服务器表单">×</button></header>
    <form onSubmit={event => void save(event)} className={css.form}>
      <label>名称<input required autoFocus value={name} onChange={event => setName(event.target.value)} /></label>
      <label>主机<input required value={host} disabled={!!server} onChange={event => setHost(event.target.value)} /></label>
      <label>端口<input required type="number" min={1} max={65535} value={port} disabled={!!server} onChange={event => setPort(event.target.valueAsNumber || 0)} /></label>
      <label>用户名<input required autoComplete="off" value={username} disabled={!!server} onChange={event => setUsername(event.target.value)} /></label>
      <label>认证方式<select value={auth} disabled={!!server} onChange={event => { setAuth(event.target.value as Auth); setSecret('') }}><option value="agent">SSH Agent</option><option value="key_file">私钥文件</option><option value="password">密码</option></select></label>
      {auth === 'key_file' && !server && <label>私钥绝对路径<input required value={path} onChange={event => setPath(event.target.value)} /></label>}
      {auth !== 'agent' && <label>{auth === 'password' ? '密码' : '私钥口令'}<input type="password" autoComplete="new-password" required={!server && auth === 'password'} value={secret} disabled={clear} placeholder={server?.has_password || server?.has_passphrase ? '已保存' : ''} onChange={event => setSecret(event.target.value)} /></label>}
      {server && (server.has_password || server.has_passphrase) && <label className={css.check}><input type="checkbox" checked={clear} onChange={event => setClear(event.target.checked)} />清除已保存凭据</label>}
      <label>环境<select value={tier} onChange={event => setTier(event.target.value as Tier)}><option value="dev">Dev</option><option value="staging">Staging</option><option value="prod">Prod</option></select></label>
      <label className={css.check}><input type="checkbox" checked={tier === 'prod' || approval} disabled={tier === 'prod'} onChange={event => setApproval(event.target.checked)} />终端与写入操作需确认</label>
      <label>默认目录<input value={directory} onChange={event => setDirectory(event.target.value)} placeholder="." /></label>
      <label>标签（逗号分隔）<input value={tags} onChange={event => setTags(event.target.value)} /></label>
      <label className={css.wide}>备注<textarea value={notes} rows={2} onChange={event => setNotes(event.target.value)} /></label>
      {server && <div className={css.wide}><span>主机指纹</span><code>{server.host_key_fingerprint ?? '未建立信任'}</code></div>}
      {keyError?.keyChanged && <div className={css.keyWarning}><strong>主机指纹发生变化</strong><code>{keyError.detail.trusted}</code><code>{keyError.detail.received}</code><button type="button" disabled={saving} onClick={() => void trust()}>信任新指纹</button></div>}
      {error && <p className={css.error} role="alert">{error}</p>}
      <footer className={css.dialogActions}><button type="button" onClick={onClose}>取消</button><button type="submit" className={css.primary} disabled={saving}>{saving ? '保存中…' : '保存'}</button></footer>
    </form>
  </dialog>
}
