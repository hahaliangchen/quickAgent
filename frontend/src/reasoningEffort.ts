export function availableEfforts(supported: readonly string[]): readonly string[] {
  return supported.filter(level => level !== '' && level !== 'none')
}

export function resolveEffort(value: string | null | undefined, supported: readonly string[]): string {
  const available = availableEfforts(supported)
  if (value && available.includes(value)) return value
  return available.includes('medium') ? 'medium' : (available.includes('high') ? 'high' : (available[0] ?? ''))
}
