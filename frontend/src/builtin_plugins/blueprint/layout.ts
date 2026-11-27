import type { Entity, Graph } from './types.ts'

/** Rank acyclic paths while keeping trigger nodes at the left edge. Cycles retain their type's baseline rank. */
export function layout(graph: Graph, entities: readonly Entity[]) {
  const byId = new Map(entities.map(e => [e.id, e]))
  const ids = graph.nodes.map(n => n.entity_id).sort()
  const rank = new Map(ids.map(id => { const kind = byId.get(id)?.kind ?? 'service'; return [id, kind.startsWith('trigger_') ? 0 : kind === 'service' ? 1 : 2] }))
  const degree = new Map(ids.map(id => [id, 0]))
  const next = new Map<string, string[]>()
  for (const edge of graph.edges) {
    if (!rank.has(edge.source_entity) || !rank.has(edge.target_entity)) continue
    degree.set(edge.target_entity, (degree.get(edge.target_entity) ?? 0) + 1)
    const links = next.get(edge.source_entity) ?? []
    links.push(edge.target_entity)
    next.set(edge.source_entity, links)
  }
  const order = ids.filter(id => degree.get(id) === 0)
  for (let i = 0; i < order.length; i++) {
    const id = order[i]!
    for (const to of next.get(id) ?? []) {
      if (rank.get(to) !== 0) rank.set(to, Math.max(rank.get(to) ?? 0, (rank.get(id) ?? 0) + 1))
      const remaining = (degree.get(to) ?? 0) - 1
      degree.set(to, remaining)
      if (!remaining) order.push(to)
    }
  }
  const heights = new Map<number, number>()
  return [...new Set([...order, ...ids])].map(entity_id => {
    const col = rank.get(entity_id) ?? 0
    const y = heights.get(col) ?? 56
    const ent = byId.get(entity_id)
    heights.set(col, y + 142 + (ent?.properties?.length ?? 0) * 26 + (ent?.ports.length ?? 0) * 31)
    return { entity_id, x: 40 + col * 470, y }
  })
}

