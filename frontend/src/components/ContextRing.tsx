import { Tooltip } from '@deepseek-ai/dsh-client-ui-primitives'
import { t } from '../i18n.ts'
import { formatUsageCost, type ContextUsage } from './contextUsage.ts'

const thousands = (value: number) => value >= 10_000 ? `${Math.round(value / 1000)}k` : value >= 1000 ? `${(value / 1000).toFixed(1)}k` : String(value)
const tokenCount = (value: number) => t('message.turnUsage.count', { count: value.toLocaleString() })

/** Context fullness plus session-wide cache and billing telemetry. */
export function ContextRing({ usage }: { usage: ContextUsage | undefined }) {
  if (!usage) return null
  const ratio = usage.used / usage.budget
  const tone = ratio >= 0.85 ? 'var(--theme-ring-high)' : ratio >= 0.6 ? 'var(--theme-ring-mid)' : 'var(--theme-ring-low)'
  const radius = 7
  const circumference = 2 * Math.PI * radius
  const percent = Math.round(ratio * 100)
  const label = [ratio >= 0.85
    ? `上下文快满了：已用 ${thousands(usage.used)} / ${thousands(usage.budget)} tokens（${percent}%）`
    : `上下文已用 ${thousands(usage.used)} / ${thousands(usage.budget)} tokens（${percent}%）`]
  const { totals } = usage
  const hit = totals?.cacheHitPct.toFixed(1)
  const cost = totals?.costs.map(formatUsageCost).join(' / ')
  if (totals !== undefined) {
    label.push(t('stats.cacheHit', { percent: hit ?? 0 }))
    label.push(`${t('message.turnUsage.cacheRead')} ${tokenCount(totals.cacheRead)}`)
    label.push(`${t('message.turnUsage.cacheWrite')} ${tokenCount(totals.cacheWrite)}`)
    label.push(`${t('message.turnUsage.input')} ${tokenCount(totals.uncachedInput)}`)
    label.push(`${t('message.turnUsage.output')} ${tokenCount(totals.output)}`)
    if (cost) label.push(t('message.turnUsage.sessionCost', { cost }))
    if (totals.latestCost) label.push(t('message.turnUsage.latestCost', { cost: formatUsageCost(totals.latestCost) }))
  }
  const description = label.join(' · ')
  return <Tooltip label={description} side="top" delayMs={200} maxWidth={320}>
    <span role="img" aria-label={description} tabIndex={0} style={{ display: 'inline-flex', alignItems: 'center', gap: 4, fontSize: 11.5, fontVariantNumeric: 'tabular-nums', color: 'var(--codex-text-muted, #6b7280)' }}>
      <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true" style={{ flexShrink: 0 }}>
        <circle cx="9" cy="9" r={radius} fill="none" stroke="var(--codex-card-border, #e5e7eb)" strokeWidth="2.2" />
        <circle cx="9" cy="9" r={radius} fill="none" style={{ stroke: tone }} strokeWidth="2.2" strokeLinecap="round"
          strokeDasharray={`${circumference * Math.min(1, ratio)} ${circumference}`} transform="rotate(-90 9 9)" />
      </svg>
      {ratio >= 0.85 && <span style={{ color: tone }}>{percent}%</span>}
      {hit !== undefined && <span style={{ whiteSpace: 'nowrap' }}>{t('stats.cacheHit', { percent: hit })}</span>}
      {cost && <span>{cost}</span>}
    </span>
  </Tooltip>
}
