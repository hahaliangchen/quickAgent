import { useId, useMemo } from 'react'
import type { ReactNode } from 'react'
import { IconChevronDownOutlineRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import type { AgentEvent } from '../api.ts'
import { latestPlan, type PlanStatus } from '../model.ts'
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
          stroke={blocked ? undefined : `url(#${gradId})`}
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
function StepGlyph({ status }: { status: PlanStatus }): ReactNode {
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
      <svg className={styles.activeIcon} viewBox="0 0 16 16" fill="none" aria-hidden="true">
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

export function PlanProgress({ events, variant = 'inline' }: { events: readonly AgentEvent[]; variant?: 'inline' | 'dock' }) {
  const plan = useMemo(() => latestPlan(events), [events])
  if (!plan || plan.steps.length === 0) return null
  const { steps, completed } = plan
  const total = steps.length
  const percent = Math.round((completed / total) * 100)
  const blocked = steps.find(step => step.status === 'blocked')
  const running = steps.find(step => step.status === 'in_progress')
  const head = blocked ?? running
  const allDone = completed === total
  const useSegments = total <= 16

  return (
    <details className={variant === 'dock' ? `${styles.plan} ${styles.dock}` : styles.plan}>
      <summary className={styles.summary}>
        <div className={styles.headerRow}>
          <div className={styles.headerLeft}>
            <ProgressRing percent={percent} done={allDone} blocked={blocked !== undefined} />
            <span className={styles.label}>任务计划</span>
            <span className={styles.count} aria-live="polite" aria-atomic="true">
              {completed} / {total}
            </span>
          </div>

          <div className={styles.headerRight}>
            {head !== undefined
              ? (
                  <span
                    className={styles.collapsedStatus}
                    data-status={blocked !== undefined ? 'blocked' : 'in_progress'}
                  >
                    <span className={styles.statusDot} aria-hidden="true" />
                    <span className={styles.statusTitle}>{head.title}</span>
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
          aria-valuenow={completed}
          aria-valuemin={0}
          aria-valuemax={total}
        >
          {useSegments
            ? steps.map((step, index) => (
                <span
                  key={step.id}
                  className={styles.segment}
                  data-status={step.status}
                  style={
                    step.status === 'done' || step.status === 'in_progress'
                      ? {
                          backgroundSize: `${total * 100}% 100%`,
                          backgroundPosition: total > 1 ? `${(index / (total - 1)) * 100}% 0` : '0% 0',
                        }
                      : undefined
                  }
                />
              ))
            : (
                <span className={styles.continuousTrack}>
                  <span className={styles.continuousFill} style={{ width: `${percent}%` }} />
                </span>
              )}
        </div>
      </summary>

      <ol className={styles.steps}>
        {steps.map(step => {
          const highlighted = step.status === 'in_progress' || step.status === 'blocked'
          return (
            <li key={step.id} className={styles.step} data-status={step.status}>
              <div className={styles.railCell}>
                <StepGlyph status={step.status} />
                <span className={styles.railStem} aria-hidden="true" />
              </div>
              <div className={highlighted ? styles.activeCard : styles.plainRow} data-status={step.status}>
                <div className={styles.stepMain}>
                  <div className={styles.title}>{step.title}</div>
                  {step.note && <div className={styles.note}>{step.note}</div>}
                </div>
                {highlighted && (
                  <span className={styles.badge} data-status={step.status}>
                    {labels[step.status]}
                  </span>
                )}
              </div>
            </li>
          )
        })}
      </ol>
    </details>
  )
}
