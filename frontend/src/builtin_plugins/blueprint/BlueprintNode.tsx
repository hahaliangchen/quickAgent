import { memo, useLayoutEffect } from 'react'
import { Handle, Position, useUpdateNodeInternals, type Node, type NodeProps } from '@xyflow/react'
import { entityLabels, kindColors, type Entity, type EntityStatus, type Port, type PropertyKind } from './types.ts'
import css from './style.module.css'

export type Card = Node<{ entity: Entity; status?: EntityStatus | undefined; focusedPort?: string | undefined; references: number; inspect: (entity: string, port?: string) => void; addPort: (entity: string) => void; impact: (entity: string) => void }, 'blueprint'>
const badges: Record<Port['kind'], string> = { event: 'EVT', method: 'FN', route: 'API', tool: 'TOOL', query: 'DB', mutation: 'DB', state: 'STATE', guard: 'GUARD' }
const propBadges: Record<PropertyKind, string> = { file: 'FILE', symbol: 'SYM', config: 'CFG', env: 'ENV', rule: 'RULE' }

export const BlueprintNode = memo(function BlueprintNode({ id, data, selected }: NodeProps<Card>) {
  const { entity, status, references } = data
  const updateInternals = useUpdateNodeInternals()
  const props = entity.properties ?? []
  const portShape = props.length + '|' + entity.ports.map(p => p.id + ':' + p.direction).join(',')
  useLayoutEffect(() => { updateInternals(id) }, [id, portShape, updateInternals])

  return <article className={css.card} data-selected={selected || undefined} data-testid={'node-' + id}>
    <header className={css.nodeHead} style={{ borderTopColor: kindColors[entity.kind] }}>
      <span aria-hidden='true' style={{ color: kindColors[entity.kind] }}>◆</span>
      <strong title={entityLabels[entity.kind] + ' · ' + entity.name}>{entity.name}</strong>
      {entity.traits.length > 0 && <span className={css.traits} title={entity.traits.join(' · ')}>{entity.traits.slice(0, 2).join(' · ')}</span>}
      {status?.file_modified && <i className={css.statusDot} data-status='drifted' title='关联源码自上次同步后有改动' />}
      <button type='button' className={'nodrag ' + css.refCount} title='查看跨画布引用' aria-label={'跨画布引用 ' + references} onClick={event => { event.stopPropagation(); data.impact(id) }}>{references}</button>
    </header>
    <div className={css.nodeSub}>
      {entity.file_path && <code title={entity.file_path}>{entity.file_path}</code>}
      <span title={entity.summary}>{entity.summary || entityLabels[entity.kind]}</span>
    </div>
    {props.length > 0 && (
      <div className={css.propsSection} aria-label='类属性锚点'>
        {props.map(prop => {
          const propAnchor = status?.properties?.[prop.id]
          return <button
            key={prop.id}
            type='button'
            className={'nodrag ' + css.propRow}
            title={`${prop.name}: ${prop.value}${prop.summary ? '\n' + prop.summary : ''}`}
            onClick={event => { event.stopPropagation(); data.inspect(id) }}
          >
            <small className={css.propTag} data-kind={prop.kind}>{propBadges[prop.kind]}</small>
            <span className={css.propName}>{prop.name}</span>
            <code className={css.propVal}>{prop.value}</code>
            {propAnchor && propAnchor.status !== 'ok' && <i className={css.statusDot} data-status={propAnchor.status} title={propAnchor.status} />}
          </button>
        })}
      </div>
    )}
    <div className={css.ports}>{entity.ports.map(port => {
      const anchor = status?.ports[port.id]
      const readsHint = port.reads?.length ? ` · 读 ${port.reads.join(', ')}` : ''
      return <div key={port.id} className={css.port} data-active={data.focusedPort === port.id || undefined}>
        {port.direction !== 'out' && <Handle className={css.handle} type='target' id={'in:' + port.id} position={Position.Left} />}
        <button type='button' className={'nodrag ' + css.portButton} onClick={event => { event.stopPropagation(); data.inspect(id, port.id) }} title={(port.summary || port.name) + readsHint}>
          <small>{badges[port.kind]}</small>
          <span>{port.name}</span>
          {port.symbol && <code>{port.symbol}</code>}
          <i className={css.statusDot} data-status={anchor?.status ?? 'ok'} aria-label={anchor?.status ?? 'ok'} title={anchor?.status ?? 'ok'} />
        </button>
        {port.direction !== 'in' && <Handle className={css.handle} type='source' id={'out:' + port.id} position={Position.Right} />}
      </div>
    })}</div>
    <button type='button' className={'nodrag ' + css.addPort} onClick={event => { event.stopPropagation(); data.addPort(id) }}>+ 添加方法端口</button>
  </article>
})
