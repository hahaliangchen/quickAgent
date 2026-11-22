import { useMemo } from 'react'
import { IconChevronRightOutlineRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import type { AgentEvent } from '../api.ts'
import { latestPlan, type PlanStatus } from '../model.ts'
import styles from './PlanProgress.module.css'

const labels: Record<PlanStatus, string> = {
  pending: '待开始', in_progress: '进行中', done: '完成', blocked: '受阻',
}

export function PlanProgress({ events, variant = 'inline' }: { events: readonly AgentEvent[]; variant?: 'inline' | 'dock' }) {
  const plan = useMemo(() => latestPlan(events), [events])
  if (!plan || plan.steps.length === 0) return null
  const { steps, completed } = plan
  const active = steps.find(step => step.status === 'blocked') ?? steps.find(step => step.status === 'in_progress')
  return (
    <details className={variant === 'dock' ? `${styles.plan} ${styles.dock}` : styles.plan}>
      <summary className={styles.summary}>
        <IconChevronRightOutlineRegular className={styles.chevron} aria-hidden />
        <span className={styles.label}>任务计划</span>
        <span className={styles.count} aria-live="polite" aria-atomic="true">{completed} / {steps.length}</span>
        {active && <span className={styles.current} data-status={active.status}>{labels[active.status]}：{active.title}</span>}
        <progress className={styles.progress} aria-label="任务计划完成度" value={completed} max={steps.length} />
      </summary>
      <ol className={styles.steps}>
        {steps.map(step => <li key={step.id} className={styles.step}>
          <span className={styles.status} data-status={step.status}>{labels[step.status]}</span>
          <div className={styles.detail}>
            <div>{step.title}</div>
            {step.note && <div className={styles.note}>{step.note}</div>}
          </div>
        </li>)}
      </ol>
    </details>
  )
}
