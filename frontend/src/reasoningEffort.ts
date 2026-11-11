/**
 * Levels a picker may offer: everything the model declares, except `none`.
 * Thinking is never switched off (design guide §8): the host sends `none`/`minimal`
 * as `low`, so offering `none` would show a level other than the one on the wire.
 */
export function availableEfforts(supported: readonly string[]): readonly string[] {
  return supported.filter(level => level !== '' && level.toLowerCase() !== 'none')
}

export function resolveEffort(value: string | null | undefined, supported: readonly string[], preferred?: string | null): string {
  const available = availableEfforts(supported)
  // A stored level the model still offers is kept as it is. A stored `none` is not
  // offered any more, so it falls through to the default below and the request
  // sends a level the picker can show.
  if (value && available.includes(value)) return value
  // The model's own default comes next, then the host's usual order.
  if (preferred && available.includes(preferred)) return preferred
  return available.includes('medium') ? 'medium' : (available.includes('high') ? 'high' : (available[0] ?? ''))
}
