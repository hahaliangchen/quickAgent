import { Fragment, memo, useEffect, useMemo, useState, type ReactNode } from 'react'
import {
  DisclosureRow, IconClockOutlineMedium, IconCodeOutlineRegular, IconPlayOutlineMedium,
  IconQuestionOutlineMedium, IconSparkleRegular, IconThinkOutlineRegular, IconWarningTriangleOutlineMedium, MarkdownText,
} from '@deepseek-ai/dsh-client-ui-primitives'
import { useDisclosure } from '../disclosure.ts'
import toolCss from '../dsh/tool/ToolRow.module.css'
import { markdownLabels } from '../dsh/markdown-labels.ts'
import { t } from '../i18n.ts'
import {
  PROGRAM_FENCE, contractSummary, emptyThought, processEntries, reasoningHeadline, runningCallState,
  type AssistantItem, type CallItem, type ChatItem, type ExecutionPlan, type ProgramItem, type ToolItem,
} from '../model.ts'
import { AssistantMessage } from './AssistantMessage.tsx'
import { ToolRow, toolLabel } from './ToolRow.tsx'
import { toolVisual } from './toolVisuals.tsx'
import css from './ProcessList.module.css'

const ACTIVE = new Set(['pending', 'running', 'suspended'])

function firstLine(text: string): string {
  return text.trim().split(/\r?\n/)[0] ?? ''
}

const LEAKED_PROGRAM_RE = /(?:^|\n)\s*(?:\/\/|\/\*+|\*)\s*@(?:step|purpose|expect|reuse|virtual)\b/
const STEP_TAG_RE = /(?:^|\n)\s*(?:\/\/|\/\*+|\*)\s*@step\s+([^\n]+)/g

function extractStepTitles(text: string): string[] {
  const titles: string[] = []
  for (const match of text.matchAll(STEP_TAG_RE)) {
    const raw = (match[1] ?? '').replace(/\*\/\s*$/, '').trim()
    if (raw !== '') titles.push(raw)
  }
  return titles
}

function isLeakedProgramDraft(text: string): boolean {
  const trimmed = text.trimStart()
  return !trimmed.startsWith('```') && LEAKED_PROGRAM_RE.test(trimmed)
}

function activityLine(reply: AssistantItem): string {
  if (reply.text.trim() === '') return reasoningHeadline(reply.reasoning)
  if (isLeakedProgramDraft(reply.text)) {
    return extractStepTitles(reply.text)[0] ?? (reasoningHeadline(reply.reasoning) || '编写程序')
  }
  return firstLine(reply.text)
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
      return inner === '' ? `${item.name} · ${runningCallState(item, Date.now())}` : `${item.name} · ${inner}`
    }
    if (item.kind === 'tool') {
      const label = toolLabel(item.name, item.arguments)
      return [label.title, label.summary].filter(Boolean).join(' · ')
    }
    if (item.kind === 'program') {
      if (item.event === 'started') return '正在执行代码'
      if (item.event === 'invalid') return '代码未通过编译，正在修正'
      if (item.event === 'failed') return '代码执行出错，正在排查'
    }
    if (item.kind === 'assistant') {
      if (item.streaming && item.toolProgress?.length) return toolProgressLabel(item)
      if (item.program !== undefined) return '编写程序'
      const line = activityLine(item)
      if (line !== '') return line
    }
  }
  return ''
}

export function toolProgressLabel(reply: AssistantItem): string {
  return (reply.toolProgress ?? []).map(tool => {
    const action = tool.name === 'write_program' ? '正在编写程序'
      : (tool.name === 'run' || tool.name === 'run_js') ? '正在编写执行代码'
        : tool.name === 'write_file' ? '正在编写文件'
          : `正在生成工具调用${tool.name ? ` ${tool.name}` : ''}`
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
  // The model can answer with a bare run_js call and no reasoning summary at all;
  // a settled reply like that would otherwise paint an empty "思考" row.
  if (emptyThought(reply, tools.length)) return null
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
        {expanded && <div className={css.inside}><ExecutionPlanView plan={program?.plan} /><AssistantMessage text={reply.text} reasoning={reply.reasoning} streaming={reply.streaming} showActions={false} /></div>}
      </DisclosureRow>
    </div>
  )
})

/** The time, ticking once a second while `on`, for live durations. */
function useClock(on: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!on) return
    setNow(Date.now())
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [on])
  return now
}

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
  const clock = useClock(active)
  const state = active && call.status !== 'suspended' ? runningCallState(call, clock)
    : active || !ACTIVE.has(call.status) ? CALL_STATES[call.status] ?? call.status : '已停止'
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

const PROGRAM_EVENTS: Record<ProgramItem['event'], {
  label: string
  tone: 'muted' | 'warn' | 'error'
  chipTone: 'task' | 'context' | 'edit' | 'danger'
  icon: ReactNode
}> = {
  started: { label: '开始运行', tone: 'muted', chipTone: 'task', icon: <IconPlayOutlineMedium /> },
  invalid: { label: '程序未通过编译', tone: 'error', chipTone: 'danger', icon: <IconWarningTriangleOutlineMedium /> },
  failed: { label: '程序出错', tone: 'error', chipTone: 'danger', icon: <IconWarningTriangleOutlineMedium /> },
  question: { label: '程序停下来等你回答', tone: 'warn', chipTone: 'edit', icon: <IconQuestionOutlineMedium /> },
  paused: { label: '程序已暂停', tone: 'warn', chipTone: 'edit', icon: <IconClockOutlineMedium /> },
  result_invalid: { label: '返回值不符合约定', tone: 'warn', chipTone: 'edit', icon: <IconWarningTriangleOutlineMedium /> },
  module_loaded: { label: '使用已写好的模块', tone: 'muted', chipTone: 'context', icon: <IconCodeOutlineRegular /> },
  module_fix: { label: '模块出错，交给 AI 修复', tone: 'warn', chipTone: 'edit', icon: <IconWarningTriangleOutlineMedium /> },
  visual_completed: { label: '视觉检查完成', tone: 'muted', chipTone: 'context', icon: <IconSparkleRegular /> },
}

function ProgramEvent({ item }: { item: ProgramItem }) {
  const { expanded, toggle } = useDisclosure()
  const meta = PROGRAM_EVENTS[item.event]
  const where = item.line === undefined ? '' : `第 ${item.line} 行`
  const source = item.source?.trim() ?? ''
  if (source !== '' && (item.event === 'started' || item.event === 'invalid')) {
    const lineCount = source.split(/\r?\n/).length
    const title = item.event === 'started' ? '执行代码' : meta.label
    const detail = [where, item.text].filter(Boolean).join('：')
    const summary = item.event === 'started'
      ? [`${lineCount} 行`, detail].filter(Boolean).join(' · ')
      : [detail || `${lineCount} 行`].join('')
    const fenced = `\`\`\`js\n${source}\n\`\`\``
    return (
      <div className={toolCss.root} data-tone={meta.chipTone} data-state={item.event === 'invalid' ? 'error' : 'ok'}>
        <DisclosureRow
          rowClassName={toolCss.row}
          leadingClassName={toolCss.leading}
          titleClassName={toolCss.title}
          chevronClassName={toolCss.chevron}
          icon={item.event === 'started' ? <IconCodeOutlineRegular size={14} /> : meta.icon}
          title={title}
          open={expanded}
          expandable
          expandOnRowClick
          keepContentWhenOpen
          onToggle={toggle}
          collapsedContent={summary !== '' && (
            <>
              <Separator />
              <span className={`${toolCss.summary} ${item.event === 'invalid' ? toolCss.errorSummary : ''}`} title={detail || undefined}>
                {summary}
              </span>
            </>
          )}
        >
          {expanded && (
            <div className={css.inside}>
              <ExecutionPlanView plan={item.plan} />
              <AssistantMessage text={fenced} reasoning="" streaming={false} showActions={false} />
            </div>
          )}
        </DisclosureRow>
      </div>
    )
  }
  return (
    <div
      className={`${toolCss.root} ${css.event}`}
      data-tone={meta.chipTone}
      data-event-tone={meta.tone}
      role={meta.tone === 'error' ? 'status' : undefined}
    >
      <div className={css.eventRow}>
        <span className={`${toolCss.leading} ${css.eventLeading}`} aria-hidden>{meta.icon}</span>
        <div className={css.eventBody}>
          <div className={css.eventHeader}>
            <strong>{meta.label}</strong>
            {(where !== '' || item.text !== '') && <span title={item.text}>{[where, item.text].filter(Boolean).join('：')}</span>}
          </div>
          <ExecutionPlanView plan={item.plan} />
        </div>
      </div>
    </div>
  )
}

function ExecutionPlanView({ plan }: { plan: ExecutionPlan | undefined }) {
  if (plan === undefined || plan.steps.length === 0) return null
  return (
    <div className={css.plan}>
      <div className={css.planTitle}>计划（{plan.steps.length} 步）</div>
      <ol className={css.planList}>
        {plan.steps.map(step => {
          const details = [
            step.purpose ? { label: '目的：', value: step.purpose } : null,
            step.expect ? { label: '预期：', value: step.expect } : null,
            step.reuse ? { label: '复用：', value: step.reuse } : null,
          ].filter((item): item is { label: string; value: string } => item !== null)
          return (
            <li key={`${step.line ?? 0}:${step.title}`} className={css.planStep}>
              <div className={css.planStepTitle}>{step.title}</div>
              {details.length > 0 && (
                <div className={css.planMeta}>
                  {details.map((item, idx) => (
                    <Fragment key={item.label}>
                      {idx > 0 && '；'}
                      <strong className={css.planMetaLabel}>{item.label}</strong>
                      {item.value}
                    </Fragment>
                  ))}
                </div>
              )}
            </li>
          )
        })}
      </ol>
    </div>
  )
}

/** A leaked @step draft reply: show only extracted step intent, keeping raw draft collapsed. */
const LeakedDraftStep = memo(function LeakedDraftStep({ reply, tools, closed, onOpenTask }: {
  reply: AssistantItem
  tools: readonly ToolItem[]
  closed: boolean
  onOpenTask: (taskId: string) => void
}) {
  const { expanded, toggle } = useDisclosure()
  const labels = useMemo(() => markdownLabels(t), [])
  const titles = useMemo(() => extractStepTitles(reply.text), [reply.text])
  const title = titles[0] ?? (reasoningHeadline(reply.reasoning) || '执行步骤')
  return (
    <>
      <div className={toolCss.root} data-tone="context" data-state={reply.streaming ? 'running' : 'ok'}>
        <DisclosureRow rowClassName={toolCss.row} leadingClassName={toolCss.leading} titleClassName={toolCss.title} chevronClassName={toolCss.chevron}
          icon={<IconThinkOutlineRegular size={14} />} title={title} running={reply.streaming}
          open={expanded} expandable expandOnRowClick keepContentWhenOpen onToggle={toggle}
          collapsedContent={titles.length > 1 ? <><Separator /><span className={toolCss.summary}>{`共 ${titles.length} 步`}</span></> : undefined}>
          {expanded && (
            <div className={css.inside}>
              {reply.reasoning.trim() !== '' && <div className={css.thought}><MarkdownText text={reply.reasoning} labels={labels} streaming={reply.streaming} variant="compact" /></div>}
              <AssistantMessage text={reply.text} reasoning="" streaming={reply.streaming} showActions={false} />
            </div>
          )}
        </DisclosureRow>
      </div>
      <Tools tools={tools} closed={closed} onOpenTask={onOpenTask} />
    </>
  )
})

const Step = memo(function Step({ reply, tools, closed, onOpenTask }: {
  reply: AssistantItem
  tools: readonly ToolItem[]
  closed: boolean
  onOpenTask: (taskId: string) => void
}) {
  if (reply.program !== undefined) return <><ProgramReply reply={reply} /><Tools tools={tools} closed={closed} onOpenTask={onOpenTask} /></>
  if (reply.text.trim() === '') return <Thought reply={reply} tools={tools} closed={closed} onOpenTask={onOpenTask} />
  if (isLeakedProgramDraft(reply.text)) return <LeakedDraftStep reply={reply} tools={tools} closed={closed} onOpenTask={onOpenTask} />
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
