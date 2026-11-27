import { ParchmentQuillIcon } from '../../components/ParchmentQuillIcon.tsx'
import { buildMergeObserverSubmissionPrompt } from './playbookPrompts.ts'
import type { GraphSummary, ObserverSubmission, PlaybookSummary } from './types.ts'
import css from './style.module.css'

const statusLabel: Record<ObserverSubmission['status'], string> = {
  pending: '待处理',
  merged: '已合入蓝图',
  dismissed: '已忽略',
}

function formatTime(iso: string): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

export function ObserverInboxView({
  submission,
  existingPlaybooks,
  existingGraphs,
  busy,
  onMergeWithAi,
  onSaveAsPlaybook,
  onSetStatus,
  onDelete,
  onOpenSession,
  onJumpPlaybook,
}: {
  submission: ObserverSubmission
  existingPlaybooks: readonly PlaybookSummary[]
  existingGraphs: readonly GraphSummary[]
  busy: boolean
  onMergeWithAi?: ((prompt: string) => void) | undefined
  onSaveAsPlaybook: () => void
  onSetStatus: (status: ObserverSubmission['status']) => void
  onDelete: () => void
  onOpenSession?: ((taskId: string) => void) | undefined
  onJumpPlaybook: (playbookId: string) => void
}) {
  return (
    <div className={css.playbookPage} data-testid="blueprint-observer-inbox-view">
      <header className={css.playbookHeader}>
        <div className={css.playbookTitleBlock}>
          <div className={css.playbookBadges}>
            <span className={css.inboxSourceBadge}>
              <ParchmentQuillIcon size={13} variant="line" />
              <span>观察者战地呈报</span>
            </span>
            <span className={css.inboxStatusBadge} data-status={submission.status}>
              {statusLabel[submission.status]}
            </span>
            {submission.created_at && (
              <span className={css.playbookId}>{formatTime(submission.created_at)}</span>
            )}
          </div>
          <h1>{submission.title}</h1>
          {submission.trigger && (
            <p className={css.playbookTrigger}>适用场景：{submission.trigger}</p>
          )}
          {submission.summary && (
            <p className={css.playbookSummary}>{submission.summary}</p>
          )}
        </div>

        <div className={css.playbookActions}>
          <button
            type="button"
            className={css.primary}
            disabled={busy || !onMergeWithAi}
            onClick={() =>
              onMergeWithAi?.(
                buildMergeObserverSubmissionPrompt(submission, existingPlaybooks, existingGraphs),
              )
            }
            title="发送给蓝图 AI 核实源码后自动整理合入画布或最短路径"
          >
            交由蓝图 AI 整理合入
          </button>
          <button
            type="button"
            className={css.subtleButton}
            disabled={busy}
            onClick={onSaveAsPlaybook}
            title="无需等待 AI，直接基于观察者呈报一键建为最短路径蓝图"
          >
            直接存为最短路径
          </button>
          {submission.status === 'pending' ? (
            <button
              type="button"
              className={css.subtleButton}
              disabled={busy}
              onClick={() => onSetStatus('dismissed')}
            >
              忽略
            </button>
          ) : (
            <button
              type="button"
              className={css.subtleButton}
              disabled={busy}
              onClick={() => onSetStatus('pending')}
            >
              恢复待处理
            </button>
          )}
          <button
            type="button"
            className={`${css.subtleButton} ${css.warning}`}
            disabled={busy}
            onClick={onDelete}
          >
            删除
          </button>
        </div>
      </header>

      <div className={css.inboxOriginStrip}>
        <span className={css.inboxOriginLabel}>战场来源</span>
        {onOpenSession ? (
          <button
            type="button"
            className={css.linkChip}
            onClick={() => onOpenSession(submission.source_task_id)}
            title="跳回产生这条战地经验的原始主会话"
          >
            {submission.source_task_title || submission.source_task_id.slice(0, 14)} ↗
          </button>
        ) : (
          <span className={css.metaChip}>
            {submission.source_task_title || submission.source_task_id}
          </span>
        )}
        {submission.merged_playbook_id && (
          <button
            type="button"
            className={css.linkChip}
            onClick={() => onJumpPlaybook(submission.merged_playbook_id!)}
          >
            已合入最短路径：{submission.merged_playbook_id} →
          </button>
        )}
      </div>

      <div className={css.playbookGrid}>
        <section className={css.playbookCard}>
          <h2>涉及锚点文件</h2>
          {submission.anchor_files.length ? (
            <div className={css.chipList}>
              {submission.anchor_files.map(file => (
                <span key={file} className={css.anchorChip} title={file}>
                  <code>{file}</code>
                </span>
              ))}
            </div>
          ) : (
            <p className={css.playbookEmptyHint}>未指定锚点文件（合入时由蓝图 AI 核实补齐）</p>
          )}
        </section>

        <section className={css.playbookCard}>
          <h2>环境约束与隔离约定</h2>
          {submission.environment.length ? (
            <ul className={css.playbookList}>
              {submission.environment.map((item, idx) => (
                <li key={idx}>{item}</li>
              ))}
            </ul>
          ) : (
            <p className={css.playbookEmptyHint}>暂无环境约束记录</p>
          )}
        </section>

        <section className={css.playbookCard}>
          <h2>避坑提示与红线</h2>
          {submission.pitfalls.length ? (
            <ul className={css.playbookList}>
              {submission.pitfalls.map((item, idx) => (
                <li key={idx}>{item}</li>
              ))}
            </ul>
          ) : (
            <p className={css.playbookEmptyHint}>暂无避坑记录</p>
          )}
        </section>

        <section className={css.playbookCard}>
          <h2>有效验收命令</h2>
          {submission.verify_commands.length ? (
            <ul className={css.playbookCommandList}>
              {submission.verify_commands.map((cmd, idx) => (
                <li key={idx}>
                  <code>{cmd}</code>
                </li>
              ))}
            </ul>
          ) : (
            <p className={css.playbookEmptyHint}>暂无验收命令</p>
          )}
        </section>
      </div>
    </div>
  )
}
