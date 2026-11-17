/**
 * Levels the agent page picker may offer: everything the model declares, except `none`.
 * Backend/API/headless requests still allow explicit `none`, while the agent UI keeps it unselectable.
 */
export function availableEfforts(supported: readonly string[]): readonly string[] {
  return supported.filter(level => level !== '' && level.toLowerCase() !== 'none')
}

export function resolveEffort(value: string | null | undefined, supported: readonly string[], preferred?: string | null): string {
  const available = availableEfforts(supported)
  // A stored level the agent picker still offers is kept as it is; `none` is not selectable here,
  // so it falls through to the default below on the agent page.
  if (value && available.includes(value)) return value
  // The model's own default comes next, then the host's usual order.
  if (preferred && available.includes(preferred)) return preferred
  return available.includes('medium') ? 'medium' : (available.includes('high') ? 'high' : (available[0] ?? ''))
}
