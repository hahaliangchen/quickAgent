import { api } from './api.ts'
import { resultOf, type ToolResult } from './model.ts'

// Shared by transcript and flow details; bound both retained bytes and entry count.
const cache = new Map<string, { result: ToolResult; bytes: number }>()
const pending = new Map<string, Promise<ToolResult>>()
const maxBytes = 16 * 1024 * 1024
let cachedBytes = 0

export function loadToolDetail(taskId: string, seq: number, key: string, fullBytes: number): Promise<ToolResult> {
  const cached = cache.get(key)
  if (cached !== undefined) {
    cache.delete(key)
    cache.set(key, cached)
    return Promise.resolve(cached.result)
  }
  const existing = pending.get(key)
  if (existing !== undefined) return existing
  const request = api.event(taskId, seq).then(event => {
    if (event.type !== 'tool/result' || event.seq !== seq) throw new Error('Unexpected tool event')
    const result = resultOf(event)
    const bytes = Math.max(fullBytes, result.text.length * 2)
    if (bytes <= maxBytes) {
      cache.set(key, { result, bytes })
      cachedBytes += bytes
      for (const [oldest, entry] of cache) {
        if (cachedBytes <= maxBytes && cache.size <= 32) break
        cache.delete(oldest)
        cachedBytes -= entry.bytes
      }
    }
    return result
  }).finally(() => pending.delete(key))
  pending.set(key, request)
  return request
}
