import { Fragment, memo, useMemo, type ReactNode } from 'react'
import {
  DisclosureRow, IconCodeOutlineRegular, IconSparkleRegular, IconThinkOutlineRegular, MarkdownText,
} from '@deepseek-ai/dsh-client-ui-primitives'
import { useDisclosure } from '../disclosure.ts'
import toolCss from '../dsh/tool/ToolRow.module.css'
import { markdownLabels } from '../dsh/markdown-labels.ts'
import { t } from '../i18n.ts'
import {
  PROGRAM_FENCE, contractSummary, processEntries, reasoningHeadline,
  type AssistantItem, type CallItem, type ChatItem, type ProgramItem, type ToolItem,
} from '../model.ts'
import { AssistantMessage } from './AssistantMessage.tsx'
import { ToolRow, toolLabel } from './ToolRow.tsx'
import { toolVisual } from './toolVisuals.tsx'
import css from './ProcessList.module.css'

const ACTIVE = new Set(['pending', 'running', 'suspended'])

function firstLine(text: string): string {
  return text.trim().split(/\r?\n/)[0] ?? ''
}

/** Lines of the ```js block a reply holds. */
function programLines(text: string): number {
  const open = PROGRAM_FENCE.exec(text)
  if (open === null) return 0
  const body = text.slice(open.index + open[0].length)
  const close = /^```[ \t]*$/m.exec(body)
  return (close === null ? body : body.slice(0, close.index)).trim().split(/\r?\n/).length
}

/** What a list of steps is doing now: the latest thought, call or tool, inside running calls first. */
export function liveActivity(items: readonly ChatItem[]): string {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index]
    if (item === undefined) continue
    if (item.kind === 'call') {
      if (!ACTIVE.has(item.status)) return ''
      const inner = liveActivity(item.steps)
      return inner === '' ? `${item.name} · AI 处理中` : `${item.name} · ${inner}`
    }
    if (item.kind === 'tool') {
      const label = toolLabel(item.name, item.arguments)
      return [label.title, label.summary].filter(Boolean).join(' · ')
    }
    if (item.kind === 'assistant') {
      if (item.streaming && item.toolProgress?.length) return toolProgressLabel(item)
      if (PROGRAM_FENCE.test(item.text)) return '编写程序'
      const line = item.text.trim() !== '' ? firstLine(item.text) : reasoningHeadline(item.reasoning)
      if (line !== '') return line
    }
  }
  return ''
}

export function toolProgressLabel(reply: AssistantItem): string {
  return (reply.toolProgress ?? []).map(tool => {
    const action = tool.name === 'write_program' ? '正在编写程序' : tool.name === 'write_file' ? '正在编写文件' : `正在生成工具调用${tool.name ? ` ${tool.name}` : ''}`
    return `${action} · 已生成 ${tool.argumentBytes.toLocaleString()} 字节`
  }).join('；')
}

function toolsTitle(tools: readonly ToolItem[]): string {
  const counts = new Map<string, number>()
  for (const tool of tools) {
    const title = toolLabel(tool.name, tool.arguments).title
    counts.set(title, (counts.get(title) ?? 0) + 1)
  }
  return [...counts].map(([title, count]) => count > 1 ? `${title} ×${count}` : title).join(' · ')
}

function toolsSummary(tools: readonly ToolItem[]): string {
  const only = tools.length === 1 ? tools[0] : undefined
  if (only === undefined) return toolsTitle(tools)
  const label = toolLabel(only.name, only.arguments)
  return [only.line === undefined ? '' : `第 ${only.line} 行`, label.title, label.summary].filter(Boolean).join(' · ')
}

function Separator() {
  return <span className={toolCss.sep} aria-hidden>·</span>
}

const Tools = memo(function Tools({ tools, closed, onOpenTask }: {
  tools: readonly ToolItem[]
  closed: boolean
  onOpenTask: (taskId: string) => void
}) {
  const { expanded, toggle } = useDisclosure()
  const first = tools[0]
  if (first === undefined) return null
  const rows = tools.map(tool => (
    <ToolRow key={tool.key} name={tool.name} rawArguments={tool.arguments} result={tool.result} stopped={closed} onOpenChild={onOpenTask} line={tool.line} />
  ))
  if (tools.length === 1) return rows[0]
  const running = !closed && tools.some(tool => tool.result === undefined)
  const failed = tools.filter(tool => tool.result?.isError === true).length
  const visual = toolVisual(first.name, undefined)
  return (
    <div className={toolCss.root} data-tone={visual.tone} data-state={running ? 'running' : failed > 0 ? 'error' : 'ok'}>
      <DisclosureRow rowClassName={toolCss.row} leadingClassName={toolCss.leading} titleClassName={toolCss.title} chevronClassName={toolCss.chevron}
        icon={visual.icon} title={toolsTitle(tools)} running={running} open={expanded} expandable expandOnRowClick keepContentWhenOpen onToggle={toggle}
        collapsedContent={failed > 0 && <><Separator /><span className={`${toolCss.summary} ${toolCss.errorSummary}`}>{failed} 个失败</span></>}>
        {expanded && <div className={css.inside}>{rows}</div>}
      </DisclosureRow>
    </div>
  )
})

/** A reply with no text: its reasoning summary says what the AI is doing, its tools follow inside. */
const Thought = memo(function Thought({ reply, tools, closed, onOpenTask }: {
  reply: AssistantItem
  tools: readonly ToolItem[]
  closed: boolean
  onOpenTask: (taskId: string) => void
}) {
  const { expanded, toggle } = useDisclosure()
  const labels = useMemo(() => markdownLabels(t), [])
  const running = reply.streaming || (!closed && tools.some(tool => tool.result === undefined))
  const failed = tools.some(tool => tool.result?.isError === true)
  const headline = reasoningHeadline(reply.reasoning)
  return (
    <div className={toolCss.root} data-tone="context" data-state={running ? 'running' : failed ? 'error' : 'ok'}>
      <DisclosureRow rowClassName={toolCss.row} leadingClassName={toolCss.leading} titleClassName={toolCss.title} chevronClassName={toolCss.chevron}
        icon={<IconThinkOutlineRegular size={14} />} title={reply.streaming && reply.toolProgress?.length ? toolProgressLabel(reply) : headline === '' ? '思考' : headline} running={running}
        open={expanded} expandable expandOnRowClick keepContentWhenOpen onToggle={toggle}
        collapsedContent={tools.length > 0 && <><Separator /><span className={`${toolCss.summary} ${failed ? toolCss.errorSummary : ''}`}>{toolsSummary(tools)}</span></>}>
        {expanded && (
          <div className={css.inside}>
            {reply.reasoning.trim() !== '' && <div className={css.thought}><MarkdownText text={reply.reasoning} labels={labels} streaming={reply.streaming} variant="compact" /></div>}
            {tools.map(tool => <ToolRow key={tool.key} name={tool.name} rawArguments={tool.arguments} result={tool.result} stopped={closed} onOpenChild={onOpenTask} line={tool.line} />)}
          </div>
        )}
      </DisclosureRow>
    </div>
  )
})

/** A reply holding a program: one row, the code inside. */
const ProgramReply = memo(function ProgramReply({ reply }: { reply: AssistantItem }) {
  const { expanded, toggle } = useDisclosure()
  const program = reply.program
  const status = program?.status === 'started' ? '已开始运行'
    : program?.status === 'invalid' ? `未通过编译${program.line === undefined ? '' : `（第 ${program.line} 行）`}：${program.error ?? ''}`
      : ''
  return (
    <div className={toolCss.root} data-tone="task" data-state={program?.status === 'invalid' ? 'error' : reply.streaming ? 'running' : 'ok'}>
      <DisclosureRow rowClassName={toolCss.row} leadingClassName={toolCss.leading} titleClassName={toolCss.title} chevronClassName={toolCss.chevron}
        icon={<IconCodeOutlineRegular size={14} />} title="编写了程序" running={reply.streaming}
        open={expanded} expandable expandOnRowClick keepContentWhenOpen onToggle={toggle}
        collapsedContent={<><Separator /><span className={`${toolCss.summary} ${program?.status === 'invalid' ? toolCss.errorSummary : ''}`}>
          {[`${programLines(reply.text)} 行`, status].filter(Boolean).join(' · ')}
        </span></>}>
        {expanded && <div className={css.inside}><AssistantMessage text={reply.text} reasoning={reply.reasoning} streaming={reply.streaming} showActions={false} /></div>}
      </DisclosureRow>
    </div>
  )
})

const CALL_STATES: Record<string, string> = {
  pending: 'AI 处理中', running: 'AI 处理中', suspended: '等待内部调用', paused: '已暂停',
  done: '完成', failed: '失败', interrupted: '已中断', cancelled: '已取消',
}

function ReturnValue({ text }: { text: string }) {
  const value = useMemo(() => {
    try { return JSON.stringify(JSON.parse(text), null, 2) } catch { return text }
  }, [text])
  return <details className={css.returned}><summary>返回值</summary><pre>{value}</pre></details>
}

/** A @virtual or @module call: what it is for, its state, and its own steps inside. */
const Call = memo(function Call({ call, closed, onOpenTask, renderItem }: {
  call: CallItem
  closed: boolean
  onOpenTask: (taskId: string) => void
  renderItem: (item: ChatItem) => ReactNode
}) {
  const { expanded, toggle } = useDisclosure()
  const active = ACTIVE.has(call.status) && !closed
  const state = active || !ACTIVE.has(call.status) ? CALL_STATES[call.status] ?? call.status : '已停止'
  const now = active ? liveActivity(call.steps) : ''
  const detail = active ? now : call.error ?? contractSummary(call.description)
  const failed = call.status === 'failed'
  return (
    <div className={toolCss.root} data-tone={failed ? 'danger' : 'agent'} data-state={active ? 'running' : failed ? 'error' : 'ok'}>
      <DisclosureRow rowClassName={toolCss.row} leadingClassName={toolCss.leading} titleClassName={toolCss.title} chevronClassName={toolCss.chevron}
        icon={<IconSparkleRegular size={14} />} title={call.name} running={active}
        open={expanded} expandable expandOnRowClick keepContentWhenOpen onToggle={toggle}
        collapsedContent={<><Separator /><span className={`${toolCss.summary} ${failed ? toolCss.errorSummary : ''}`}>
          {[call.line === undefined ? '' : `第 ${call.line} 行`, call.module ? `模块 · ${state}` : state, detail].filter(Boolean).join(' · ')}
        </span></>}>
        {expanded && (
          <div className={css.inside}>
            {call.description !== '' && <p className={css.contract}>{call.description}</p>}
            <ProcessList items={call.steps} closed={closed || !active} onOpenTask={onOpenTask} renderItem={renderItem} />
            {call.error !== undefined && <p className={css.error}>{call.error}</p>}
            {call.result !== undefined && <ReturnValue text={call.result} />}
          </div>
        )}
      </DisclosureRow>
    </div>
  )
})

const PROGRAM_EVENTS: Record<ProgramItem['event'], { label: string; tone: 'muted' | 'warn' | 'error' }> = {
  started: { label: '开始运行', tone: 'muted' },
  invalid: { label: '程序未通过编译', tone: 'error' },
  failed: { label: '程序出错', tone: 'error' },
  question: { label: '程序停下来等你回答', tone: 'warn' },
  paused: { label: '程序已暂停', tone: 'warn' },
  result_invalid: { label: '返回值不符合约定', tone: 'warn' },
  module_loaded: { label: '使用已写好的模块', tone: 'muted' },
  module_fix: { label: '模块出错，交给 AI 修复', tone: 'warn' },
}

function ProgramEvent({ item }: { item: ProgramItem }) {
  const meta = PROGRAM_EVENTS[item.event]
  const where = item.line === undefined ? '' : `第 ${item.line} 行`
  return (
    <div className={css.event} data-tone={meta.tone} role={meta.tone === 'error' ? 'status' : undefined}>
      <strong>{meta.label}</strong>
      {(where !== '' || item.text !== '') && <span title={item.text}>{[where, item.text].filter(Boolean).join('：')}</span>}
    </div>
  )
}

const Step = memo(function Step({ reply, tools, closed, onOpenTask }: {
  reply: AssistantItem
  tools: readonly ToolItem[]
  closed: boolean
  onOpenTask: (taskId: string) => void
}) {
  if (PROGRAM_FENCE.test(reply.text)) return <><ProgramReply reply={reply} /><Tools tools={tools} closed={closed} onOpenTask={onOpenTask} /></>
  if (reply.text.trim() === '') return <Thought reply={reply} tools={tools} closed={closed} onOpenTask={onOpenTask} />
  return (
    <>
      <AssistantMessage text={reply.text} reasoning={reply.reasoning} streaming={reply.streaming} showActions={false} />
      <Tools tools={tools} closed={closed} onOpenTask={onOpenTask} />
    </>
  )
})

/** A turn's process, or a call's: each model reply with its tools, the program's calls, and what the host recorded. */
export function ProcessList({ items, closed, onOpenTask, renderItem }: {
  items: readonly ChatItem[]
  /** Nothing in this list runs any more: calls without a result were stopped. */
  closed: boolean
  onOpenTask: (taskId: string) => void
  /** Rows this list does not draw itself, such as messages from the user. */
  renderItem: (item: ChatItem) => ReactNode
}) {
  const entries = useMemo(() => processEntries(items), [items])
  return (
    <>
      {entries.map(entry => {
        if (entry.kind === 'step') return <Step key={entry.key} reply={entry.reply} tools={entry.tools} closed={closed} onOpenTask={onOpenTask} />
        if (entry.kind === 'tools') return <Tools key={entry.key} tools={entry.tools} closed={closed} onOpenTask={onOpenTask} />
        const item = entry.item
        if (item.kind === 'call') return <Call key={entry.key} call={item} closed={closed} onOpenTask={onOpenTask} renderItem={renderItem} />
        if (item.kind === 'program') return <ProgramEvent key={entry.key} item={item} />
        return <Fragment key={entry.key}>{renderItem(item)}</Fragment>
      })}
    </>
  )
}
