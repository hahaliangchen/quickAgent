import { Fragment, useCallback, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import clsx from 'clsx'
import { writeClipboard } from '../clipboard.ts'
import { IconCheckOutlineRegular, IconCopyOutlineRegular } from '../icons/index.tsx'
import { Tooltip } from '../Tooltip.tsx'
import { tokenizeTerminalLine } from './codeBlockMeta.ts'
import type { ExecutionEntryData, SemanticTone } from './codeBlockMeta.ts'
import { useIsPaperStyle } from './uiStyle.ts'
import css from './ExecutionGroup.module.css'

export interface ExecutionGroupAttachment {
  readonly kind: 'note' | 'diagnostic'
  readonly key: string
  readonly tone?: SemanticTone | undefined
  readonly text?: string | undefined
  readonly node?: ReactNode | undefined
}

export interface ExecutionGroupItem {
  readonly key: string
  readonly entry: ExecutionEntryData
  readonly attachments: readonly ExecutionGroupAttachment[]
}

export interface ExecutionGroupProps {
  readonly items: readonly ExecutionGroupItem[]
  readonly copyLabel: string
  readonly copiedLabel: string
  /** Original ungrouped blocks rendered verbatim when Borderless Paper mode is active. */
  readonly fallback: readonly ReactNode[]
}

type ExecutionTab = 'overview' | 'commands' | 'output'

function OverviewIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M2.5 13V9.5M6.2 13V4.5M9.8 13V7.2M13.5 13V3" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  )
}

function CodeIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M5.5 4.2L2 8L5.5 11.8M10.5 4.2L14 8L10.5 11.8M8.8 3.2L7.2 12.8" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

function OutputIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <rect x="2.2" y="2.2" width="11.6" height="11.6" rx="2.2" stroke="currentColor" strokeWidth="1.35" />
      <path d="M5 6.2H11M5 9.2H9.2" stroke="currentColor" strokeWidth="1.35" strokeLinecap="round" />
    </svg>
  )
}

function ChevronIcon({ open }: { readonly open: boolean }) {
  return (
    <svg className={clsx(css.chevron, open && css.chevronOpen)} width="12" height="12" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M6 3.75L10.5 8L6 12.25" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

function TerminalMiniIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M3.75 5.25L6.75 8L3.75 10.75M8.25 10.75H12" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

function CheckMiniIcon() {
  return (
    <svg width="11" height="11" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M3.2 8.4L6.5 11.6L12.8 4.6" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

function CrossMiniIcon() {
  return (
    <svg width="11" height="11" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M4.5 4.5L11.5 11.5M11.5 4.5L4.5 11.5" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
    </svg>
  )
}

function MinusMiniIcon() {
  return (
    <svg width="11" height="11" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M4 8H12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
    </svg>
  )
}

function WarningMiniIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M8 2.2L14.2 13.1C14.48 13.6 14.12 14.2 13.55 14.2H2.45C1.88 14.2 1.52 13.6 1.8 13.1L8 2.2Z" stroke="currentColor" strokeWidth="1.35" strokeLinejoin="round" />
      <path d="M8 6.1V9.3" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
      <circle cx="8" cy="11.6" r="0.8" fill="currentColor" />
    </svg>
  )
}

function renderNumberedLines(lines: readonly string[]): ReactNode {
  return (
    <div className={css.well}>
      <pre className={css.wellPre}><code>{lines.map((line, index) => (
        <div key={index} className={css.wellLine}>
          <span className={css.lineNo} aria-hidden="true">{index + 1}</span>
          <span className={css.lineText}>
            {tokenizeTerminalLine(line).map((token, tokenIndex) => (
              token.kind === 'plain'
                ? <Fragment key={tokenIndex}>{token.text}</Fragment>
                : <span key={tokenIndex} className={css[`term_${token.kind}`]}>{token.text}</span>
            ))}
          </span>
        </div>
      ))}</code></pre>
    </div>
  )
}

function CopyIconButton({
  text,
  copyLabel,
  copiedLabel,
}: {
  readonly text: string
  readonly copyLabel: string
  readonly copiedLabel: string
}) {
  const [copied, setCopied] = useState(false)
  const label = copied ? copiedLabel : copyLabel
  const onCopy = useCallback((event: React.MouseEvent) => {
    event.stopPropagation()
    if (copied) return
    void writeClipboard(text).then((ok) => {
      if (!ok) return
      setCopied(true)
      window.setTimeout(() => { setCopied(false) }, 1000)
    })
  }, [copied, text])

  return (
    <Tooltip label={label} side="top" portal>
      <button type="button" className={css.iconBtn} aria-label={label} onClick={onCopy}>
        {copied ? <IconCheckOutlineRegular size={14} /> : <IconCopyOutlineRegular size={14} />}
      </button>
    </Tooltip>
  )
}

export function ExecutionGroup({ items, copyLabel, copiedLabel, fallback }: ExecutionGroupProps) {
  const isPaperStyle = useIsPaperStyle()
  const [tab, setTab] = useState<ExecutionTab>('overview')
  const [copiedPane, setCopiedPane] = useState<string | null>(null)

  // Failed commands open by default so errors are immediately visible; passed commands start compact.
  const [openMap, setOpenMap] = useState<Record<string, boolean>>(() => {
    const initial: Record<string, boolean> = {}
    for (const item of items) {
      initial[item.key] = item.entry.tone === 'error'
    }
    return initial
  })

  const stats = useMemo(() => {
    let totalPassed = 0
    let totalFailed = 0
    let totalIgnored = 0
    let hasTestCounts = false
    let passedCmds = 0
    let failedCmds = 0

    for (const { entry } of items) {
      if (entry.passed !== undefined) {
        totalPassed += entry.passed
        hasTestCounts = true
      }
      if (entry.failed !== undefined) {
        totalFailed += entry.failed
        hasTestCounts = true
      }
      if (entry.ignored !== undefined) {
        totalIgnored += entry.ignored
        hasTestCounts = true
      }
      if (entry.tone === 'success') passedCmds += 1
      else if (entry.tone === 'error') failedCmds += 1
    }

    return { totalPassed, totalFailed, totalIgnored, hasTestCounts, passedCmds, failedCmds }
  }, [items])

  const allExpanded = useMemo(
    () => items.length > 0 && items.every(item => openMap[item.key] === true),
    [items, openMap],
  )

  const toggleRow = useCallback((key: string) => {
    setOpenMap(prev => ({ ...prev, [key]: !prev[key] }))
  }, [])

  const toggleAll = useCallback(() => {
    const next = !allExpanded
    const updated: Record<string, boolean> = {}
    for (const item of items) updated[item.key] = next
    setOpenMap(updated)
  }, [allExpanded, items])

  const allCommandsLines = useMemo(() => items.map(item => item.entry.command), [items])
  const allCommandsText = useMemo(() => allCommandsLines.join('\n'), [allCommandsLines])

  const allOutputLines = useMemo(() => {
    const lines: string[] = []
    items.forEach((item, idx) => {
      if (idx > 0) lines.push('')
      lines.push(`$ ${item.entry.command}`)
      if (item.entry.exitCode !== undefined) lines.push(`exit code: ${item.entry.exitCode}`)
      lines.push(...item.entry.outputLines)
      for (const att of item.attachments) {
        if (att.kind === 'diagnostic' && att.text !== undefined) {
          lines.push(...att.text.split('\n'))
        }
      }
    })
    return lines
  }, [items])
  const allOutputText = useMemo(() => allOutputLines.join('\n'), [allOutputLines])

  const copyPaneText = useCallback((paneId: string, text: string) => {
    if (copiedPane === paneId) return
    void writeClipboard(text).then((ok) => {
      if (!ok) return
      setCopiedPane(paneId)
      window.setTimeout(() => { setCopiedPane(cur => (cur === paneId ? null : cur)) }, 1000)
    })
  }, [copiedPane])

  // Borderless Paper mode stays 100% untouched.
  if (isPaperStyle) {
    return <>{fallback}</>
  }

  return (
    <div className={css.panel} data-execution-group>
      <div className={css.tabBar} role="tablist" aria-label="执行结果视图">
        <div className={css.tabs}>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'overview'}
            className={css.tab}
            onClick={() => { setTab('overview') }}
          >
            <OverviewIcon />
            <span>概览</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'commands'}
            className={css.tab}
            onClick={() => { setTab('commands') }}
          >
            <CodeIcon />
            <span>代码</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'output'}
            className={css.tab}
            onClick={() => { setTab('output') }}
          >
            <OutputIcon />
            <span>输出</span>
          </button>
        </div>
      </div>

      {tab === 'overview' && (
        <>
          <div className={css.summaryBar}>
            <div className={css.metrics}>
              {stats.hasTestCounts ? (
                <>
                  {stats.totalPassed > 0 && (
                    <span className={clsx(css.metric, css.metricPass)}>
                      <span className={css.metricDot}><CheckMiniIcon /></span>
                      <span>{stats.totalPassed} passed</span>
                    </span>
                  )}
                  {stats.totalFailed > 0 && (
                    <span className={clsx(css.metric, css.metricFail)}>
                      <span className={css.metricDot}><CrossMiniIcon /></span>
                      <span>{stats.totalFailed} failed</span>
                    </span>
                  )}
                  {stats.totalIgnored > 0 && (
                    <span className={clsx(css.metric, css.metricIgnored)}>
                      <span className={css.metricDot}><MinusMiniIcon /></span>
                      <span>{stats.totalIgnored} ignored</span>
                    </span>
                  )}
                </>
              ) : (
                <>
                  {stats.passedCmds > 0 && (
                    <span className={clsx(css.metric, css.metricPass)}>
                      <span className={css.metricDot}><CheckMiniIcon /></span>
                      <span>{stats.passedCmds} passed</span>
                    </span>
                  )}
                  {stats.failedCmds > 0 && (
                    <span className={clsx(css.metric, css.metricFail)}>
                      <span className={css.metricDot}><CrossMiniIcon /></span>
                      <span>{stats.failedCmds} failed</span>
                    </span>
                  )}
                  {stats.passedCmds === 0 && stats.failedCmds === 0 && (
                    <span className={clsx(css.metric, css.metricIgnored)}>
                      <span>{items.length} 条命令</span>
                    </span>
                  )}
                </>
              )}
            </div>
            <div className={css.summaryRight}>
              <span>{items.length} 条命令</span>
              <button type="button" className={css.toggleAllBtn} onClick={toggleAll}>
                {allExpanded ? '收起全部' : '展开全部'}
              </button>
            </div>
          </div>

          <div className={css.rows}>
            {items.map((item) => {
              const { entry, attachments } = item
              const open = openMap[item.key] === true
              const hasDetails = entry.exitCode !== undefined || entry.outputLines.length > 0 || attachments.length > 0
              const copyPayload = attachments.some(a => a.kind === 'diagnostic' && a.text !== undefined)
                ? [entry.raw, ...attachments.filter(a => a.kind === 'diagnostic' && a.text !== undefined).map(a => a.text)].join('\n')
                : entry.raw

              return (
                <div key={item.key} className={css.row} data-tone={entry.tone}>
                  <div className={css.rowHead}>
                    <button
                      type="button"
                      className={css.rowTrigger}
                      aria-expanded={open}
                      onClick={() => { toggleRow(item.key) }}
                    >
                      <ChevronIcon open={open} />
                      <span className={css.cmdGlyph}><TerminalMiniIcon /></span>
                      <span className={css.cmdText} title={entry.command}>{entry.command}</span>
                    </button>
                    <div className={css.rowControls}>
                      <span className={css.statusPill} data-tone={entry.tone}>
                        {entry.tone === 'success' && <CheckMiniIcon />}
                        {entry.tone === 'error' && <CrossMiniIcon />}
                        <span>{entry.statusText}</span>
                      </span>
                      <CopyIconButton text={copyPayload} copyLabel={copyLabel} copiedLabel={copiedLabel} />
                    </div>
                  </div>

                  {open && hasDetails && (
                    <div className={css.rowBody}>
                      {entry.exitCode !== undefined && (
                        <div className={css.exitLine} data-tone={entry.exitCode === 0 ? 'success' : 'error'}>
                          exit code: {entry.exitCode}
                        </div>
                      )}
                      {entry.outputLines.length > 0 && renderNumberedLines(entry.outputLines)}
                      {attachments.map((att) => (
                        att.kind === 'diagnostic' && att.text !== undefined ? (
                          <div key={att.key} className={css.attachedDiag} data-tone={att.tone ?? 'error'}>
                            <WarningMiniIcon />
                            <pre className={css.diagPre}><code>{att.text}</code></pre>
                            <CopyIconButton text={att.text} copyLabel={copyLabel} copiedLabel={copiedLabel} />
                          </div>
                        ) : att.node !== undefined ? (
                          <div key={att.key} className={css.attachedNote}>{att.node}</div>
                        ) : null
                      ))}
                    </div>
                  )}
                </div>
              )
            })}
          </div>
        </>
      )}

      {tab === 'commands' && (
        <div className={css.tabPane}>
          <div className={css.paneHeader}>
            <span>共 {items.length} 条执行命令</span>
            <button
              type="button"
              className={css.copyPill}
              onClick={() => { copyPaneText('commands', allCommandsText) }}
            >
              {copiedPane === 'commands' ? <IconCheckOutlineRegular size={13} /> : <IconCopyOutlineRegular size={13} />}
              <span>{copiedPane === 'commands' ? copiedLabel : copyLabel}</span>
            </button>
          </div>
          {renderNumberedLines(allCommandsLines)}
        </div>
      )}

      {tab === 'output' && (
        <div className={css.tabPane}>
          <div className={css.paneHeader}>
            <span>完整输出（{allOutputLines.length} 行）</span>
            <button
              type="button"
              className={css.copyPill}
              onClick={() => { copyPaneText('output', allOutputText) }}
            >
              {copiedPane === 'output' ? <IconCheckOutlineRegular size={13} /> : <IconCopyOutlineRegular size={13} />}
              <span>{copiedPane === 'output' ? copiedLabel : copyLabel}</span>
            </button>
          </div>
          {renderNumberedLines(allOutputLines)}
        </div>
      )}
    </div>
  )
}
