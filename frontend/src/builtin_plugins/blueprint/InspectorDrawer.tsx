import { useEffect, useState, type FormEvent, type ReactNode } from 'react'
import { CodeBlock } from '../../dsh/ui-primitives/markdown/CodeBlock.tsx'
import type { BlueprintApi } from './api.ts'
import { directions, entityKinds, entityLabels, portKinds, relations, severities, type Anchor, type Entity, type GraphEdge, type Impact, type Port, type Source } from './types.ts'
import css from './style.module.css'

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return <label className={css.field}><span>{label}</span>{children}</label>
}
export function Choices<T extends string>({ value, values, onChange, labels }: { value: T; values: readonly T[]; onChange: (value: T) => void; labels?: Partial<Record<T, string>> }) {
  return <select value={value} onChange={e => onChange(e.target.value as T)}>{values.map(key => <option key={key} value={key}>{labels?.[key] ?? key}</option>)}</select>
}
export function Drawer({ title, close, children }: { title: string; close: () => void; children: ReactNode }) {
  return <aside role='dialog' aria-label={title} className={css.drawer}>
    <header><strong>{title}</strong><button type='button' aria-label='关闭检查器' onClick={close}>×</button></header>
    <div className={css.drawerBody}>{children}</div>
  </aside>
}
export const newPort = (): Port => ({ id: 'port-' + crypto.randomUUID().slice(0, 8), name: '', kind: 'method', direction: 'both', symbol: null, line_start: null, line_end: null, summary: '' })
export const newEntity = (): Entity => ({ id: 'entity-' + crypto.randomUUID().slice(0, 8), name: '', kind: 'service', traits: [], file_path: null, summary: '', ports: [] })
function formAction(event: FormEvent) { event.preventDefault() }

function SourcePreview({ api, path, port, anchor }: { api: BlueprintApi; path: string; port: Port; anchor?: Anchor | undefined }) {
  const [snippet, setSnippet] = useState<Source>()
  const [error, setError] = useState('')
  const start = anchor?.suggested_line ?? port.line_start ?? 1
  const end = anchor?.suggested_line !== undefined ? start + Math.max(0, (port.line_end ?? start) - (port.line_start ?? start)) : port.line_end ?? start
  useEffect(() => {
    let active = true
    setSnippet(undefined); setError('')
    void api.source(path, start, end).then(s => { if (active) setSnippet(s) }, error => { if (active) setError(String(error.message ?? error)) })
    return () => { active = false }
  }, [api, path, start, end])
  const suffix = path.split('.').at(-1) ?? ''
  const lang = ({ rs: 'rust', ts: 'typescript', tsx: 'tsx', js: 'javascript', jsx: 'jsx', py: 'python', go: 'go', java: 'java', css: 'css', html: 'html', json: 'json', toml: 'toml', sh: 'bash', md: 'markdown', yml: 'yaml', yaml: 'yaml' } as Record<string, string>)[suffix]
  return <section className={css.source} aria-label='源码预览'>
    <h3>源码预览</h3>
    {error ? <p role='alert'>{error}</p> : snippet ? <><code title={path}>{path}:{snippet.line_start}–{snippet.line_end}</code>
      <CodeBlock code={snippet.lines.map(row => row.text).join('\n')} lang={lang} lineNumbers lineNumberStart={snippet.line_start} showHeader={false} copyLabel='复制' copiedLabel='已复制' />
    </> : <p>正在读取源码…</p>}
  </section>
}

export function EntityEditor({ initial, isNew, initialPort, addingPort, api, anchor, busy, save, remove, removeNode, onImpact }: {
  initial: Entity; isNew: boolean; initialPort?: string | undefined; addingPort?: boolean | undefined; api: BlueprintApi; anchor?: Anchor | undefined; busy: boolean;
  save: (entity: Entity) => void; remove?: (() => void) | undefined; removeNode?: (() => void) | undefined; onImpact: (port?: string) => void;
}) {
  const [entity, setEntity] = useState(() => addingPort ? { ...initial, ports: [...initial.ports, newPort()] } : initial)
  const [traits, setTraits] = useState(initial.traits.join(', '))
  const [openPort, setOpenPort] = useState(addingPort ? entity.ports.at(-1)?.id : initialPort)
  const sourcePort = initial.ports.find(p => p.id === initialPort)
  const change = (patch: Partial<Entity>) => setEntity(current => ({ ...current, ...patch }))
  const changePort = (id: string, patch: Partial<Port>) => setEntity(current => ({ ...current, ports: current.ports.map(p => p.id === id ? { ...p, ...patch } : p) }))
  return <>
    {initial.file_path && sourcePort && <SourcePreview api={api} path={initial.file_path} port={sourcePort} anchor={anchor} />}
    <form className={css.form} onSubmit={event => { formAction(event); save({ ...entity, name: entity.name.trim(), traits: traits.split(/[,，]/).map(t => t.trim()).filter(Boolean) }) }}>
      <Field label='实体名称'><input required value={entity.name} onChange={event => change({ name: event.target.value })} /></Field>
      {isNew && <Field label='实体 ID'><input required pattern='[a-zA-Z0-9_.\-]+' value={entity.id} onChange={event => change({ id: event.target.value })} /></Field>}
      <Field label='实体类型'><Choices value={entity.kind} values={entityKinds} labels={entityLabels} onChange={kind => change({ kind })} /></Field>
      <Field label='Traits'><input value={traits} onChange={e => setTraits(e.target.value)} /></Field>
      <Field label='源码路径'><input value={entity.file_path ?? ''} onChange={e => change({ file_path: e.target.value || null })} /></Field>
      <Field label='职责摘要'><textarea rows={2} value={entity.summary} onChange={e => change({ summary: e.target.value })} /></Field>
      <div className={css.row}><h3>端口</h3><button type='button' onClick={() => { const port = newPort(); change({ ports: [...entity.ports, port] }); setOpenPort(port.id) }}>+ 添加端口</button></div>
      {entity.ports.map((port, index) => <section key={port.id} className={css.portEditor}>
        <button type='button' aria-expanded={openPort === port.id} onClick={() => setOpenPort(openPort === port.id ? undefined : port.id)}>{port.name || '端口 ' + (index + 1)}</button>
        <div hidden={openPort !== port.id}>
          <Field label={'端口名称 ' + (index + 1)}><input required={openPort === port.id} value={port.name} onChange={e => changePort(port.id, { name: e.target.value })} /></Field>
          <div className={css.columns}>
            <Field label='方向'><Choices value={port.direction} values={directions} onChange={direction => changePort(port.id, { direction })} /></Field>
            <Field label='端口类型'><Choices value={port.kind} values={portKinds} onChange={kind => changePort(port.id, { kind })} /></Field>
          </div>
          <Field label='符号'><input value={port.symbol ?? ''} onChange={e => changePort(port.id, { symbol: e.target.value || null })} /></Field>
          <div className={css.columns}>
            <Field label='起始行'><input type='number' min={1} value={port.line_start ?? ''} onChange={e => changePort(port.id, { line_start: Number(e.target.value) || null })} /></Field>
            <Field label='结束行'><input type='number' min={port.line_start ?? 1} value={port.line_end ?? ''} onChange={e => changePort(port.id, { line_end: Number(e.target.value) || null })} /></Field>
          </div>
          <Field label='端口摘要'><input value={port.summary} onChange={e => changePort(port.id, { summary: e.target.value })} /></Field>
          <button type='button' onClick={() => change({ ports: entity.ports.filter(p => p.id !== port.id) })}>删除端口</button>
          {!isNew && initial.ports.some(p => p.id === port.id) && <button type='button' onClick={() => onImpact(port.id)}>端口影响面</button>}
        </div>
      </section>)}
      <button className={css.primary} disabled={busy} type='submit'>保存实体</button>
      {!isNew && <div className={css.row}>
        <button type='button' onClick={() => onImpact(initialPort)}>查看影响面</button>
        {removeNode && <button type='button' disabled={busy} onClick={removeNode}>移出画布</button>}
        {remove && <button type='button' disabled={busy} className={css.warning} onClick={remove}>删除共享实体</button>}
      </div>}
    </form>
  </>
}

export function EdgeEditor({ initial, busy, save, remove }: { initial: GraphEdge; busy: boolean; save: (edge: GraphEdge) => void; remove?: (() => void) | undefined }) {
  const [edge, setEdge] = useState(initial)
  const change = (patch: Partial<GraphEdge>) => setEdge(current => ({ ...current, ...patch }))
  return <form className={css.form} onSubmit={e => { formAction(e); save(edge) }}>
    <code className={css.endpoint}>{edge.source_entity}:{edge.source_port} → {edge.target_entity}:{edge.target_port}</code>
    <div className={css.columns}>
      <Field label='连线关系'><Choices value={edge.relation} values={relations} onChange={relation => change({ relation })} /></Field>
      <Field label='严重级别'><Choices value={edge.severity} values={severities} onChange={severity => change({ severity })} /></Field>
    </div>
    <Field label='连线标签'><input value={edge.label} onChange={e => change({ label: e.target.value })} /></Field>
    <Field label='连线红线'><textarea rows={3} value={edge.invariant ?? ''} onChange={e => change({ invariant: e.target.value || null })} /></Field>
    <button type='submit' className={css.primary} disabled={busy}>保存连线</button>
    {remove && <button type='button' className={css.warning} onClick={remove} disabled={busy}>删除连线</button>}
  </form>
}

export function ImpactPanel({ report, jump, graphTitle }: { report: Impact; jump: (id: string) => void; graphTitle: (id: string) => string }) {
  return <div className={css.form}>
    <section><h3>引用画布</h3>{report.matches.flatMap(match => match.graphs).map(id => <button type='button' key={id} onClick={() => jump(id)}>{graphTitle(id)}</button>)}</section>
    {([['上游', report.upstream], ['下游', report.downstream]] as const).map(([label, hits]) => <section key={label}><h3>{label} · {hits.length}</h3>
      {hits.map(hit => <button type='button' key={hit.graph_id + '/' + hit.edge.id} className={css.impactHit} onClick={() => jump(hit.graph_id)}>
        <code>{hit.entity_id}:{hit.port_id}</code><small>{graphTitle(hit.graph_id)} · {hit.depth} 跳</small>
      </button>)}
    </section>)}
    {report.invariants.length > 0 && <section><h3>红线不变量</h3><ul>{report.invariants.map(rule => <li key={rule.text} className={rule.severity === 'critical' ? css.warning : undefined}>{rule.text}</li>)}</ul></section>}
    {report.test_guards.length > 0 && <section><h3>测试守卫</h3><ul>{report.test_guards.map(guard => <li key={guard.id}>{guard.name} {guard.file_path && <code>{guard.file_path}</code>}</li>)}</ul></section>}
  </div>
}

