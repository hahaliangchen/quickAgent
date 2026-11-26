import { useState } from 'react'
import { CodeBlock } from '../../dsh/ui-primitives/markdown/CodeBlock.tsx'
import { Field } from './InspectorDrawer.tsx'
import { buildRunPlaybookPrompt, buildSyncPlaybookPrompt } from './playbookPrompts.ts'
import type { GraphSummary, Playbook, PlaybookStatus, PlaybookUpdate, ProjectKind } from './types.ts'
import { projectKinds } from './types.ts'
import css from './style.module.css'

const statusLabel: Record<PlaybookStatus['status'], string> = {
  ok: '锚点有效',
  drifted: '锚点已变更',
  missing: '锚点缺失',
}

function splitLines(raw: string): string[] {
  return raw.split('\n').map(line => line.trim()).filter(Boolean)
}

export function PlaybookViewPanel({
  playbook,
  status,
  graphs,
  busy,
  onRunAi,
  onSyncAi,
  onJumpGraph,
  onOpenSession,
  onEdit,
  onDelete,
}: {
  playbook: Playbook
  status?: PlaybookStatus | undefined
  graphs: readonly GraphSummary[]
  busy: boolean
  onRunAi?: ((prompt: string) => void) | undefined
  onSyncAi?: ((prompt: string) => void) | undefined
  onJumpGraph: (graphId: string) => void
  onOpenSession?: ((taskId: string) => void) | undefined
  onEdit: () => void
  onDelete: () => void
}) {
  const effectiveStatus = status?.status ?? 'ok'
  const missingSet = new Set(status?.missing_files ?? [])
  const changedSet = new Set(status?.changed_files ?? [])
  const graphMap = new Map(graphs.map(g => [g.id, g.title]))

  return (
    <div className={css.playbookPage} data-testid="blueprint-playbook-view">
      <header className={css.playbookHeader}>
        <div className={css.playbookTitleBlock}>
          <div className={css.playbookBadges}>
            <span className={css.playbookKindBadge}>最短路径 · Blueprint-as-Code</span>
            <span className={css.playbookStatusBadge} data-status={effectiveStatus}>
              {statusLabel[effectiveStatus]}
            </span>
            <code className={css.playbookId}>{playbook.id}</code>
          </div>
          <h1>{playbook.title}</h1>
          {playbook.trigger && <p className={css.playbookTrigger}>适用场景：{playbook.trigger}</p>}
          {playbook.summary && <p className={css.playbookSummary}>{playbook.summary}</p>}
        </div>
        <div className={css.playbookActions}>
          <button
            type="button"
            className={css.primary}
            disabled={!onRunAi}
            onClick={() => onRunAi?.(buildRunPlaybookPrompt(playbook, status))}
          >
            按此最短路径执行
          </button>
          <button
            type="button"
            className={css.subtleButton}
            disabled={!onSyncAi}
            onClick={() => onSyncAi?.(buildSyncPlaybookPrompt(playbook, status))}
          >
            校验并刷新
          </button>
          <button type="button" className={css.subtleButton} disabled={busy} onClick={onEdit}>
            编辑
          </button>
          <button type="button" className={`${css.subtleButton} ${css.warning}`} disabled={busy} onClick={onDelete}>
            删除
          </button>
        </div>
      </header>

      {effectiveStatus !== 'ok' && (
        <div className={css.playbookAlert} data-status={effectiveStatus}>
          <strong>
            {effectiveStatus === 'missing'
              ? '部分锚点文件已不存在，建议刷新最短路径'
              : '关联源码自上次同步后有改动，执行前请先核实变动'}
          </strong>
          <span>
            {[...(status?.missing_files ?? []).map(f => `缺失：${f}`), ...(status?.changed_files ?? []).map(f => `已改：${f}`)].join(' · ')}
          </span>
        </div>
      )}

      <section className={css.playbookSection}>
        <div className={css.playbookSectionHead}>
          <h2>提炼后的编排蓝图代码</h2>
          <span>剔除历史弯路，仅保留最小可执行主干</span>
        </div>
        {playbook.code.trim() ? (
          <div className={css.playbookCodeWrap}>
            <CodeBlock
              code={playbook.code}
              lang="javascript"
              lineNumbers
              showHeader
              copyLabel="复制路径代码"
              copiedLabel="已复制"
            />
          </div>
        ) : (
          <p className={css.playbookEmptyHint}>暂无编排代码，点击右上角「编辑」或让 AI 从历史会话提炼。</p>
        )}
      </section>

      <div className={css.playbookGrid}>
        <section className={css.playbookCard}>
          <h2>锚点文件（漂移监测）</h2>
          {playbook.anchor_files.length ? (
            <div className={css.chipList}>
              {playbook.anchor_files.map(file => {
                const state = missingSet.has(file) ? 'missing' : changedSet.has(file) ? 'drifted' : 'ok'
                return (
                  <span key={file} className={css.anchorChip} data-status={state} title={file}>
                    <i className={css.statusDot} data-status={state} />
                    <code>{file}</code>
                  </span>
                )
              })}
            </div>
          ) : (
            <p className={css.playbookEmptyHint}>尚未绑定锚点文件</p>
          )}
        </section>

        <section className={css.playbookCard}>
          <h2>关联架构画布 & 溯源会话</h2>
          {playbook.related_graphs.length > 0 && (
            <div className={css.chipList}>
              {playbook.related_graphs.map(gid => (
                <button
                  key={gid}
                  type="button"
                  className={css.linkChip}
                  onClick={() => onJumpGraph(gid)}
                >
                  画布：{graphMap.get(gid) ?? gid} →
                </button>
              ))}
            </div>
          )}
          {playbook.related_entities.length > 0 && (
            <div className={css.chipList}>
              {playbook.related_entities.map(eid => (
                <span key={eid} className={css.metaChip}>
                  实体：{eid}
                </span>
              ))}
            </div>
          )}
          {playbook.source_tasks.length > 0 && (
            <div className={css.chipList}>
              {playbook.source_tasks.map(tid =>
                onOpenSession ? (
                  <button
                    key={tid}
                    type="button"
                    className={css.linkChip}
                    onClick={() => onOpenSession(tid)}
                    title="打开来源历史会话"
                  >
                    会话：{tid.slice(0, 12)} ↗
                  </button>
                ) : (
                  <span key={tid} className={css.metaChip}>
                    会话：{tid}
                  </span>
                ),
              )}
            </div>
          )}
          {!playbook.related_graphs.length && !playbook.related_entities.length && !playbook.source_tasks.length && (
            <p className={css.playbookEmptyHint}>暂无关联画布或来源会话</p>
          )}
        </section>

        <section className={css.playbookCard}>
          <h2>环境约束与关键路径（自动挂入上下文尾部）</h2>
          {playbook.environment?.length ? (
            <ul className={css.playbookList}>
              {playbook.environment.map((item, idx) => (
                <li key={idx}>{item}</li>
              ))}
            </ul>
          ) : (
            <p className={css.playbookEmptyHint}>暂无环境记录（端口、venv/解释器路径、隔离边界等将自动挂在模型上下文尾部提醒）</p>
          )}
        </section>

        <section className={css.playbookCard}>
          <h2>避坑提示与红线（自动挂入上下文尾部）</h2>
          {playbook.pitfalls.length ? (
            <ul className={css.playbookList}>
              {playbook.pitfalls.map((item, idx) => (
                <li key={idx}>{item}</li>
              ))}
            </ul>
          ) : (
            <p className={css.playbookEmptyHint}>暂无记录</p>
          )}
        </section>

        <section className={css.playbookCard}>
          <h2>最短验收命令</h2>
          {playbook.verify_commands.length ? (
            <ul className={css.playbookCommandList}>
              {playbook.verify_commands.map((cmd, idx) => (
                <li key={idx}>
                  <code>{cmd}</code>
                </li>
              ))}
            </ul>
          ) : (
            <p className={css.playbookEmptyHint}>暂无验收命令</p>
          )}
        </section>
      </div>
    </div>
  )
}

export function PlaybookForm({
  playbook,
  project,
  busy,
  save,
  remove,
}: {
  playbook?: Playbook | undefined
  project?: ProjectKind | undefined
  busy: boolean
  save: (id: string, payload: PlaybookUpdate) => void
  remove?: (() => void) | undefined
}) {
  const [id, setId] = useState(playbook?.id ?? 'path-' + crypto.randomUUID().slice(0, 8))
  const [title, setTitle] = useState(playbook?.title ?? '')
  const [trigger, setTrigger] = useState(playbook?.trigger ?? '')
  const [summary, setSummary] = useState(playbook?.summary ?? '')
  const [code, setCode] = useState(playbook?.code ?? '')
  const [anchors, setAnchors] = useState(playbook?.anchor_files.join('\n') ?? '')
  const [graphs, setGraphs] = useState(playbook?.related_graphs.join('\n') ?? '')
  const [environment, setEnvironment] = useState(playbook?.environment?.join('\n') ?? '')
  const [pitfalls, setPitfalls] = useState(playbook?.pitfalls.join('\n') ?? '')
  const [commands, setCommands] = useState(playbook?.verify_commands.join('\n') ?? '')
  const [projectKind, setProjectKind] = useState<ProjectKind>(project ?? 'fullstack')

  return (
    <form
      className={css.form}
      onSubmit={e => {
        e.preventDefault()
        const cleanId = id.trim()
        if (!cleanId) return
        save(cleanId, {
          id: cleanId,
          title: title.trim() || cleanId,
          trigger: trigger.trim(),
          summary: summary.trim(),
          code,
          anchor_files: splitLines(anchors),
          related_graphs: splitLines(graphs),
          environment: splitLines(environment),
          pitfalls: splitLines(pitfalls),
          verify_commands: splitLines(commands),
          ...(playbook || project ? {} : { project_kind: projectKind }),
        })
      }}
    >
      {!playbook && (
        <Field label="路径 ID（字母/数字/-/_）">
          <input required pattern="[a-zA-Z0-9_\-]+" value={id} onChange={e => setId(e.target.value)} />
        </Field>
      )}
      <Field label="名称">
        <input autoFocus required value={title} onChange={e => setTitle(e.target.value)} placeholder="如：桌面端构建与验收最短路径" />
      </Field>
      {!playbook && !project && (
        <Field label="项目形态">
          <select value={projectKind} onChange={e => setProjectKind(e.target.value as ProjectKind)}>
            {projectKinds.map(k => <option key={k} value={k}>{k}</option>)}
          </select>
        </Field>
      )}
      <Field label="适用触发场景">
        <input value={trigger} onChange={e => setTrigger(e.target.value)} placeholder="什么时候应该直接复用这条路径" />
      </Field>
      <Field label="摘要">
        <textarea rows={2} value={summary} onChange={e => setSummary(e.target.value)} />
      </Field>
      <Field label="最短路径编排代码（JS / 伪代码）">
        <textarea rows={8} className={css.monoInput} value={code} onChange={e => setCode(e.target.value)} placeholder="// 1. 读取关键配置&#10;// 2. 修改核心文件&#10;// 3. 运行定向验收" />
      </Field>
      <Field label="锚点文件（每行一个工作区相对路径）">
        <textarea rows={3} className={css.monoInput} value={anchors} onChange={e => setAnchors(e.target.value)} placeholder="src/server/routes.rs&#10;frontend/src/App.tsx" />
      </Field>
      <Field label="关联画布 ID（每行一个）">
        <textarea rows={2} className={css.monoInput} value={graphs} onChange={e => setGraphs(e.target.value)} />
      </Field>
      <Field label="环境约束与关键路径（每行一条，自动挂在 AI 上下文尾部）">
        <textarea rows={3} value={environment} onChange={e => setEnvironment(e.target.value)} placeholder="如：201 验证端口 18133 必须用 /opt/anydoc.../venv/bin/python (3.12)" />
      </Field>
      <Field label="避坑提示 / 红线（每行一条，自动挂在 AI 上下文尾部）">
        <textarea rows={3} value={pitfalls} onChange={e => setPitfalls(e.target.value)} />
      </Field>
      <Field label="最短验收命令（每行一条）">
        <textarea rows={2} className={css.monoInput} value={commands} onChange={e => setCommands(e.target.value)} />
      </Field>
      <button type="submit" className={css.primary} disabled={busy}>
        {playbook ? '保存最短路径' : '创建最短路径'}
      </button>
      {remove && (
        <button type="button" className={css.warning} disabled={busy} onClick={remove}>
          删除最短路径
        </button>
      )}
    </form>
  )
}
