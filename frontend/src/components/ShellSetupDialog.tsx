import { useEffect, useRef, useState } from 'react'
import { Modal } from '../dsh/ui-primitives/Modal.tsx'
import { shellSetupApi, type ShellInstallProgress, type ShellStatus } from '../shellSetup.ts'
import css from './ShellSetupDialog.module.css'

const message = (error: unknown) => error instanceof Error ? error.message : String(error)

/** Dependency setup for code sessions only; opening the dialog never starts an installation. */
export function ShellSetupDialog({ enabled, scope, onReady }: {
  enabled: boolean
  scope: string
  onReady?: (() => void) | undefined
}) {
  const [shell, setShell] = useState<ShellStatus>()
  const [progress, setProgress] = useState<ShellInstallProgress>()
  const [dismissed, setDismissed] = useState(false)
  const [manual, setManual] = useState(false)
  const [path, setPath] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const onReadyRef = useRef(onReady)
  onReadyRef.current = onReady
  const installing = progress?.running === true

  useEffect(() => {
    if (!enabled) return
    const controller = new AbortController()
    setDismissed(false)
    setShell(undefined)
    setError('')
    void shellSetupApi.status(controller.signal).then(async status => {
      if (controller.signal.aborted) return
      setShell(status)
      if (!status.available && status.pwsh_detected !== true && status.platform.os === 'windows') {
        const current = await shellSetupApi.progress(controller.signal)
        if (!controller.signal.aborted) setProgress(current)
      }
    }).catch(cause => { if (!controller.signal.aborted) setError(message(cause)) })
    return () => { controller.abort() }
  }, [enabled, scope])

  useEffect(() => {
    if (!enabled || !installing) return
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      try {
        const current = await shellSetupApi.progress(controller.signal)
        if (controller.signal.aborted) return
        if (!current.running) {
          // Verify the installation, including Store aliases, without restarting the app.
          const detected = await shellSetupApi.detect()
          if (controller.signal.aborted) return
          setShell(detected)
          if (detected.available) onReadyRef.current?.()
        }
        setProgress(current)
        setError(current.error ?? '')
        if (current.running) timer = setTimeout(() => { void poll() }, 750)
      } catch (cause) {
        if (controller.signal.aborted) return
        setError(message(cause))
        timer = setTimeout(() => { void poll() }, 1500)
      }
    }
    void poll()
    return () => { controller.abort(); clearTimeout(timer) }
  }, [enabled, installing])

  async function detect(manualPath?: string) {
    setBusy(true); setError('')
    try {
      const detected = await shellSetupApi.detect(manualPath)
      setShell(detected)
      if (detected.available) onReadyRef.current?.()
      else setError(detected.error ?? '仍未检测到 PowerShell 7，请确认安装完成或指定 pwsh.exe。')
    } catch (cause) { setError(message(cause)) }
    finally { setBusy(false) }
  }

  async function install() {
    setBusy(true); setError('')
    try {
      const current = await shellSetupApi.install()
      setProgress(current)
      if (!current.running) {
        if (current.error) setError(current.error)
        else await detect()
      }
    } catch (cause) { setError(message(cause)) }
    finally { setBusy(false) }
  }

  const open = enabled && !dismissed && shell?.platform.os === 'windows' && !shell.available && shell.pwsh_detected !== true
  const locked = busy || installing
  return <Modal open={open} title="代码任务需要 PowerShell 7" closeLabel="暂不安装" onClose={() => setDismissed(true)} className={css.dialog!}>
    <p className={css.description}>安装后可运行命令、启动服务和验证代码。现在仍可使用文件读取与编辑工具。</p>
    <p className={css.hint}>一键安装将通过 Microsoft Store 安装 PowerShell 7。安装前请查看命令：</p>
    <code className={css.command}>{shell?.install?.command ?? 'winget install --id 9MZ1SNWT0N5D --source msstore'}</code>
    <div className={css.actions}>
      <button type="button" className={css.primary} disabled={locked} onClick={() => { void install() }}>{installing ? '正在安装…' : '一键安装'}</button>
      <button type="button" className={css.secondary} disabled={busy} onClick={() => { void shellSetupApi.openStore().catch(cause => setError(message(cause))) }}>打开商店页面</button>
      <button type="button" className={css.secondary} disabled={locked} onClick={() => { setManual(true); void detect() }}>我已安装 / 手动指定路径</button>
    </div>
    {manual && <form className={css.manual} onSubmit={event => { event.preventDefault(); void detect(path.trim()) }}>
      <label htmlFor="shell-pwsh-path">PowerShell 7 的 pwsh.exe 路径</label>
      <input id="shell-pwsh-path" value={path} onChange={event => setPath(event.target.value)} disabled={locked} placeholder="C:/Program Files/PowerShell/7/pwsh.exe" />
      <div className={css.manualFooter}>
        <small>留空恢复自动查找；支持商店版，无需重启应用。</small>
        <button type="submit" className={css.secondary} disabled={locked}>{busy ? '检测中…' : '保存并重新检测'}</button>
      </div>
    </form>}
    {progress && progress.status !== 'idle' && <div className={css.progress}>
      <p role="status" aria-live="polite">{installing ? '正在执行安装命令…' : `安装命令已结束${progress.exit_code == null ? '' : `，退出码 ${progress.exit_code}`}`}</p>
      {(progress.stdout || progress.stderr) && <pre aria-label="安装命令输出">{[progress.stdout, progress.stderr].filter(Boolean).join('\n')}</pre>}
      {installing && <button type="button" className={css.secondary} disabled={busy} onClick={() => {
        setBusy(true)
        void shellSetupApi.cancelInstall().then(setProgress, cause => setError(message(cause))).finally(() => setBusy(false))
      }}>取消安装</button>}
      {!installing && <button type="button" className={css.secondary} disabled={busy} onClick={() => { void detect() }}>重新检测</button>}
    </div>}
    {(error || shell?.error) && <p className={css.error} role="alert">{error || shell?.error}</p>}
    <button type="button" className={css.skip} onClick={() => setDismissed(true)}>暂不安装，继续使用文件工具</button>
  </Modal>
}
