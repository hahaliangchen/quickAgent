import { memo } from 'react'
import { Handle, Position, type Node, type NodeProps } from '@xyflow/react'
import type { FrameSummary } from '../api.ts'
import { contractSummary, formatDuration, isCommandTool, reasoningHeadline } from '../model.ts'
import { toolLabel } from './ToolRow.tsx'
import styles from './FrameNode.module.css'

export type FrameNodeData = { readonly frame: FrameSummary; readonly now: number } & Record<string, unknown>

const KINDS: Record<string, string> = {
  request: '用户请求', virtual: '虚函数', module: '模块', function: '函数', paused_program: '暂停的程序', conversation: '会话',
}

export const FRAME_STATES: Record<string, string> = {
  pending: '等待开始', running: 'AI 处理中', suspended: '等待内部调用', paused: '已暂停',
  done: '完成', failed: '失败', interrupted: '已中断', cancelled: '已取消',
}

/** Where a frame stands. A running frame whose latest step is a command, or a
 *  module whose code runs without an AI turn, is not an AI at work. */
export function frameState(frame: FrameSummary): string {
  if (frame.status === 'running') {
    if (frame.activity?.tool && isCommandTool(frame.activity.tool.name)) return '等待命令'
    if (frame.kind === 'module' && !frame.activity?.reasoning) return '运行中'
  }
  return FRAME_STATES[frame.status] ?? frame.status
}

/** What a running frame is doing: the tool it called after its latest thought, or that thought. */
export function frameActivity(frame: FrameSummary): string {
  const activity = frame.activity
  if (!activity) return ''
  if (activity.tool) {
    const label = toolLabel(activity.tool.name, JSON.stringify(activity.tool.arguments))
    return [label.title, label.summary].filter(Boolean).join(' · ')
  }
  return reasoningHeadline(activity.reasoning)
}

export function frameDuration(frame: FrameSummary, now: number): string {
  if (frame.started_at === null) return ''
  const end = frame.ended_at ?? (frame.status === 'running' || frame.status === 'suspended' ? now : null)
  return end === null ? '' : formatDuration(end - frame.started_at)
}

/** One function frame: what opened it, what it is for, where it stands. */
export const FrameNode = memo(function FrameNode({ data, selected }: NodeProps<Node<FrameNodeData>>) {
  const { frame, now } = data
  const name = frame.kind === 'request' ? frame.title : frame.kind === 'paused_program' ? '被你的消息暂停的程序' : frame.function ?? frame.title
  const purpose = frame.kind === 'request' ? '' : contractSummary(frame.description ?? '')
  const live = frame.status === 'running' ? frameActivity(frame) : ''
  const duration = frameDuration(frame, now)
  return (
    <div className={styles.node} data-status={frame.status} data-kind={frame.kind} data-selected={selected || undefined}>
      <Handle type="target" position={Position.Top} id="in" className={styles.handle} />
      <Handle type="source" position={Position.Bottom} id="out" className={styles.handle} />
      <div className={styles.header}>
        <span className={styles.kind}>{KINDS[frame.kind] ?? frame.kind}</span>
        {frame.concurrent && frame.kind !== 'paused_program' && <span className={styles.tag}>并发</span>}
        {frame.call_site && <span className={styles.tag}>第 {frame.call_site.line} 行</span>}
        <span className={styles.status}>{frameState(frame)}{duration && ` · ${duration}`}</span>
      </div>
      <div className={styles.name} title={name}>{name}</div>
      {purpose !== '' && <div className={styles.purpose} title={frame.description ?? ''}>{purpose}</div>}
      {live !== '' && <div className={styles.live} title={live}>{live}</div>}
      {frame.error && <div className={styles.error} title={frame.error}>{frame.error}</div>}
    </div>
  )
})
