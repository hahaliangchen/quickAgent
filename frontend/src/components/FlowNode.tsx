import { memo } from 'react'
import { Handle, Position, type Node, type NodeProps } from '@xyflow/react'
import type { ChatItem } from '../model.ts'
import { FLOW_HEIGHT, type FlowKind, type FlowNode as FlowGraphNode, type FlowStatus } from '../flowGraph.ts'
import { toolLabel } from './ToolRow.tsx'
import styles from './FlowNode.module.css'

export type FlowNodeData = { readonly node: FlowGraphNode } & Record<string, unknown>

const KINDS: Readonly<Record<FlowKind, string>> = {
  request: '请求', step: '步骤', tool: '工具', call: '函数调用', program: '程序',
  delegate: '子代理', note: '插话', reply: '回复', end: '结束',
}

const STATES: Readonly<Record<FlowStatus, string>> = {
  pending: '等待', running: '进行中', done: '完成', failed: '失败', interrupted: '已中断',
}

/** The tool a row stands for: its action title and what it was pointed at. */
function toolOf(item: ChatItem): { title: string; summary: string } {
  if (item.kind !== 'tool') return { title: '', summary: '' }
  try {
    return toolLabel(item.name, item.arguments)
  } catch {
    return { title: item.name, summary: '' }
  }
}

/** What the node says under its title: the tools behind it name what they did. */
function describe(node: FlowGraphNode): { title: string; summary: string } {
  const first = node.items[0]
  if (node.kind !== 'tool' || first === undefined) return { title: node.title, summary: node.summary }
  const label = toolOf(first)
  if (node.items.length === 1) return { title: label.title || node.title, summary: node.summary || label.summary }
  return { title: node.title, summary: label.title + (label.summary === '' ? '' : ' · ' + label.summary) }
}

/** One node of the turn: what it is, what it did, where it stands. */
export const FlowNodeView = memo(function FlowNodeView({ data, selected }: NodeProps<Node<FlowNodeData>>) {
  const { node } = data
  const text = describe(node)
  const time = node.kind === 'end' ? node.meta : ''
  const meta = node.kind === 'end' ? '' : node.meta
  return (
    // The height is the one the layout reserved: the layout marks the edges, the node keeps its word.
    <div className={styles.node} style={{ height: FLOW_HEIGHT[node.kind] }} data-kind={node.kind} data-status={node.status} data-selected={selected || undefined}>
      <Handle type="target" position={Position.Top} id="in" className={styles.handle} />
      <Handle type="source" position={Position.Bottom} id="out" className={styles.handle} />
      <div className={styles.top}>
        <span className={styles.kind}>{KINDS[node.kind]}</span>
        {node.items.length > 1 && <span className={styles.count}>{node.items.length}</span>}
        {meta !== '' && <span className={styles.meta}>{meta}</span>}
        <span className={styles.spacer} />
        <span className={styles.status}>{STATES[node.status]}{time !== '' && ' · ' + time}</span>
      </div>
      <div className={styles.title} title={text.title}>{text.title}</div>
      {text.summary !== '' && <div className={styles.summary} title={text.summary}>{text.summary}</div>}
    </div>
  )
})
