import assert from 'node:assert/strict'
import test from 'node:test'
import { latestContextUsage, formatUsageCost } from '../src/components/contextUsage.ts'

const estimate = (used, budget) => ({ type: 'context/usage', data: { estimated_input_tokens: used, input_budget: budget } })
const usage = (used, budget, data) => ({
  type: 'context/usage',
  data: { source: 'provider_usage', estimated_input_tokens: used, input_budget: budget, ...data },
})

test('uses latest main provider context and aggregates cache counts weighted by input', () => {
  const result = latestContextUsage([
    estimate(10_000, 100_000),
    usage(11_000, 100_000, { cached_tokens: 9500, cache_write_tokens: 1000, uncached_input_tokens: 500, output_tokens: 100, cache_hit_pct: 86.4 }),
    usage(1000, 9000, { cached_tokens: 100, output_tokens: 200, cache_hit_pct: 10 }),
    usage(3000, 30_000, { actor: 'virtual', cached_tokens: 2000, uncached_input_tokens: 1000, output_tokens: 50 }),
    estimate(6000, 9000),
    { type: 'assistant/message', data: { source: 'provider_usage' } },
  ])
  assert.equal(result.used, 1000)
  assert.equal(result.budget, 9000)
  assert.deepEqual(result.totals, {
    cacheHitPct: 100 * 11_600 / 15_000, cacheRead: 11_600, cacheWrite: 1000,
    uncachedInput: 2400, output: 350, costs: [], latestCost: undefined,
  })
})

test('totals price events by currency and never carries a prior price into an unpriced step', () => {
  const events = [
    usage(100, 1000, { cost_amount: 0.001, cost_currency: 'CNY' }),
    usage(150, 1000, { cost_amount: 0.002, cost_currency: 'CNY' }),
    usage(200, 1000, { cost_amount: 1.23, cost_currency: 'USD' }),
  ]
  const totals = latestContextUsage(events).totals
  assert.deepEqual(totals.costs.map(formatUsageCost), ['¥0.0030', '$1.23'])
  assert.equal(formatUsageCost(totals.latestCost), '$1.23')
  assert.equal(latestContextUsage([...events, usage(200, 1000, {})]).totals.latestCost, undefined)
  assert.equal(formatUsageCost({ amount: 0, currency: 'USD' }), '$0.0000')
})

test('estimates do not become billed usage and zero usage stays valid', () => {
  assert.equal(latestContextUsage([]), undefined)
  assert.deepEqual(latestContextUsage([estimate(17, 100)]), { used: 17, budget: 100 })
  const zero = latestContextUsage([usage(0, 100, { cost_amount: 0, cost_currency: 'CNY' })])
  assert.equal(zero.totals.cacheHitPct, 0)
  assert.equal(formatUsageCost(zero.totals.latestCost), '¥0.0000')
})
