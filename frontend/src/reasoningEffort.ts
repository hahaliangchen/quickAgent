export function availableEfforts(supported: readonly string[]): readonly string[] {
  return supported.filter(level => level !== '' && level !== 'none')
}

export function resolveEffort(value: string | null | undefined, supported: readonly string[], preferred?: string | null): string {
  const available = availableEfforts(supported)
  if (value && available.includes(value)) return value
  // The model's own default comes next, then the host's usual order.
  if (preferred && available.includes(preferred)) return preferred
  return available.includes('medium') ? 'medium' : (available.includes('high') ? 'high' : (available[0] ?? ''))
}
