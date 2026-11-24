import { BaseEdge, EdgeLabelRenderer, getBezierPath, type Edge, type EdgeProps } from '@xyflow/react'
import type { GraphEdge } from './types.ts'
import css from './style.module.css'

export type Wire = Edge<{ item: GraphEdge; inspect: (id: string) => void; highlighted: boolean }, 'blueprint'>
const tones: Record<GraphEdge['relation'], string> = { triggers: 'var(--bp-ui)', http: 'var(--bp-service)', calls: 'var(--bp-wire)', reads: 'var(--bp-store)', writes: 'var(--bp-store)', emits: 'var(--bp-external)', must_sync: 'var(--bp-test)', guards: 'var(--bp-test)' }
export function BlueprintEdge({ id, data, selected, ...points }: EdgeProps<Wire>) {
  const [path, x, y] = getBezierPath(points)
  if (!data) return null
  const { item, inspect, highlighted } = data
  const critical = item.severity === 'critical'
  return <>
    <BaseEdge id={id} path={path} interactionWidth={18} style={{ stroke: critical ? 'var(--bp-ui)' : tones[item.relation], strokeWidth: selected || highlighted ? 3 : 2, ...(item.relation === 'must_sync' ? { strokeDasharray: '6 4' } : {}), ...(critical ? { filter: 'drop-shadow(0 0 3px var(--bp-ui))' } : {}) }} />
    <EdgeLabelRenderer><button type='button' data-testid={'edge-' + id} className={'nodrag nopan ' + css.edgeLabel} data-selected={selected || highlighted || undefined}
      style={{ left: x, top: y }} title={item.invariant || item.label || item.relation} onClick={() => inspect(id)}>
      {item.invariant && <span className={css.warning} aria-label='红线'>⚠ </span>}{item.label || item.relation}
    </button></EdgeLabelRenderer>
  </>
}

