import { useMemo } from 'react'
import type { ReactNode } from 'react'
import { IconChevronRightOutlineRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import type { AgentEvent } from '../api.ts'
import { latestPlan, type PlanStatus } from '../model.ts'
import styles from './PlanProgress.module.css'

const labels: Record<PlanStatus, string> = {
  pending: '待开始',
  in_progress: '进行中',
  done: '完成',
  blocked: '受阻',
}

/**
 * One state glyph per step. The word 完成 repeated on six rows turned the list
 * into a column of identical text; a filled check disc says the same thing in
 * six pixels and lets the eye land on the step that is still running.
 */
function StepGlyph({ status }: { status: PlanStatus }): ReactNode {
  if (status === 'done') {
    return (
      <svg className={styles.mark} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <circle cx="8" cy="8" r="7" className={styles.disc} />
        <path className={styles.tick} d="M4.7 8.15 6.85 10.3 11.35 5.6" />
      </svg>
    )
  }
  return <span className={styles.glyph} data-status={status} aria-hidden="true" />
}

/** Compact summary line: the header answers "how far" and "what now" at a glance. */
function SummaryState({ blocked, running, settled }: {
  readonly blocked: { title: string } | undefined
  readonly running: { title: string } | undefined
  readonly settled: boolean
}): ReactNode {
  const head = blocked ?? running
  if (head !== undefined) {
    const status: PlanStatus = blocked === undefined ? 'in_progress' : 'blocked'
    return (
      <span className={styles.state} data-status={status}>
        <span className={styles.dot} aria-hidden="true" />
        <span className={styles.stateText}>{labels[status]}：{head.title}</span>
      </span>
    )
  }
  if (!settled) return null
  return (
    <span className={styles.settled}>
      <svg className={styles.settledMark} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <path d="M3.4 8.4 6.6 11.6 12.8 4.9" />
      </svg>
      已全部完成
    </span>
  )
}

export function PlanProgress({ events, variant = 'inline' }: { events: readonly AgentEvent[]; variant?: 'inline' | 'dock' }) {
  const plan = useMemo(() => latestPlan(events), [events])
  if (!plan || plan.steps.length === 0) return null
  const { steps, completed } = plan
  const blocked = steps.find(step => step.status === 'blocked')
  const running = steps.find(step => step.status === 'in_progress')
  return (
    <details className={variant === 'dock' ? `${styles.plan} ${styles.dock}` : styles.plan}>
      <summary className={styles.summary}>
        <IconChevronRightOutlineRegular className={styles.chevron} aria-hidden />
        <span className={styles.label}>任务计划</span>
        <span className={styles.count} aria-live="polite" aria-atomic="true">{completed} / {steps.length}</span>
        <SummaryState blocked={blocked} running={running} settled={completed === steps.length} />
        <progress className={styles.progress} aria-label="任务计划完成度" value={completed} max={steps.length} />
      </summary>
      <ol className={styles.steps}>
        {steps.map(step => (
          <li key={step.id} className={styles.step} data-status={step.status}>
            <StepGlyph status={step.status} />
            <div className={styles.detail}>
              <div className={styles.title}>{step.title}</div>
              {step.note && <div className={styles.note}>{step.note}</div>}
            </div>
            {/* Waiting steps keep the empty ring alone: a gray 待开始 chip beside
                a gray ring was the faintest thing on the row. */}
            {(step.status === 'in_progress' || step.status === 'blocked') && (
              <span className={styles.tag} data-status={step.status}>{labels[step.status]}</span>
            )}
          </li>
        ))}
      </ol>
    </details>
  )
}
