export type VisualRecord = Record<string, unknown>
/** The full request remains available through explicit copy/export. */
export function foldImageData(value: unknown): unknown {
  if (typeof value === 'string' && value.startsWith('data:image/')) return `[图片数据已折叠，${value.length} 字符；实际发送体可复制核对]`
  if (Array.isArray(value)) return value.map(foldImageData)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, foldImageData(item)]))
  return value
}
