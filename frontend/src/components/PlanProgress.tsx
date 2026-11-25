import { useId, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { IconChevronDownOutlineRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import type { AgentEvent } from '../api.ts'
import { groupPlanSteps, latestPlan, type PlanStatus } from '../model.ts'
import styles from './PlanProgress.module.css'

const labels: Record<PlanStatus, string> = {
  pending: '待开始',
  in_progress: '进行中',
  done: '完成',
  blocked: '受阻',
}

const RING_RADIUS = 10
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS

/** Crisp circular SVG progress indicator with centered percentage or completion check. */
function ProgressRing({ percent, done, blocked }: {
  readonly percent: number
  readonly done: boolean
  readonly blocked: boolean
}): ReactNode {
  const gradId = useId()
  const clamped = Math.max(0, Math.min(100, percent))
  const offset = RING_CIRCUMFERENCE * (1 - clamped / 100)
  return (
    <span
      className={styles.ring}
      data-done={done ? 'true' : undefined}
      data-blocked={blocked ? 'true' : undefined}
      aria-hidden="true"
    >
      <svg className={styles.ringSvg} viewBox="0 0 24 24" fill="none">
        <defs>
          <linearGradient id={gradId} x1="0%" y1="0%" x2="100%" y2="100%">
            <stop offset="0%" stopColor="var(--plan-grad-start)" />
            <stop offset="55%" stopColor="var(--plan-grad-mid)" />
            <stop offset="100%" stopColor="var(--plan-grad-end)" />
          </linearGradient>
        </defs>
        <circle className={styles.ringTrack} cx="12" cy="12" r={RING_RADIUS} />
        <circle
          className={styles.ringArc}
          cx="12"
          cy="12"
          r={RING_RADIUS}
          stroke={blocked ? undefined : 'url(#' + gradId + ')'}
          strokeDasharray={RING_CIRCUMFERENCE}
          strokeDashoffset={offset}
        />
      </svg>
      {done
        ? (
            <svg className={styles.ringCheck} viewBox="0 0 12 12" fill="none">
              <path d="M2.6 6.2 4.9 8.5 9.4 3.6" />
            </svg>
          )
        : <span className={styles.ringPct}>{clamped}%</span>}
    </span>
  )
}

/** Step status node sitting on the left vertical timeline rail (matte, no glow). */
function StepGlyph({ status, stale }: { status: PlanStatus; stale?: boolean }): ReactNode {
  if (status === 'done') {
    return (
      <svg className={styles.doneIcon} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <circle cx="8" cy="8" r="7" className={styles.doneDisc} />
        <path className={styles.doneTick} d="M4.8 8.2 6.9 10.3 11.3 5.7" />
      </svg>
    )
  }
  if (status === 'in_progress') {
    return (
      <svg
        className={styles.activeIcon}
        data-stale={stale ? 'true' : undefined}
        viewBox="0 0 16 16"
        fill="none"
        aria-hidden="true"
      >
        <circle cx="8" cy="8" r="6" className={styles.activeTrack} />
        <circle cx="8" cy="8" r="6" className={styles.activeArc} />
      </svg>
    )
  }
  if (status === 'blocked') {
    return (
      <span className={styles.blockedNode} aria-hidden="true">
        <span className={styles.blockedCore} />
      </span>
    )
  }
  return <span className={styles.pendingNode} aria-hidden="true" />
}

export function PlanProgress({
  events,
  variant = 'inline',
  running = true,
  taskStatus,
}: {
  events: readonly AgentEvent[]
  variant?: 'inline' | 'dock'
  running?: boolean | undefined
  taskStatus?: string | undefined
}) {
  const plan = useMemo(() => latestPlan(events), [events])
  const tree = useMemo(() => groupPlanSteps(plan?.steps ?? []), [plan])
  // Running and blocked phases show their sub-steps by default; a finished or
  // untouched phase stays folded until the user clicks. Only a click is stored,
  // so a later status change still follows the default.
  const [folded, setFolded] = useState<Record<string, boolean>>({})
  // The sub-step that best explains the current state, shown without expanding.
  const focus = useMemo(() => {
    for (const group of tree.topSteps) {
      const child = group.children.find(step => step.status === 'blocked')
        ?? group.children.find(step => step.status === 'in_progress')
      if (child !== undefined) return { group, child }
    }
    return undefined
  }, [tree])
  if (!plan || plan.steps.length === 0) return null

  const { topSteps, topCompleted, topTotal, percent } = tree
  const blockedTop = topSteps.find(step => step.status === 'blocked')
  const runningTop = topSteps.find(step => step.status === 'in_progress')
  const status: PlanStatus | undefined = focus !== undefined
    ? focus.child.status
    : blockedTop !== undefined ? 'blocked' : runningTop !== undefined ? 'in_progress' : undefined
  const headline = focus?.child.title ?? blockedTop?.title ?? runningTop?.title
  const parentLine = focus?.group.title
  const allDone = topTotal > 0 && topCompleted === topTotal
  const useSegments = topTotal <= 16
  const staleInProgress = !running
  const staleLabel = taskStatus === 'cancelled' || taskStatus === 'interrupted' || taskStatus === 'failed'
    ? '已中断'
    : '未更新'
  const statusLabel = (s: PlanStatus) => (s === 'in_progress' && staleInProgress ? staleLabel : labels[s])

  return (
    <details className={variant === 'dock' ? styles.plan + ' ' + styles.dock : styles.plan}>
      <summary className={styles.summary}>
        <div className={styles.headerRow}>
          <div className={styles.headerLeft}>
            <ProgressRing percent={percent} done={allDone} blocked={blockedTop !== undefined || focus?.child.status === 'blocked'} />
            <span className={styles.label}>任务计划</span>
            <span className={styles.count} aria-live="polite" aria-atomic="true">
              {topCompleted} / {topTotal}
            </span>
          </div>

          <div className={styles.headerRight}>
            {headline !== undefined
              ? (
                  <span
                    className={styles.collapsedStatus}
                    data-status={status}
                    data-stale={status === 'in_progress' && staleInProgress ? 'true' : undefined}
                  >
                    <span className={styles.statusDot} aria-hidden="true" />
                    {parentLine !== undefined && <span className={styles.statusParent}>{parentLine} ›</span>}
                    <span className={styles.statusTitle}>{headline}</span>
                  </span>
                )
              : allDone
                ? <span className={styles.collapsedDone}>已全部完成</span>
                : null}
            <span className={styles.expandedPct} aria-hidden="true">{percent}%</span>
            <span className={styles.chevronBtn} aria-hidden="true">
              <IconChevronDownOutlineRegular className={styles.chevron} />
            </span>
          </div>
        </div>

        <div
          className={styles.progressBar}
          role="progressbar"
          aria-label="任务计划完成度"
          aria-valuenow={topCompleted}
          aria-valuemin={0}
          aria-valuemax={topTotal}
        >
          {useSegments
            ? topSteps.map(group => {
                const doneChildren = group.children.filter(child => child.status === 'done').length
                // A running phase with sub-steps fills its segment by sub-step ratio.
                const partial = group.status === 'in_progress' && group.children.length > 0
                return (
                  <span
                    key={group.id}
                    className={styles.segment}
                    data-status={group.status}
                    data-partial={partial ? 'true' : undefined}
                  >
                    {partial && (
                      <span
                        className={styles.segmentFill}
                        style={{ width: Math.round((doneChildren / group.children.length) * 100) + '%' }}
                      />
                    )}
                  </span>
                )
              })
            : (
                <span className={styles.continuousTrack}>
                  <span className={styles.continuousFill} style={{ width: percent + '%' }} />
                </span>
              )}
        </div>
      </summary>

      <ol className={styles.steps}>
        {topSteps.map(group => {
          const highlighted = group.status === 'in_progress' || group.status === 'blocked'
          const stepStale = group.status === 'in_progress' && staleInProgress
          const doneChildren = group.children.filter(child => child.status === 'done').length
          const open = group.children.length > 0 && (folded[group.id] ?? highlighted)
          return (
            <li key={group.id} className={styles.step} data-status={group.status}>
              <div className={styles.railCell}>
                <StepGlyph status={group.status} stale={stepStale} />
                <span className={styles.railStem} aria-hidden="true" />
              </div>
              <div
                className={highlighted ? styles.activeCard : styles.plainRow}
                data-status={group.status}
                data-stale={stepStale ? 'true' : undefined}
              >
                <div className={styles.stepMain}>
                  <div className={styles.title}>{group.title}</div>
                  {group.note && <div className={styles.note}>{group.note}</div>}
                </div>
                <div className={styles.stepMeta}>
                  {group.children.length > 0 && (
                    <button
                      type="button"
                      className={styles.subToggle}
                      aria-expanded={open}
                      aria-label={(open ? '收起 ' : '展开 ') + group.title + ' 的子步骤'}
                      onClick={() => setFolded(previous => ({ ...previous, [group.id]: !(previous[group.id] ?? highlighted) }))}
                    >
                      <span className={styles.subCount} data-status={group.status}>
                        {doneChildren}/{group.children.length}
                      </span>
                      <IconChevronDownOutlineRegular className={open ? styles.subChevronOpen : styles.subChevron} />
                    </button>
                  )}
                  {highlighted && (
                    <span
                      className={styles.badge}
                      data-status={group.status}
                      data-stale={stepStale ? 'true' : undefined}
                    >
                      {statusLabel(group.status)}
                    </span>
                  )}
                </div>
              </div>
              {open && (
                <ol className={styles.subSteps}>
                  {group.children.map(child => (
                    <li key={child.id} className={styles.subStep} data-status={child.status}>
                      <span className={styles.subDot} aria-hidden="true" />
                      <span className={styles.subBody}>
                        <span className={styles.subTitle}>{child.title}</span>
                        {child.note && <span className={styles.subNote}>{child.note}</span>}
                      </span>
                    </li>
                  ))}
                </ol>
              )}
            </li>
          )
        })}
      </ol>
    </details>
  )
}