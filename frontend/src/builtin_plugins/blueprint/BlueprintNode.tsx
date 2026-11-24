import { memo, useLayoutEffect } from 'react'
import { Handle, Position, useUpdateNodeInternals, type Node, type NodeProps } from '@xyflow/react'
import { entityLabels, kindColors, type Entity, type EntityStatus, type Port } from './types.ts'
import css from './style.module.css'

export type Card = Node<{ entity: Entity; status?: EntityStatus | undefined; focusedPort?: string | undefined; references: number; inspect: (entity: string, port?: string) => void; addPort: (entity: string) => void; impact: (entity: string) => void }, 'blueprint'>
const badges: Record<Port['kind'], string> = { event: 'EVT', method: 'FN', route: 'API', tool: 'TOOL', query: 'DB', mutation: 'DB', state: 'STATE', guard: 'GUARD' }
export const BlueprintNode = memo(function BlueprintNode({ id, data, selected }: NodeProps<Card>) {
  const { entity, status, references } = data
  const updateInternals = useUpdateNodeInternals()
  const portShape = entity.ports.map(p => p.id + ':' + p.direction).join(',')
  useLayoutEffect(() => { updateInternals(id) }, [id, portShape, updateInternals])
  return <article className={css.card} data-selected={selected || undefined} data-testid={'node-' + id}>
    <header className={css.nodeHead} style={{ borderTopColor: kindColors[entity.kind] }}>
      <span aria-hidden='true' style={{ color: kindColors[entity.kind] }}>◆</span><strong title={entityLabels[entity.kind] + ' · ' + entity.name}>{entity.name}</strong>
      {entity.traits.length > 0 && <span className={css.traits} title={entity.traits.join(' · ')}>{entity.traits.slice(0, 2).join(' · ')}</span>}
      <button type='button' className={'nodrag ' + css.refCount} title='查看跨画布引用' aria-label={'跨画布引用 ' + references} onClick={event => { event.stopPropagation(); data.impact(id) }}>{references}</button>
    </header>
    <div className={css.nodeSub}><code title={entity.file_path ?? ''}>{entity.file_path || '无源码路径'}</code><span title={entity.summary}>{entity.summary || entityLabels[entity.kind]}</span></div>
    <div className={css.ports}>{entity.ports.map(port => {
      const anchor = status?.ports[port.id]
      return <div key={port.id} className={css.port} data-active={data.focusedPort === port.id || undefined}>
        {port.direction !== 'out' && <Handle className={css.handle} type='target' id={'in:' + port.id} position={Position.Left} />}
        <button type='button' className={'nodrag ' + css.portButton} onClick={event => { event.stopPropagation(); data.inspect(id, port.id) }} title={port.summary || port.name}>
          <small>{badges[port.kind]}</small><span>{port.name}</span>{port.symbol && <code>{port.symbol}</code>}
          <i className={css.statusDot} data-status={anchor?.status ?? 'ok'} aria-label={anchor?.status ?? 'ok'} title={anchor?.status ?? 'ok'} />
        </button>
        {port.direction !== 'in' && <Handle className={css.handle} type='source' id={'out:' + port.id} position={Position.Right} />}
      </div>
    })}</div>
    <button type='button' className={'nodrag ' + css.addPort} onClick={event => { event.stopPropagation(); data.addPort(id) }}>+ 添加端口</button>
  </article>
})
