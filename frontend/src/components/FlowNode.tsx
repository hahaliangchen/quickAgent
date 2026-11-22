import { memo } from 'react'
import { Handle, Position, type Node, type NodeProps } from '@xyflow/react'
import { FLOW_HEIGHT, type FlowKind, type FlowNode as FlowGraphNode, type FlowStatus } from '../flowGraph.ts'
import styles from './FlowNode.module.css'

export type FlowNodeData = { readonly node: FlowGraphNode } & Record<string, unknown>

const KINDS: Readonly<Record<FlowKind, string>> = {
  request: '请求', step: '步骤', call: '函数调用', reply: '回复', end: '结束',
}

const STATES: Readonly<Record<FlowStatus, string>> = {
  pending: '等待', running: '进行中', done: '完成', failed: '失败', interrupted: '已中断',
}

/** How many calls a card names before it counts the rest. */
const SHOWN_CALLS = 2

/**
 * One node of the turn: what it is, what it did, where it stands.
 *
 * A step is a single card by design — the tools it called are named on it, not
 * drawn as cards of their own — so the shape of a long turn stays readable at a
 * glance and the detail lives one click away.
 */
export const FlowNodeView = memo(function FlowNodeView({ data, selected }: NodeProps<Node<FlowNodeData>>) {
  const { node } = data
  const names = node.work.names
  const hidden = names.length - SHOWN_CALLS
  const meta = node.kind === 'end' ? '' : node.meta
  return (
    // The height is the one the layout reserved: the layout marks the edges, the node keeps its word.
    <div className={styles.node} style={{ height: FLOW_HEIGHT[node.kind] }} data-kind={node.kind} data-status={node.status} data-selected={selected || undefined}>
      <Handle type="target" position={Position.Top} id="in" className={styles.handle} />
      <Handle type="source" position={Position.Bottom} id="out" className={styles.handle} />
      <div className={styles.top}>
        <span className={styles.kind}>{KINDS[node.kind]}</span>
        {meta !== '' && <span className={styles.meta}>{meta}</span>}
        <span className={styles.spacer} />
        {node.work.tools > 0 && <span className={styles.count} title={String(node.work.tools) + ' 次工具调用'}>{node.work.tools}</span>}
        <span className={styles.status}>{STATES[node.status]}{node.kind === 'end' && node.meta !== '' ? ' · ' + node.meta : ''}</span>
      </div>
      <div className={styles.title} title={node.title}>{node.title}</div>
      {node.summary !== '' && <div className={styles.summary} title={node.summary}>{node.summary}</div>}
      {names.length > 0 && (
        <div className={styles.chips}>
          {names.slice(0, SHOWN_CALLS).map(one => (
            <span key={one.name} className={styles.chip} data-tone={one.tone} title={one.name}>{one.name}</span>
          ))}
          {hidden > 0 && <span className={styles.more} title={node.work.label}>+{hidden}</span>}
        </div>
      )}
    </div>
  )
})
