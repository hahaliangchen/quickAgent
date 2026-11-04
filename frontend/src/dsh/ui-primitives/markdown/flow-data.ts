/** Data-only flow diagrams; reject invalid and oversized models instead of guessing. */
export type FlowData = {
  nodes: { id: string; label: string }[]
  edges: { source: string; target: string; label?: string }[]
}
export function parseFlowData(source: string): FlowData | null {
  if (source.length > 20000) return null
  try {
    const value: unknown = JSON.parse(source)
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null
    const data = value as Record<string, unknown>
    if (!Array.isArray(data.nodes) || !Array.isArray(data.edges) || data.nodes.length < 2 || data.nodes.length > 50 || data.edges.length < 1 || data.edges.length > 100) return null
    const nodes: FlowData['nodes'] = []
    const ids = new Set<string>()
    for (const item of data.nodes) {
      if (!item || typeof item !== 'object' || Array.isArray(item)) return null
      const node = item as Record<string, unknown>
      if (typeof node.id !== 'string' || !/^[a-zA-Z0-9_-]{1,64}$/.test(node.id) || ids.has(node.id) || typeof node.label !== 'string' || !node.label.trim() || node.label.length > 160) return null
      ids.add(node.id); nodes.push({id:node.id, label:node.label})
    }
    const edges: FlowData['edges'] = []
    for (const item of data.edges) {
      if (!item || typeof item !== 'object' || Array.isArray(item)) return null
      const edge = item as Record<string, unknown>
      if (typeof edge.source !== 'string' || typeof edge.target !== 'string' || !ids.has(edge.source) || !ids.has(edge.target) || edge.source === edge.target || (edge.label !== undefined && (typeof edge.label !== 'string' || edge.label.length > 80))) return null
      edges.push({source:edge.source, target:edge.target, ...(typeof edge.label === 'string' ? {label:edge.label} : {})})
    }
    return {nodes, edges}
  } catch { return null }
}

/** Place a directed graph in layers. Cyclic remainders keep their supplied order. */
export function layoutFlowData(flow: FlowData): Map<string, {x:number; y:number}> {
  const incoming = new Map(flow.nodes.map(node => [node.id, 0]))
  const levels = new Map(flow.nodes.map(node => [node.id, 0]))
  const outgoing = new Map(flow.nodes.map(node => [node.id, [] as string[]]))
  for (const edge of flow.edges) { incoming.set(edge.target, incoming.get(edge.target)! + 1); outgoing.get(edge.source)!.push(edge.target) }
  const queue = flow.nodes.filter(node => incoming.get(node.id) === 0).map(node => node.id)
  for (let i=0; i<queue.length; i++) {
    const id=queue[i]!
    for (const target of outgoing.get(id)!) {
      levels.set(target, Math.max(levels.get(target)!, levels.get(id)! + 1))
      incoming.set(target, incoming.get(target)! - 1)
      if (incoming.get(target) === 0) queue.push(target)
    }
  }
  const groups = new Map<number, string[]>()
  for (const node of flow.nodes) { const level=levels.get(node.id)!; groups.set(level, [...groups.get(level) ?? [],node.id]) }
  const result = new Map<string, {x:number;y:number}>()
  for (const [level, ids] of groups) ids.forEach((id, index) => result.set(id,{x:(index-(ids.length-1)/2)*240,y:level*85}))
  return result
}
