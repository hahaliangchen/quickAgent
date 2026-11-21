import type { AgentEvent } from '../api.ts'

interface RequestSize {
  readonly used: number
  readonly budget: number
}

type Currency = 'CNY' | 'USD'
interface Cost {
  readonly amount: number
  readonly currency: Currency
}

export interface ContextUsage extends RequestSize {
  readonly totals?: {
    readonly cacheHitPct: number
    readonly cacheRead: number
    readonly cacheWrite: number
    readonly uncachedInput: number
    readonly output: number
    readonly costs: readonly Cost[]
    readonly latestCost: Cost | undefined
  }
}

function nonnegative(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : undefined
}

export function latestContextUsage(events: readonly AgentEvent[]): ContextUsage | undefined {
  let actual: RequestSize | undefined
  let estimate: RequestSize | undefined
  let requests = 0
  let totalInput = 0
  let cacheRead = 0
  let cacheWrite = 0
  let uncachedInput = 0
  let output = 0
  const costs = new Map<Currency, number>()
  let latestCost: Cost | undefined

  for (const { type, data } of events) {
    if (type !== 'context/usage') continue
    const used = nonnegative(data.estimated_input_tokens)
    const budget = nonnegative(data.input_budget)
    const reported = data.source === 'provider_usage'
    // Virtual calls contribute to spend, but don't change the main context ring.
    if ((data.actor === undefined || data.actor === 'agent') && used !== undefined && budget !== undefined && budget > 0) {
      if (reported) actual = { used, budget }
      else estimate = { used, budget }
    }
    if (!reported) continue
    requests += 1
    const read = nonnegative(data.cached_tokens) ?? 0
    const write = nonnegative(data.cache_write_tokens) ?? 0
    const uncached = nonnegative(data.uncached_input_tokens)
    // Old Messages events could report the uncached input as their total.
    const input = uncached === undefined && used !== undefined && read > used ? used + read + write : used ?? 0
    totalInput += input
    cacheRead += read
    cacheWrite += write
    uncachedInput += uncached ?? Math.max(0, input - read - write)
    output += nonnegative(data.output_tokens) ?? 0
    const currency = data.cost_currency
    const amount = nonnegative(data.cost_amount)
    latestCost = undefined
    if (amount !== undefined && (currency === 'CNY' || currency === 'USD')) {
      latestCost = { currency, amount }
      costs.set(currency, (costs.get(currency) ?? 0) + amount)
    }
  }
  const size = actual ?? estimate
  if (!size) return undefined
  if (requests === 0) return size
  return { ...size, totals: {
    cacheHitPct: totalInput > 0 ? Math.min(100, 100 * cacheRead / totalInput) : 0,
    cacheRead, cacheWrite, uncachedInput, output,
    costs: Array.from(costs, ([currency, amount]) => ({ currency, amount })),
    latestCost,
  } }
}

export function formatUsageCost({ currency, amount }: Cost): string {
  return (currency === 'CNY' ? '¥' : '$') + amount.toFixed(amount < 0.1 ? 4 : 2)
}
