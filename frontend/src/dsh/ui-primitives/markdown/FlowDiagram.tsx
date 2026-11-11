import { useMemo, useState } from 'react'
import { BaseEdge, EdgeLabelRenderer, getSmoothStepPath, MarkerType, Position, ReactFlow, type Edge, type EdgeProps, type Node } from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import { FLOW_NODE_WIDTH, layoutFlowData, type FlowData } from './flow-data.ts'
import css from './FlowDiagram.module.css'

function FlowEdge({id, sourceX, sourceY, targetX, targetY, markerEnd, style, label, data}: EdgeProps) {
  // Only an edge that skips a layer goes round the side, clear of the boxes between.
  const long = (data as {skips?: boolean} | undefined)?.skips === true
  const side = (data as {side?: number} | undefined)?.side ?? Math.max(sourceX, targetX) + 230
  const [normalPath, normalX, normalY] = getSmoothStepPath({sourceX, sourceY, targetX, targetY, sourcePosition:Position.Bottom, targetPosition:Position.Top, borderRadius:0, offset:10})
  const path = long ? `M ${sourceX} ${sourceY} L ${sourceX} ${sourceY + 15} L ${side} ${sourceY + 15} L ${side} ${targetY - 15} L ${targetX} ${targetY - 15} L ${targetX} ${targetY}` : normalPath
  return <><BaseEdge id={id} path={path} {...(markerEnd ? {markerEnd} : {})} {...(style ? {style} : {})} />{label && <EdgeLabelRenderer><span className={css.edgeLabel} style={{transform:`translate(-50%, -50%) translate(${long ? side : normalX}px,${long ? (sourceY+targetY)/2 : normalY}px)`}}>{label}</span></EdgeLabelRenderer>}</>
}
const edgeTypes = { flow: FlowEdge }

/** A diagram describes the answer's subject, not the host's execution frames. */
export function FlowDiagram({data, source}: {data: FlowData; source: string}) {
  const [showSource, setShowSource] = useState(false)
  const graph = useMemo(() => {
    const positions=layoutFlowData(data)
    const nodes: Node[] = data.nodes.map(node => ({id:node.id, position:positions.get(node.id)!, data:{label:node.label}, sourcePosition:Position.Bottom, targetPosition:Position.Top, style:{width:FLOW_NODE_WIDTH,whiteSpace:'pre-wrap',lineHeight:'20px'}}))
    // A skipping edge passes right of every box.
    const right = Math.max(...[...positions.values()].map(position => position.x)) + FLOW_NODE_WIDTH + 30
    const edges: Edge[] = data.edges.map((edge,index) => ({id:`e${index}`,source:edge.source,target:edge.target,label:edge.label,type:'flow',data:{skips:Math.abs(positions.get(edge.target)!.level-positions.get(edge.source)!.level)!==1, side:right},markerEnd:{type:MarkerType.ArrowClosed, color:'#43a783'},style:{stroke:'#43a783',strokeWidth:2}}))
    const graphHeight = Math.max(...[...positions.values()].map(position => position.y + position.height))
    return {nodes,edges,graphHeight}
  },[data])
  const graphHeight = graph.graphHeight
  return <figure className={css.figure} aria-label="流程示意图">
    <div className={css.header}><span>流程示意图</span><button type="button" onClick={() => setShowSource(value=>!value)}>{showSource ? '隐藏数据' : '查看数据'}</button></div>
    <div className={css.canvas} style={{height:Math.min(700, Math.max(260, graphHeight + 60))}}>
      <ReactFlow nodes={graph.nodes} edges={graph.edges} edgeTypes={edgeTypes} fitView fitViewOptions={{padding:0.12,maxZoom:1}} nodesDraggable={false} nodesConnectable={false} elementsSelectable={false} panOnScroll zoomOnScroll={false} proOptions={{hideAttribution:true}} />
    </div>
    {showSource && <pre className={css.source}><code>{source}</code></pre>}
  </figure>
}
